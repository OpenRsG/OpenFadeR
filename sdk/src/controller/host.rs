//! What the host offers a controller plugin: the functions of this module call the [`Host`] table of the call being served on this
//! thread. Outside a call (or on another thread) they do nothing and answer NaN / 0 / `false` / empty. See [`Host`] for each one.

use super::{Host, Str};
use std::cell::Cell;

thread_local! {
    static CURRENT: Cell<*const Host> = const { Cell::new(std::ptr::null()) };
}

/// Runs `f` with `host` as the table of this thread's call.
#[doc(hidden)]
pub fn serving<R>(host: *const Host, f: impl FnOnce() -> R) -> R {
    struct Reset(*const Host);
    impl Drop for Reset {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(CURRENT.with(|c| c.replace(host)));
    f()
}

fn table<R>(f: impl FnOnce(&Host) -> R, none: R) -> R {
    // SAFETY: the pointer is the table of the call being served, valid until it returns.
    CURRENT
        .with(|c| unsafe { c.get().as_ref() })
        .map_or(none, f)
}

/// The control `key`, NaN when there is none.
pub fn get(key: &str) -> f64 {
    // SAFETY: the host's functions take the text for the duration of the call.
    table(|h| unsafe { (h.get)(h.ctx, Str::new(key)) }, f64::NAN)
}

/// Sets the control `key`; `false` when there is none.
pub fn set(key: &str, value: f64) -> bool {
    table(|h| unsafe { (h.set)(h.ctx, Str::new(key), value) }, false)
}

/// Sends one MIDI message to the controller.
pub fn midi_out(msg: &[u8]) {
    table(
        |h| unsafe { (h.midi_out)(h.ctx, msg.as_ptr(), msg.len()) },
        (),
    );
}

/// A timer ([`super::Controller::timer`] gets the id): once after `ms`, or every `ms`. `None` when refused.
pub fn timer(ms: u32, repeat: bool) -> Option<u64> {
    Some(table(|h| unsafe { (h.timer)(h.ctx, ms, repeat) }, 0)).filter(|&id| id != 0)
}

pub fn cancel(id: u64) {
    table(|h| unsafe { (h.cancel)(h.ctx, id) }, ());
}

/// Starts or stops [`super::Controller::changed`] for `key`.
pub fn watch(key: &str, on: bool) {
    table(|h| unsafe { (h.watch)(h.ctx, Str::new(key), on) }, ());
}

/// Soft takeover of the absolute control `key` ([`Host::takeover`]).
pub fn takeover(key: &str, on: bool) {
    table(|h| unsafe { (h.takeover)(h.ctx, Str::new(key), on) }, ());
}

/// The controller's name for the host's window.
pub fn label(text: &str) {
    table(|h| unsafe { (h.label)(h.ctx, Str::new(text)) }, ());
}

pub fn log(text: &str) {
    table(|h| unsafe { (h.log)(h.ctx, Str::new(text)) }, ());
}

/// The plugin's settings from `plugin.json`, one JSON object (`{}` outside a call).
pub fn settings() -> String {
    // SAFETY: the text is read before the next host call.
    table(
        |h| unsafe { (h.settings)(h.ctx).as_str() }.to_owned(),
        "{}".to_owned(),
    )
}
