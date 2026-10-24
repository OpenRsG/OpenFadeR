//! What the host offers a plugin while it works: every plugin call gets a [`Host`] table, and a plugin asks it for the things
//! only the host has (its analysis of a track, the audio's sample rate) and tells it its progress. Same shape as the plugin's
//! own table: one JSON request in, one JSON answer out, the buffer freed with the table's `free`.
//!
//! | `op` | other fields | answer |
//! |---|---|---|
//! | `analysis` | `track`, `computed` | `{"analysis": `[`Analysis`]` or null}`: `computed` false: what the track's own library holds (its `analysis_ref`); true: the host's analysis of the audio (grid, waveform, key; cached) |
//! | `audio_info` | `file` | `{"sample_rate": Hz}`, 0 when unknown |
//! | `progress` | `phase`, `done`, `total`, `bytes_done`, `bytes_total` | `{"cancel": bool}`: the user cancelled the job, stop at the next safe point. The byte counts (0 when the plugin does not count bytes) let the host show the speed and the time left |
//!
//! The functions of this module call the table of the request being served on this thread; outside a request (or on another
//! thread) they answer nothing.

use super::{Analysis, Buf, Track};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::Cell;
use std::path::Path;

/// The host's side of the ABI, passed to every `call`; valid until that call returns.
#[repr(C)]
pub struct Host {
    pub call: unsafe extern "C" fn(req: *const u8, len: usize) -> Buf,
    pub free: unsafe extern "C" fn(buf: Buf),
}

/// A request of a plugin to the host, `{"op": "<snake_case variant>", …}` in JSON.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    Analysis {
        track: Track,
        computed: bool,
    },
    AudioInfo {
        file: std::path::PathBuf,
    },
    Progress {
        phase: String,
        done: usize,
        total: usize,
        #[serde(default)]
        bytes_done: u64,
        #[serde(default)]
        bytes_total: u64,
    },
}

/// The job's last progress report and byte counts, so a byte count can be sent without the plugin naming its phase again.
#[derive(Default)]
struct Job {
    phase: String,
    done: usize,
    total: usize,
    bytes_done: u64,
    bytes_total: u64,
    sent: Option<std::time::Instant>,
}

thread_local! {
    static CURRENT: Cell<*const Host> = const { Cell::new(std::ptr::null()) };
    static JOB: std::cell::RefCell<Job> = std::cell::RefCell::default();
}

/// Runs `f` with `host` as the table of this thread's requests.
#[doc(hidden)]
pub fn serving<R>(host: *const Host, f: impl FnOnce() -> R) -> R {
    struct Reset(*const Host);
    impl Drop for Reset {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(CURRENT.with(|c| c.replace(host)));
    JOB.with(|j| *j.borrow_mut() = Job::default());
    f()
}

fn ask(req: &HostRequest) -> Option<Value> {
    let host = CURRENT.with(Cell::get);
    // SAFETY: `host` is the table of the call being served (see `serving`), valid until it returns; the answer is read and
    // freed once.
    let host = unsafe { host.as_ref()? };
    let body = serde_json::to_vec(req).ok()?;
    unsafe {
        let buf = (host.call)(body.as_ptr(), body.len());
        let v = serde_json::from_slice::<Value>(std::slice::from_raw_parts(buf.ptr, buf.len)).ok();
        (host.free)(buf);
        v
    }
}

/// The analysis of `track` ([`HostRequest::Analysis`]).
pub fn analysis(track: &Track, computed: bool) -> Option<Analysis> {
    let v = ask(&HostRequest::Analysis {
        track: track.clone(),
        computed,
    })?;
    serde_json::from_value(v.get("analysis")?.clone()).ok()
}

/// Sample rate of the audio file, 0 when the host cannot tell.
pub fn sample_rate(file: &Path) -> u32 {
    ask(&HostRequest::AudioInfo {
        file: file.to_owned(),
    })
    .and_then(|v| v.get("sample_rate")?.as_u64())
    .map_or(0, |n| n as u32)
}

/// Tells the host the job is at `done` of `total` in `phase` ("Copying tracks"); `true` when the user cancelled it.
pub fn progress(phase: &str, done: usize, total: usize) -> bool {
    JOB.with(|j| {
        let mut j = j.borrow_mut();
        phase.clone_into(&mut j.phase);
        (j.done, j.total) = (done, total);
    });
    send()
}

/// Sets how many bytes the whole job will write (audio plus the estimate of the rest), and none written yet.
pub fn set_bytes_total(total: u64) {
    JOB.with(|j| {
        let mut j = j.borrow_mut();
        (j.bytes_done, j.bytes_total) = (0, total);
    });
}

/// Counts `n` more bytes written; reports to the host at most every 200 ms. `true` when the user cancelled the job.
pub fn add_bytes(n: u64) -> bool {
    let due = JOB.with(|j| {
        let mut j = j.borrow_mut();
        j.bytes_done += n;
        j.sent
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(200))
    });
    due && send()
}

fn send() -> bool {
    let req = JOB.with(|j| {
        let mut j = j.borrow_mut();
        j.sent = Some(std::time::Instant::now());
        HostRequest::Progress {
            phase: j.phase.clone(),
            done: j.done,
            total: j.total,
            bytes_done: j.bytes_done,
            bytes_total: j.bytes_total,
        }
    });
    ask(&req)
        .and_then(|v| v.get("cancel")?.as_bool())
        .unwrap_or(false)
}
