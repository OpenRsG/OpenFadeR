//! What the host answers a library plugin while it works (`openfader_plugin::library::host`): its analysis of a track, the
//! sample rate of an audio file and the progress of the job. The callbacks are plain `extern "C"` functions without user
//! data, so the job being served is a thread-local set around the plugin call ([`with_job`]); outside a job `progress`
//! answers "not cancelled".

use super::refs::load_ref;
use crate::plugin::device::Control;
use openfader_plugin::library::Buf;
use openfader_plugin::library::host::{Host, HostRequest};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

thread_local! {
    static JOB: RefCell<Option<Arc<Control>>> = const { RefCell::new(None) };
}

/// The table every plugin call gets.
pub static HOST: Host = Host { call, free };

/// Runs `f` (a plugin call) as the job `ctl` on this thread: the plugin's `progress` reports to `ctl` and learns of its cancel.
pub fn with_job<R>(ctl: &Arc<Control>, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<Arc<Control>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            JOB.with(|j| *j.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(JOB.with(|j| j.borrow_mut().replace(ctl.clone())));
    f()
}

fn answer(req: &[u8]) -> Value {
    match serde_json::from_slice(req) {
        Err(e) => json!({ "error": format!("bad request: {e}") }),
        Ok(HostRequest::Analysis { track, computed }) => {
            let a = if computed {
                track
                    .file
                    .as_deref()
                    .and_then(|f| crate::analysis::cache::get_or_analyze(f).ok())
                    .map(|(a, _)| a)
            } else {
                load_ref(&track)
            };
            json!({ "analysis": a })
        }
        Ok(HostRequest::AudioInfo { file }) => {
            json!({ "sample_rate": crate::audio::probe_rate(&file).unwrap_or(0) })
        }
        Ok(HostRequest::Progress { phase, done, total }) => {
            let cancel = JOB.with(|j| {
                j.borrow().as_ref().is_some_and(|c| {
                    c.set_phase(&phase);
                    c.total.store(total, std::sync::atomic::Ordering::Relaxed);
                    c.done.store(done, std::sync::atomic::Ordering::Relaxed);
                    c.cancelled()
                })
            });
            json!({ "cancel": cancel })
        }
    }
}

/// `Host::call`.
///
/// # Safety
/// `req` points to `len` readable bytes.
unsafe extern "C" fn call(req: *const u8, len: usize) -> Buf {
    // SAFETY: the plugin hands a valid buffer (see above).
    let req = unsafe { std::slice::from_raw_parts(req, len) };
    let v = catch_unwind(AssertUnwindSafe(|| answer(req)))
        .unwrap_or_else(|_| json!({ "error": "the host panicked" }));
    let bytes = serde_json::to_vec(&v)
        .unwrap_or_default()
        .into_boxed_slice();
    let len = bytes.len();
    Buf {
        ptr: Box::into_raw(bytes).cast(),
        len,
    }
}

/// `Host::free`.
///
/// # Safety
/// `buf` came from [`call`] and is not used again.
unsafe extern "C" fn free(buf: Buf) {
    // SAFETY: `call` made the buffer from a boxed slice of exactly this length.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.ptr, buf.len)) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    fn ask(v: Value) -> Value {
        let body = v.to_string();
        // SAFETY: the table's own functions, the buffer is freed once after reading.
        unsafe {
            let buf = (HOST.call)(body.as_ptr(), body.len());
            let v = serde_json::from_slice(std::slice::from_raw_parts(buf.ptr, buf.len)).unwrap();
            (HOST.free)(buf);
            v
        }
    }

    #[test]
    fn progress_reaches_the_job_and_brings_its_cancel_back() {
        let progress = json!({"op": "progress", "phase": "Copying", "done": 2, "total": 5});
        assert_eq!(ask(progress.clone()), json!({"cancel": false})); // no job
        let ctl = Arc::new(Control::default());
        with_job(&ctl, || {
            assert_eq!(ask(progress.clone()), json!({"cancel": false}));
            ctl.cancel();
            assert_eq!(ask(progress.clone()), json!({"cancel": true}));
        });
        assert_eq!(
            (ctl.phase(), ctl.done.load(Relaxed), ctl.total.load(Relaxed)),
            ("Copying".to_owned(), 2, 5)
        );
        assert_eq!(ask(progress), json!({"cancel": false})); // the job ended with the call
    }

    #[test]
    fn unknown_requests_are_errors_and_a_missing_file_has_no_rate() {
        assert!(ask(json!({"op": "nope"}))["error"].is_string());
        assert_eq!(
            ask(json!({"op": "audio_info", "file": "/no/such/file"})),
            json!({"sample_rate": 0})
        );
    }
}
