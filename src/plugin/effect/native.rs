//! Effects from a native library: the host side of the C ABI of the `openfader-plugin` crate (`sdk/`). [`open`] loads a
//! library, checks the entry point and ABI version, validates every table, slot and name, and keeps the library loaded
//! for the rest of the process (the effects call into it). [`Fns::stage`] makes one [`Stage`] per FX unit.

use super::{Effect, Imp, NAME_LEN, PROBE_RATE, valid_name};
use crate::audio::fx::{Ctx, Frame, Stage};
use libloading::Library;
use openfader_plugin::{ABI, ENTRY, EffectVTable, PluginInfo, Slot};
use std::ffi::{CStr, c_void};
use std::path::Path;

type Create = unsafe extern "C" fn(u32) -> *mut c_void;
type Tick = unsafe extern "C" fn(*mut c_void, *mut Frame, *const Ctx);
type Call = unsafe extern "C" fn(*mut c_void);

/// The functions of one validated table. They stay valid because the library is never unloaded.
#[derive(Clone, Copy, Debug)]
pub(super) struct Fns {
    create: Create,
    tick: Tick,
    reset: Option<Call>,
    engage: Option<Call>,
    destroy: Call,
}

impl Fns {
    /// A new instance for one FX unit. If the library's `create` fails now (it worked at load) the stage passes the
    /// signal through.
    pub(super) fn stage(&self, rate: u32) -> Box<dyn Stage> {
        // SAFETY: `create` comes from a validated table.
        let p = unsafe { (self.create)(rate) };
        if p.is_null() {
            return Box::new(super::Bypass);
        }
        Box::new(Instance { f: *self, p })
    }
}

/// An instance the library created; `destroy` runs on drop.
struct Instance {
    f: Fns,
    p: *mut c_void,
}

// SAFETY: the ABI lets instances move between threads (never called from two at once).
unsafe impl Send for Instance {}

impl Stage for Instance {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let mut frame = x;
        // SAFETY: `p` is a live instance of this table; the library never unwinds out of `tick`.
        unsafe { (self.f.tick)(self.p, &mut frame, c) };
        frame
    }

    fn reset(&mut self) {
        if let Some(f) = self.f.reset {
            unsafe { f(self.p) };
        }
    }

    fn engage(&mut self) {
        if let Some(f) = self.f.engage {
            unsafe { f(self.p) };
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { (self.f.destroy)(self.p) };
    }
}

/// The effects of the library at `path`.
pub(super) fn open(path: &Path, plugin: &str) -> Result<Vec<Effect>, String> {
    // SAFETY: loading runs the library's initialisers. A plugin is native code the user installed, like any program.
    let lib = unsafe { Library::new(path) }
        .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    let effects = {
        type Entry = unsafe extern "C" fn() -> *const PluginInfo;
        let entry = unsafe { lib.get::<Entry>(ENTRY) }.map_err(|e| {
            format!(
                "{} has no `openfader_plugin_v1` entry point ({e}); it was not built against ABI {ABI} of openfader-plugin",
                path.display()
            )
        })?;
        unsafe { read(entry(), plugin) }?
    };
    // The effects call into the library for as long as the process lives.
    std::mem::forget(lib);
    Ok(effects)
}

/// Every effect of the table `info` points to. Checks the ABI version before reading anything else.
///
/// # Safety
/// `info` is null or points to a [`PluginInfo`] whose `effects` and each table's `name` are valid.
pub(super) unsafe fn read(info: *const PluginInfo, plugin: &str) -> Result<Vec<Effect>, String> {
    let info = unsafe { info.as_ref() }.ok_or("the entry point returned null")?;
    if info.abi != ABI {
        return Err(format!(
            "built for plugin ABI {}, this build speaks {ABI}",
            info.abi
        ));
    }
    if info.effects.is_null() || info.count == 0 {
        return Err("exports no effects".into());
    }
    let tables = unsafe { std::slice::from_raw_parts(info.effects, info.count as usize) };
    tables
        .iter()
        .map(|t| unsafe { from_vtable(plugin, t) })
        .collect()
}

/// One validated effect: a good name, every required function present, and a `create` that works.
///
/// # Safety
/// `vt.name` is null or NUL terminated, and the functions follow the ABI.
pub(super) unsafe fn from_vtable(plugin: &str, vt: &EffectVTable) -> Result<Effect, String> {
    if vt.name.is_null() {
        return Err("an effect has no name".into());
    }
    let name = unsafe { CStr::from_ptr(vt.name) }
        .to_str()
        .map_err(|_| "an effect name is not UTF-8")?;
    if !valid_name(name) {
        return Err(format!(
            "effect name {name:?} must be 1 to {NAME_LEN} characters of ASCII letters, digits, space, - or _"
        ));
    }
    let slot = Slot::from_raw(vt.slot).ok_or_else(|| {
        format!(
            "effect {name:?}: unknown slot {}, this build knows 0 to {}",
            vt.slot,
            Slot::ALL.len() - 1
        )
    })?;
    let (Some(create), Some(tick), Some(destroy)) = (vt.create, vt.tick, vt.destroy) else {
        return Err(format!(
            "effect {name:?}: create, tick and destroy are required"
        ));
    };
    let probe = unsafe { create(PROBE_RATE) };
    if probe.is_null() {
        return Err(format!("effect {name:?}: create failed"));
    }
    unsafe { destroy(probe) };
    let f = Fns {
        create,
        tick,
        reset: vt.reset,
        engage: vt.engage,
        destroy,
    };
    Ok(Effect {
        name: name.into(),
        plugin: plugin.into(),
        slot,
        imp: Imp::Native(f),
    })
}
