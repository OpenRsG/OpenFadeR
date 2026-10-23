//! Controller plugins: a native library that drives a MIDI controller for the host. ABI version [`CONTROLLER_ABI`].
//!
//! The host owns the MIDI ports. For every input port it asks each controller plugin to [`ControllerPlugin::probe`] it (a score,
//! 0 = not mine); the best one, or the one the user assigned to the port in PREFERENCES > CONTROLLER, gets it
//! ([`ControllerPlugin::open`]) and from then on the port bypasses the host's own mapping files. The plugin receives every
//! message of the port ([`Controller::midi`], sysex included) and answers through the host's callbacks ([`host`]): it reads and
//! sets the host's controls (`docs/plugins.md`, Controller plugins: the control table), sends MIDI to the controller's output port
//! (lights), starts timers, and is told when a control it watches changes (the lights follow).
//!
//! Threading: the host makes **every call of a session from one thread, one at a time**, and never from the audio thread; it is
//! its UI thread, so a call must return quickly. The callbacks are valid only inside such a call (a plugin keeps no thread of its
//! own that uses them). A callback never re-enters the plugin: a control the plugin sets is reported back by a later
//! [`Controller::changed`], not from inside `set`. Nothing here needs `Send`.
//!
//! The library exports one C function, [`CONTROLLER_ENTRY`], returning a static [`ControllerApi`]. In Rust implement
//! [`ControllerPlugin`] and [`Controller`] and call [`export_controller!`]: it builds the table and keeps panics from crossing the
//! C boundary (a panic in a call is logged and the call ends; keep the default `panic = "unwind"`).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod host;

/// ABI version of the controller plugin interface.
pub const CONTROLLER_ABI: u32 = 1;

/// Name of the entry point a controller plugin exports (NUL terminated, for `dlsym`).
pub const CONTROLLER_ENTRY: &[u8] = b"openfader_controller_v1\0";

/// UTF-8 text that crosses the ABI: `len` bytes at `ptr`, no terminator, valid for the call.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Str {
    pub ptr: *const u8,
    pub len: usize,
}

impl Str {
    pub const fn new(s: &str) -> Str {
        Str {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }

    /// The text; invalid UTF-8 reads as empty.
    ///
    /// # Safety
    /// `ptr` points to `len` readable bytes that stay valid as long as the result is used.
    pub unsafe fn as_str<'a>(self) -> &'a str {
        if self.ptr.is_null() {
            return "";
        }
        std::str::from_utf8(unsafe { std::slice::from_raw_parts(self.ptr, self.len) }).unwrap_or("")
    }
}

/// The host's side of the ABI, passed to every call of the plugin; valid until that call returns. `ctx` is the host's own and
/// goes back as the first argument of each function. During `probe` no session exists: only `settings` and `log` work, the
/// others do nothing and answer NaN / 0 / false.
#[repr(C)]
pub struct Host {
    pub ctx: *mut c_void,
    /// Value of the control `key` (the control table of `docs/plugins.md`); NaN when there is no such control.
    pub get: unsafe extern "C" fn(ctx: *mut c_void, key: Str) -> f64,
    /// Sets the control `key`; `false` when there is no such control. A button takes the value as pressed (0.5 and up) or released,
    /// a knob 0 to 1 (subject to soft takeover, see `takeover`), an encoder or jog its signed ticks.
    pub set: unsafe extern "C" fn(ctx: *mut c_void, key: Str, value: f64) -> bool,
    /// Sends `len` bytes (one complete MIDI message, sysex as F0 … F7) to the controller's output port, in turn with the other
    /// messages (the host spaces them so a controller can keep up).
    pub midi_out: unsafe extern "C" fn(ctx: *mut c_void, msg: *const u8, len: usize),
    /// Starts a timer: [`Controller::timer`] gets the returned id (1 and up; 0 = refused) after `ms` (once, or every `ms` with
    /// `repeat`). The host's resolution is 5 ms.
    pub timer: unsafe extern "C" fn(ctx: *mut c_void, ms: u32, repeat: bool) -> u64,
    pub cancel: unsafe extern "C" fn(ctx: *mut c_void, id: u64),
    /// Starts / stops telling the plugin when control `key` changes ([`Controller::changed`], at most every 10 ms, with the
    /// value now; once on start so the lights begin right).
    pub watch: unsafe extern "C" fn(ctx: *mut c_void, key: Str, on: bool),
    /// Soft takeover of an absolute control the plugin sets: while on, a value that is far from the one on screen waits until it
    /// crosses it (PREFERENCES > CONTROLLER > Others > Soft takeover has the last word).
    pub takeover: unsafe extern "C" fn(ctx: *mut c_void, key: Str, on: bool),
    /// The name the host shows for the controller (the CONTROLLER line of the MIDI window).
    pub label: unsafe extern "C" fn(ctx: *mut c_void, text: Str),
    /// A line for the host's log (stderr, `plugin: <name>: <text>`); the last one of a failed `open` is shown to the user.
    pub log: unsafe extern "C" fn(ctx: *mut c_void, text: Str),
    /// The plugin's own settings from `plugin.json` as one JSON object (id → value); valid until the next call of this function.
    pub settings: unsafe extern "C" fn(ctx: *mut c_void) -> Str,
}

/// What the entry point returns: a static table. Every function gets the [`Host`] of the call; `session` is what `open` returned.
#[repr(C)]
pub struct ControllerApi {
    /// [`CONTROLLER_ABI`]; the host reads nothing else before checking it.
    pub abi: u32,
    /// Does this plugin drive the input port called `port`? 0 = no, 1 to 100 = how sure (the best score wins).
    pub probe: unsafe extern "C" fn(port: Str, host: *const Host) -> u32,
    /// Claims `port`: a session handle (1 and up), 0 = refused (say why with `log`). `forced`: the user assigned this plugin to
    /// the port, so it should drive it even when `probe` would have said no, if it can.
    pub open: unsafe extern "C" fn(port: Str, forced: bool, host: *const Host) -> u64,
    /// One incoming message of the port: a complete MIDI message as the port delivered it (note-on with velocity 0 is not
    /// rewritten; clock and active sensing are dropped).
    pub midi: unsafe extern "C" fn(session: u64, msg: *const u8, len: usize, host: *const Host),
    /// A watched control changed.
    pub changed: unsafe extern "C" fn(session: u64, key: Str, value: f64, host: *const Host),
    /// A timer of the session is due.
    pub timer: unsafe extern "C" fn(session: u64, id: u64, host: *const Host),
    /// The session ends (the port went away, the user assigned another driver, or the app quits): last chance to send MIDI (lights
    /// off); the host closes the port after it, the session handle is dead.
    pub close: unsafe extern "C" fn(session: u64, host: *const Host),
}

// SAFETY: an immutable table of function pointers.
unsafe impl Sync for ControllerApi {}

/// A controller plugin. [`export_controller!`] turns a type implementing it into the C ABI.
pub trait ControllerPlugin {
    /// Does this plugin drive `port` (the input port's name as the system gives it)? 0 = no, 1 to 100 = how sure. Called for
    /// every port at every connect: keep it cheap (read what it needs once).
    fn probe(port: &str) -> u32;

    /// Starts driving `port`. `forced`: see [`ControllerApi::open`]. An `Err` is shown to the user and the port goes back to the
    /// host's own mapping.
    fn open(port: &str, forced: bool) -> Result<Box<dyn Controller>, String>;
}

/// One claimed port.
pub trait Controller {
    /// One incoming message.
    fn midi(&mut self, msg: &[u8]);
    /// A control passed to [`host::watch`] changed.
    fn changed(&mut self, _key: &str, _value: f64) {}
    /// A timer from [`host::timer`] is due.
    fn timer(&mut self, _id: u64) {}
    /// The session ends; send the lights-off messages here.
    fn close(&mut self) {}
}

type Session = Rc<RefCell<Box<dyn Controller>>>;

thread_local! {
    static SESSIONS: RefCell<BTreeMap<u64, Session>> = const { RefCell::new(BTreeMap::new()) };
}
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Runs `f` with `host` as this thread's host table; a panic is logged and ends the call.
fn serving(host: *const Host, what: &str, f: impl FnOnce()) {
    let r = host::serving(host, || catch_unwind(AssertUnwindSafe(f)));
    if r.is_err() {
        host::serving(host, || host::log(&format!("the plugin panicked in {what}")));
    }
}

fn session(id: u64) -> Option<Session> {
    SESSIONS.with_borrow(|s| s.get(&id).cloned())
}

/// The table of `P`, for [`export_controller!`] (and for a host's tests, which call it without a library).
#[doc(hidden)]
pub const fn api<P: ControllerPlugin>() -> ControllerApi {
    unsafe extern "C" fn probe<P: ControllerPlugin>(port: Str, host: *const Host) -> u32 {
        let score = Cell::new(0);
        // SAFETY: the host passes valid text and a valid table.
        serving(host, "probe", || score.set(P::probe(unsafe { port.as_str() })));
        score.get().min(100)
    }
    unsafe extern "C" fn open<P: ControllerPlugin>(
        port: Str,
        forced: bool,
        host: *const Host,
    ) -> u64 {
        let id = Cell::new(0);
        serving(host, "open", || {
            match P::open(unsafe { port.as_str() }, forced) {
                Ok(c) => {
                    let n = NEXT.fetch_add(1, Ordering::Relaxed);
                    SESSIONS.with_borrow_mut(|s| s.insert(n, Rc::new(RefCell::new(c))));
                    id.set(n);
                }
                Err(e) => host::log(&e),
            }
        });
        id.get()
    }
    unsafe extern "C" fn midi<P: ControllerPlugin>(
        id: u64,
        msg: *const u8,
        len: usize,
        host: *const Host,
    ) {
        let msg: &[u8] = if msg.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(msg, len) }
        };
        if let Some(s) = session(id) {
            serving(host, "midi", || s.borrow_mut().midi(msg));
        }
    }
    unsafe extern "C" fn changed<P: ControllerPlugin>(
        id: u64,
        key: Str,
        value: f64,
        host: *const Host,
    ) {
        if let Some(s) = session(id) {
            serving(host, "changed", || {
                s.borrow_mut().changed(unsafe { key.as_str() }, value)
            });
        }
    }
    unsafe extern "C" fn timer<P: ControllerPlugin>(id: u64, timer: u64, host: *const Host) {
        if let Some(s) = session(id) {
            serving(host, "timer", || s.borrow_mut().timer(timer));
        }
    }
    unsafe extern "C" fn close<P: ControllerPlugin>(id: u64, host: *const Host) {
        let Some(s) = SESSIONS.with_borrow_mut(|s| s.remove(&id)) else {
            return;
        };
        serving(host, "close", || s.borrow_mut().close());
    }
    ControllerApi {
        abi: CONTROLLER_ABI,
        probe: probe::<P>,
        open: open::<P>,
        midi: midi::<P>,
        changed: changed::<P>,
        timer: timer::<P>,
        close: close::<P>,
    }
}

/// Exports a [`ControllerPlugin`] from a `cdylib`: defines the `openfader_controller_v1` entry point. Use it once per library.
///
/// ```ignore
/// openfader_plugin::export_controller!(MyDriver);
/// ```
#[macro_export]
macro_rules! export_controller {
    ($p:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn openfader_controller_v1() -> *const $crate::controller::ControllerApi {
            static API: $crate::controller::ControllerApi = $crate::controller::api::<$p>();
            &API
        }
    };
}

#[cfg(test)]
mod tests;
