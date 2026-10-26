//! Shared helpers of the device-ops tests: temp dirs, synthetic audio and snapshots, a fake host (`host::*` callbacks of a job)
//! and the real-device lookups (`OPENFADER_USB`, the git-ignored `.local/onelibrary-key`; machine paths stay out of the repository).
#![allow(dead_code)] // each test binary uses a subset

use openfader_plugin::library::{
    Analysis, Beat, Buf, Cue, ExportOptions, ExportReport, Playlist, Snapshot, Track,
    host::{self, Host, HostRequest},
};
use serde_json::{Value, json};
use std::cell::Cell;
use std::path::{Path, PathBuf};

/// The root of a real device library named by `OPENFADER_USB`; `None` (skip) otherwise.
pub fn usb_root() -> Option<PathBuf> {
    let r = PathBuf::from(std::env::var_os("OPENFADER_USB").filter(|v| !v.is_empty())?);
    let ok = r.join("PIONEER/rekordbox").is_dir();
    if !ok {
        eprintln!("skip: OPENFADER_USB is not a device library");
    }
    ok.then_some(r)
}

/// The key of the real OneLibrary: the git-ignored `.local/onelibrary-key` of the repository.
pub fn onelibrary_key() -> Option<String> {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../.local/onelibrary-key"
    ))
    .ok()
    .map(|k| k.trim().to_owned())
    .filter(|k| !k.is_empty())
}

/// An empty per-process temp directory `rk-{tag}-{pid}`, recreated on every call.
pub fn fresh_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `n` beats of a 120 BPM 4/4 grid starting at 250 ms.
pub fn grid(n: u32) -> Vec<Beat> {
    (0..n)
        .map(|i| Beat {
            bar_pos: (i % 4) as u16 + 1,
            bpm_x100: 12000,
            time_ms: 250 + i * 500,
        })
        .collect()
}

pub fn cue(hot: u16, ms: u32) -> Cue {
    Cue {
        hot_cue: hot,
        time_ms: ms,
        loop_end_ms: None,
        color: Some([1, 2, 3]),
        comment: None,
    }
}

/// Writes a `secs`-long 44.1 kHz stereo 16-bit WAV with a 2 ms 1 kHz click on every beat at `bpm` (first click at 0.173 s).
pub fn click_wav(path: &Path, bpm: f64, secs: f64) {
    const RATE: usize = 44100;
    let mut pcm = vec![0i16; (secs * RATE as f64) as usize * 2];
    let mut t = 0.173;
    while t < secs - 0.05 {
        let at = (t * RATE as f64) as usize;
        for i in 0..90 {
            let v = (0.8
                * (-(i as f32) / 30.0).exp()
                * (std::f32::consts::TAU * 1000.0 * i as f32 / RATE as f32).sin()
                * 32767.0) as i16;
            pcm[(at + i) * 2] = v;
            pcm[(at + i) * 2 + 1] = v;
        }
        t += 60.0 / bpm;
    }
    let data: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut w = [
        b"RIFF".as_slice(),
        &(36 + data.len() as u32).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
    ]
    .concat();
    w.extend_from_slice(&[1, 0, 2, 0]); // PCM, 2 channels
    w.extend_from_slice(&(RATE as u32).to_le_bytes());
    w.extend_from_slice(&(RATE as u32 * 4).to_le_bytes());
    w.extend_from_slice(&[4, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    std::fs::write(path, w).unwrap();
}

/// `(id, name, depth, folder, member track indexes)`
pub type List = (&'static str, &'static str, usize, bool, Vec<usize>);

/// The snapshot of `tracks` and `lists`, each track with `analysis(track)`.
pub fn snapshot(
    tracks: &[Track],
    lists: &[List],
    analysis: impl Fn(&Track) -> Option<Analysis>,
) -> Snapshot {
    Snapshot {
        tracks: tracks.iter().map(|t| (t.clone(), analysis(t))).collect(),
        playlists: lists
            .iter()
            .map(|l| Playlist {
                id: l.0.into(),
                name: l.1.into(),
                depth: l.2,
                is_folder: l.3,
            })
            .collect(),
        members: lists.iter().map(|l| l.4.clone()).collect(),
        ..Snapshot::default()
    }
}

/// `snap` reduced to the playlists with these ids and the tracks they list (in order of first use).
pub fn narrowed(snap: &Snapshot, ids: &[&str]) -> Snapshot {
    let keep: Vec<usize> = (0..snap.playlists.len())
        .filter(|&i| ids.contains(&snap.playlists[i].id.as_str()))
        .collect();
    let mut order: Vec<usize> = Vec::new();
    for &p in &keep {
        for &m in &snap.members[p] {
            if !order.contains(&m) {
                order.push(m);
            }
        }
    }
    Snapshot {
        tracks: order.iter().map(|&i| snap.tracks[i].clone()).collect(),
        playlists: keep.iter().map(|&p| snap.playlists[p].clone()).collect(),
        members: keep
            .iter()
            .map(|&p| {
                snap.members[p]
                    .iter()
                    .map(|m| order.iter().position(|o| o == m).unwrap())
                    .collect()
            })
            .collect(),
        ..Snapshot::default()
    }
}

thread_local! {
    static CANCEL: Cell<bool> = const { Cell::new(false) };
    static CANCEL_AT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static SEEN: std::cell::RefCell<Vec<(String, u64, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The host's analysis of the audio ("computed"): a 120 BPM grid, a 3-band waveform and a key.
fn computed() -> Analysis {
    let band = |n: usize| (0..n).map(|i| [(i % 100) as u8, 40, 20]).collect();
    Analysis {
        beats: grid(24),
        three_band_detail: band(300),
        three_band_preview: band(openfader_plugin::library::PREVIEW_ENTRIES),
        key: Some("8A".into()),
        ..Analysis::default()
    }
}

unsafe extern "C" fn call(req: *const u8, len: usize) -> Buf {
    // SAFETY: `req`/`len` are the plugin's request buffer (see `host::ask`).
    let req: HostRequest =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(req, len) }).unwrap();
    let v: Value = match req {
        HostRequest::Analysis { computed: c, .. } => json!({ "analysis": c.then(computed) }),
        HostRequest::AudioInfo { .. } => json!({ "sample_rate": 44100 }),
        HostRequest::Progress {
            phase,
            bytes_done,
            bytes_total,
            ..
        } => {
            let cancel = CANCEL.get() || CANCEL_AT.with(|c| c.borrow().as_deref() == Some(&phase));
            SEEN.with(|s| s.borrow_mut().push((phase, bytes_done, bytes_total)));
            json!({ "cancel": cancel })
        }
    };
    let out = serde_json::to_vec(&v).unwrap().into_boxed_slice();
    let len = out.len();
    Buf {
        ptr: Box::into_raw(out).cast::<u8>(),
        len,
    }
}

unsafe extern "C" fn free(buf: Buf) {
    // SAFETY: `buf` is an answer of `call`, freed once.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.ptr, buf.len)) });
}

/// Runs `f` as a job served by the fake host: it has no library analysis, computes `computed()`, and says "cancel" once
/// [`cancel`] was called on this thread.
pub fn with_host<R>(f: impl FnOnce() -> R) -> R {
    CANCEL.set(false);
    CANCEL_AT.with(|c| *c.borrow_mut() = None);
    SEEN.with(|s| s.borrow_mut().clear());
    host::serving(&Host { call, free }, f)
}

/// The user cancels the job: every progress report of this thread answers so.
pub fn cancel() {
    CANCEL.set(true);
}

/// The user cancels the job when it reports `phase`.
pub fn cancel_at(phase: &str) {
    CANCEL_AT.with(|c| *c.borrow_mut() = Some(phase.to_owned()));
}

/// `(phase, bytes_done, bytes_total)` of every progress report of the last job on this thread.
pub fn reports() -> Vec<(String, u64, u64)> {
    SEEN.with(|s| s.borrow().clone())
}

/// The plugin's settings holding `key` as the OneLibrary key (`None`: no setting, so the built-in key).
pub fn settings(key: Option<&str>) -> Value {
    key.map_or(Value::Null, |k| json!({ "onelibrary_key": k }))
}

/// `compat_rekordbox_export::export` under the fake host.
pub fn export(
    root: &Path,
    snap: &Snapshot,
    options: &ExportOptions,
    key: Option<&str>,
) -> Result<ExportReport, String> {
    with_host(|| compat_rekordbox_export::export(root, snap, options, &settings(key)))
}

/// Panics unless the host would show `text` in every language of the plugin's `locales/` (the rule of the host's `locale.rs`: the
/// text itself is a key, or matches a key whose `{}` stand for values of at least one character).
pub fn assert_translated(text: &str) {
    fn matches(key: &str, text: &str) -> bool {
        let parts: Vec<&str> = key.split("{}").collect();
        let (first, rest) = parts.split_first().unwrap();
        let Some((last, middle)) = rest.split_last() else {
            return false;
        };
        let Some(mut s) = text.strip_prefix(first).and_then(|t| t.strip_suffix(last)) else {
            return false;
        };
        for part in middle {
            let start = s.char_indices().nth(1).map_or(s.len(), |(i, _)| i);
            match s.get(start..).and_then(|r| r.find(part)) {
                Some(i) => s = &s[start + i + part.len()..],
                None => return false,
            }
        }
        !s.is_empty()
    }
    for lang in ["de", "es", "fr", "it", "nl", "pt"] {
        let file = format!(
            "{}/../device/locales/{lang}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let map: std::collections::HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let found = map.get(text).is_some_and(|t| !t.is_empty())
            || map
                .iter()
                .any(|(k, v)| !v.is_empty() && k.contains("{}") && matches(k, text));
        assert!(found, "{lang} has no translation of: {text}");
    }
}
