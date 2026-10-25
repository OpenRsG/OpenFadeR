//! Controller plugins, host side: the C ABI of `openfader_plugin::controller` (docs in `docs/plugins.md`). A plugin declares
//! `"controller": {"lib": …}` ([`ControllerDecl`]); [`load`] opens each library once at start (one that does not load, has no entry
//! point or speaks another ABI is reported and skipped) and keeps it loaded for the rest of the process. The app asks every
//! [`enabled`] one to [`Loaded::probe`] an input port, [`Loaded::open`]s the best and calls the resulting [`Session`] with each
//! message, change and timer.
//!
//! The plugin's callbacks (the control namespace, MIDI out, timers…) are the [`Backend`] the caller passes to every call: this
//! module only carries them across the ABI, so it stays free of the UI; `app/midi/claim` implements them. Every call of a session
//! is made from the caller's thread, one at a time, as the SDK documents.

use super::{config, library::LibFile, Plugin};
use libloading::Library as Dylib;
use openfader_plugin::controller::{ControllerApi, Host, Str, CONTROLLER_ABI, CONTROLLER_ENTRY};
use serde::Deserialize;
use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[cfg(test)]
mod tests;

/// `"controller"` of `plugin.json`.
#[derive(Deserialize, Clone, Debug)]
pub struct ControllerDecl {
    /// The native library, relative to the plugin folder: a file name, or one per OS (`linux`, `windows`, `macos`).
    pub lib: LibFile,
}

/// What the host offers one plugin call (the functions of `openfader_plugin::controller::Host`, see the SDK for each).
pub trait Backend {
    /// The control `key`; NaN when there is none.
    fn get(&self, key: &str) -> f64;
    fn set(&self, key: &str, value: f64) -> bool;
    fn midi_out(&self, msg: &[u8]);
    /// A timer id (1 and up), 0 = refused.
    fn timer(&self, ms: u32, repeat: bool) -> u64;
    fn cancel(&self, id: u64);
    fn watch(&self, key: &str, on: bool);
    fn takeover(&self, key: &str, on: bool);
    fn label(&self, text: &str);
    /// A line the plugin logged (already on stderr).
    fn log(&self, _text: &str) {}
}

/// The host of a `probe`: no session yet, so no control, output or timer.
struct Probing;

impl Backend for Probing {
    fn get(&self, _: &str) -> f64 {
        f64::NAN
    }
    fn set(&self, _: &str, _: f64) -> bool {
        false
    }
    fn midi_out(&self, _: &[u8]) {}
    fn timer(&self, _: u32, _: bool) -> u64 {
        0
    }
    fn cancel(&self, _: u64) {}
    fn watch(&self, _: &str, _: bool) {}
    fn takeover(&self, _: &str, _: bool) {}
    fn label(&self, _: &str) {}
}

/// One loaded library: the plugin and its entry table (valid for the rest of the process).
#[derive(Clone)]
pub struct Loaded {
    pub plugin: Plugin,
    api: &'static ControllerApi,
}

/// The plugin's view of one call: the caller's [`Backend`], plus what the host keeps for the call.
struct Ctx<'a> {
    lib: &'a Loaded,
    be: &'a dyn Backend,
    /// The plugin's settings as JSON, made when it first asks.
    settings: RefCell<String>,
    /// The last line the plugin logged: why an `open` failed.
    last: RefCell<String>,
}

/// Runs a callback; a panic of the backend must not unwind into the plugin's C frames.
fn guard<R>(none: R, f: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(none)
}

fn ctx<'a>(p: *mut c_void) -> &'a Ctx<'a> {
    // SAFETY: `ctx` of the table is the `Ctx` of the call being served, which outlives it.
    unsafe { &*p.cast::<Ctx>() }
}

/// # Safety
/// `s` is text valid for the call.
unsafe fn text<'a>(s: Str) -> &'a str {
    unsafe { s.as_str() }
}

unsafe extern "C" fn get(c: *mut c_void, key: Str) -> f64 {
    guard(f64::NAN, || ctx(c).be.get(unsafe { text(key) }))
}
unsafe extern "C" fn set(c: *mut c_void, key: Str, v: f64) -> bool {
    guard(false, || ctx(c).be.set(unsafe { text(key) }, v))
}
unsafe extern "C" fn midi_out(c: *mut c_void, msg: *const u8, len: usize) {
    if !msg.is_null() {
        // SAFETY: the plugin passes `len` readable bytes.
        let m = unsafe { std::slice::from_raw_parts(msg, len) };
        guard((), || ctx(c).be.midi_out(m));
    }
}
unsafe extern "C" fn timer(c: *mut c_void, ms: u32, repeat: bool) -> u64 {
    guard(0, || ctx(c).be.timer(ms, repeat))
}
unsafe extern "C" fn cancel(c: *mut c_void, id: u64) {
    guard((), || ctx(c).be.cancel(id));
}
unsafe extern "C" fn watch(c: *mut c_void, key: Str, on: bool) {
    guard((), || ctx(c).be.watch(unsafe { text(key) }, on));
}
unsafe extern "C" fn takeover(c: *mut c_void, key: Str, on: bool) {
    guard((), || ctx(c).be.takeover(unsafe { text(key) }, on));
}
unsafe extern "C" fn label(c: *mut c_void, t: Str) {
    guard((), || ctx(c).be.label(unsafe { text(t) }));
}
unsafe extern "C" fn settings(c: *mut c_void) -> Str {
    guard(Str::new("{}"), || {
        let cx = ctx(c);
        let mut buf = cx.settings.borrow_mut();
        // The new text replaces the last answer, which the SDK documents.
        *buf = serde_json::to_string(&config::get().values(&cx.lib.plugin.manifest))
            .unwrap_or_else(|_| "{}".to_owned());
        Str::new(&buf)
    })
}
unsafe extern "C" fn log(c: *mut c_void, t: Str) {
    guard((), || {
        let (cx, t) = (ctx(c), unsafe { text(t) });
        eprintln!("plugin: {}: {t}", cx.lib.name());
        *cx.last.borrow_mut() = t.to_owned();
        cx.be.log(t);
    });
}

impl Loaded {
    pub fn name(&self) -> &str {
        &self.plugin.manifest.name
    }

    /// Runs `f` with the host table of one call served by `be`.
    fn serve<R>(&self, be: &dyn Backend, f: impl FnOnce(*const Host) -> R) -> (R, String) {
        let cx = Ctx {
            lib: self,
            be,
            settings: RefCell::default(),
            last: RefCell::default(),
        };
        let host = Host {
            ctx: (&cx as *const Ctx).cast_mut().cast(),
            get,
            set,
            midi_out,
            timer,
            cancel,
            watch,
            takeover,
            label,
            log,
            settings,
        };
        let r = f(&host);
        (r, cx.last.take())
    }

    /// How sure the plugin is that it drives the input port `port`: 0 = not, 1 to 100.
    pub fn probe(&self, port: &str) -> u32 {
        // SAFETY: the table comes from a checked entry; the text and the host table outlive the call.
        self.serve(&Probing, |h| unsafe { (self.api.probe)(Str::new(port), h) })
            .0
            .min(100)
    }

    /// Claims `port` for `be`. `forced`: the user assigned this plugin to the port. The error is the plugin's last log line.
    pub fn open(&self, port: &str, forced: bool, be: &dyn Backend) -> Result<Session, String> {
        let (id, why) = self.serve(be, |h| unsafe {
            (self.api.open)(Str::new(port), forced, h)
        });
        if id == 0 {
            return Err(if why.is_empty() {
                format!("{}: refused the port", self.name())
            } else {
                why
            });
        }
        Ok(Session {
            lib: self.clone(),
            id,
        })
    }
}

/// A port a plugin drives. End it with [`Session::close`].
pub struct Session {
    lib: Loaded,
    id: u64,
}

impl Session {
    pub fn plugin(&self) -> &str {
        self.lib.name()
    }

    /// One incoming message.
    pub fn midi(&self, msg: &[u8], be: &dyn Backend) {
        let l = &self.lib;
        l.serve(be, |h| unsafe {
            (l.api.midi)(self.id, msg.as_ptr(), msg.len(), h)
        });
    }

    /// A watched control is now `value`.
    pub fn changed(&self, key: &str, value: f64, be: &dyn Backend) {
        let l = &self.lib;
        l.serve(be, |h| unsafe {
            (l.api.changed)(self.id, Str::new(key), value, h)
        });
    }

    pub fn timer(&self, id: u64, be: &dyn Backend) {
        let l = &self.lib;
        l.serve(be, |h| unsafe { (l.api.timer)(self.id, id, h) });
    }

    /// Ends the session; the plugin may still send MIDI through `be`.
    pub fn close(self, be: &dyn Backend) {
        let l = &self.lib;
        l.serve(be, |h| unsafe { (l.api.close)(self.id, h) });
    }
}

/// The loaded controller plugins that are still switched on, in registry order.
pub fn enabled() -> impl Iterator<Item = &'static Loaded> {
    let now = super::current();
    super::registry()
        .controllers
        .iter()
        .filter(move |l| now.get(l.name()).is_some())
}

/// Every controller library of `plugins`, loaded; what goes wrong is added to `errors` (the plugin is skipped).
pub fn load(plugins: &[Plugin], errors: &mut Vec<String>) -> Vec<Loaded> {
    let mut out = Vec::new();
    for p in plugins {
        let Some(d) = &p.manifest.controller else {
            continue;
        };
        let file = p.dir.join(d.lib.file().unwrap_or_default());
        match open_api(&file) {
            Ok(api) => out.push(Loaded {
                plugin: p.clone(),
                api,
            }),
            Err(e) => errors.push(format!("{}: {e}", p.manifest.name)),
        }
    }
    out
}

fn open_api(path: &std::path::Path) -> Result<&'static ControllerApi, String> {
    // SAFETY: loading runs the library's initialisers. A plugin is native code the user installed, like any program.
    let lib =
        unsafe { Dylib::new(path) }.map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    let api = {
        type Entry = unsafe extern "C" fn() -> *const ControllerApi;
        let entry = unsafe { lib.get::<Entry>(CONTROLLER_ENTRY) }.map_err(|e| {
            format!(
                "{} has no `openfader_controller_v1` entry point ({e}); it was not built against controller ABI {CONTROLLER_ABI} of openfader-plugin",
                path.display()
            )
        })?;
        unsafe { (*entry)().as_ref() }.ok_or("the entry point returned null")?
    };
    if api.abi != CONTROLLER_ABI {
        return Err(format!(
            "built for controller ABI {}, this build speaks {CONTROLLER_ABI}",
            api.abi
        ));
    }
    // The table and the functions live in the library, which stays loaded for the rest of the process.
    std::mem::forget(lib);
    Ok(api)
}

/// A [`Loaded`] over a table linked into the process (`openfader_plugin::controller::api`): how tests drive a plugin without a
/// library. It reads no setting, so the plugin's settings are `{}`.
pub fn linked(name: &str, api: &'static ControllerApi) -> Loaded {
    Loaded {
        plugin: Plugin {
            dir: Default::default(),
            manifest: serde_json::from_value(serde_json::json!({"name": name})).unwrap(),
            builtin: None,
        },
        api,
    }
}
