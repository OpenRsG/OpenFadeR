//! SDK for OpenFadeR effect plugins: effects in a native shared library that the host loads and runs on the audio
//! thread. The library exports one C function, [`ENTRY`], which returns a [`PluginInfo`] listing its effects as
//! [`EffectVTable`]s. `openfader_plugin.h` is the same ABI for C.
//!
//! Each effect names its [`Slot`]: the place in the mixer it runs (Beat FX unit, COLOR FX knob, mic, send / return,
//! lever, channel fader). The slot decides what the input frame is and what [`Ctx::knob`] means; see [`Slot`].
//!
//! In Rust, implement [`Effect`] and list the types in [`export_effects!`]; the macro builds the tables and the
//! entry point, and keeps panics from unwinding into the host. Build the crate as a `cdylib` with the default
//! `panic = "unwind"`.
//!
//! The ABI is versioned twice: the symbol name carries the version (a host that does not know the version finds no
//! symbol) and [`PluginInfo::abi`] is checked before anything else is read.
//!
//! Audio thread rules (every call except `create` and `destroy`): no allocation, no locks, no I/O, no blocking.
//! Instances may be created on one thread and run on another ([`Effect`] is `Send`); one instance is never called
//! from two threads at once.
//!
//! [`dsp`] holds the host's own building blocks (delay line, LFO, filters, pitch shifter, reverb) for effects to reuse.
//!
//! Track libraries (a file or folder format the host reads, and writes for the collection export) are a second kind of
//! plugin, behind the `library` feature: see [`library`].
//!
//! Controller plugins (a native library that drives a MIDI controller port for the host) are a third kind: see [`controller`].

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// ABI version of this crate and of `openfader_plugin.h`.
pub const ABI: u32 = 1;

/// Name of the entry point a plugin library exports (NUL terminated, for `dlsym`).
pub const ENTRY: &[u8] = b"openfader_plugin_v1\0";

/// One stereo frame, left and right.
pub type Frame = [f32; 2];

/// Where an effect runs ([`EffectVTable::slot`]). Every slot calls `tick` once per frame with the [`Ctx`] below; what
/// differs is the input, the output and the controls behind `level`, `g` and `knob`.
///
/// | Slot | Input → output | `level` | `knob` | `g` (engage ramp) |
/// |---|---|---|---|---|
/// | `Beat` | channel or master → replaces it | LEVEL/DEPTH | 0.5 (unused) | ON button |
/// | `Color` | channel after the EQ → replaces it | 1 | COLOR FX knob past the dead zone, -1 to 1 (0 = centre) | 1 while the knob is off centre |
/// | `Mic` | microphone after its level → replaces it | MIC FX LEVEL | MIC FX parameter, 0 to 1 | ON button |
/// | `Send` | sum of the channels' SEND knobs → the return, added to the master | RETURN level | parameter, 0 to 1 | ON button |
/// | `Lever` | master → replaces it | 1 | lever position, 0 (rest) to 1 | 1 while the lever is off rest |
/// | `Fader` | channel before its fader → replaces it, fader gain included | 1 | fader position, 0 to 1 | 1 while the curve is chosen |
///
/// The host smooths `knob` (10 ms) and fades `g` over 5 ms; with `g` at 0 an effect should stop feeding its delay lines
/// but may let them ring out (trails). A `Send` effect outputs only its return (no dry signal). A `Fader` effect is the
/// fader: it applies the gain for `knob` itself and the host adds trim and crossfader after it; when another curve is
/// chosen `g` falls to 0 and the effect must then pass its input through (`Ctx::mix` does), as the host's curve takes over.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Beat = 0,
    Color = 1,
    Mic = 2,
    Send = 3,
    Lever = 4,
    Fader = 5,
}

impl Slot {
    /// Every slot, in [`EffectVTable::slot`] value order.
    pub const ALL: [Slot; 6] = [
        Slot::Beat,
        Slot::Color,
        Slot::Mic,
        Slot::Send,
        Slot::Lever,
        Slot::Fader,
    ];

    /// The slot of a table's raw `slot` value, `None` for one this SDK does not know.
    pub fn from_raw(v: u32) -> Option<Slot> {
        Slot::ALL.get(v as usize).copied()
    }

    /// Lower-case name, as the docs and the host's messages spell it.
    pub fn name(self) -> &'static str {
        ["beat", "color", "mic", "send", "lever", "fader"][self as usize]
    }
}

/// What an effect gets for one frame (meaning per [`Slot`]).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// Sample rate in Hz.
    pub rate: f32,
    /// Samples per selected beat fraction, tempo-slewed so a BPM change glides instead of clicking.
    pub period: f32,
    /// The selected beat fraction, in beats (1/8 to 16; 1 where the slot has no beat selector).
    pub beats: f32,
    /// LEVEL knob, 0 to 1: how much of the effect is heard.
    pub level: f32,
    /// Engage ramp, 0 (off) to 1 (on); the host fades it over 5 ms when the effect is switched.
    pub g: f32,
    /// The slot's own control (see [`Slot`]).
    pub knob: f32,
}

impl Ctx {
    /// `x` moved towards the effect's signal `w` by LEVEL × the engage ramp; exactly `x` while off.
    pub fn mix(&self, x: Frame, w: Frame) -> Frame {
        let m = self.level * self.g;
        [x[0] + (w[0] - x[0]) * m, x[1] + (w[1] - x[1]) * m]
    }
}

/// One effect as the host calls it. `inst` is what `create` returned. `reset` and `engage` may be null (no-ops);
/// the other functions are required.
#[repr(C)]
pub struct EffectVTable {
    /// NUL-terminated selector label: 1 to 16 characters of ASCII letters, digits, space, `-` or `_`, unique within
    /// its slot.
    pub name: *const c_char,
    /// New instance for `rate` Hz, or null on failure. Not called on the audio thread unless the host has no choice.
    pub create: Option<unsafe extern "C" fn(rate: u32) -> *mut c_void>,
    /// Replaces `*frame` with the effect's output for it.
    pub tick: Option<unsafe extern "C" fn(inst: *mut c_void, frame: *mut Frame, ctx: *const Ctx)>,
    /// Forget everything (the effect was selected, or woke from idle).
    pub reset: Option<unsafe extern "C" fn(inst: *mut c_void)>,
    /// The effect was switched ON (or the lever left its rest): restart phases or captures.
    pub engage: Option<unsafe extern "C" fn(inst: *mut c_void)>,
    /// Frees the instance.
    pub destroy: Option<unsafe extern "C" fn(inst: *mut c_void)>,
    /// [`Slot`] as its `u32` value; 0 (a zeroed field) is `Beat`.
    pub slot: u32,
}

/// What the entry point returns: `count` tables at `effects`, valid until the library is unloaded (never, in practice).
#[repr(C)]
pub struct PluginInfo {
    /// [`ABI`]; the host reads nothing else before checking it.
    pub abi: u32,
    pub count: u32,
    pub effects: *const EffectVTable,
}

// SAFETY: both are immutable tables of function pointers and pointers to static data.
unsafe impl Sync for EffectVTable {}
unsafe impl Sync for PluginInfo {}

/// An effect written in Rust. [`export_effects!`] turns types implementing it into the C ABI.
pub trait Effect: Send {
    /// Selector label, see [`EffectVTable::name`].
    const NAME: &'static CStr;
    /// Where it runs.
    const SLOT: Slot = Slot::Beat;
    /// A new instance. Allocate here (delay lines, tables), not in `tick`.
    fn new(rate: u32) -> Self;
    /// Output frame for input `x`. Audio thread: no allocation, locks or I/O. A panic bypasses the effect until `reset`.
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame;
    /// Forget everything (the effect was selected, or woke from idle).
    fn reset(&mut self);
    /// The effect was switched ON.
    fn engage(&mut self) {}
}

/// An instance plus whether it panicked: a dead one passes its input through.
struct Guarded<E> {
    e: E,
    dead: bool,
}

impl<E: Effect> Guarded<E> {
    /// Runs `f` on the effect, marking the slot dead if it panics.
    fn guard(&mut self, f: impl FnOnce(&mut E)) {
        self.dead = catch_unwind(AssertUnwindSafe(|| f(&mut self.e))).is_err();
    }
}

/// The table of `E`, for [`export_effects!`].
#[doc(hidden)]
pub const fn vtable<E: Effect>() -> EffectVTable {
    unsafe extern "C" fn create<E: Effect>(rate: u32) -> *mut c_void {
        catch_unwind(|| {
            let slot = Box::new(Guarded {
                e: E::new(rate),
                dead: false,
            });
            Box::into_raw(slot).cast::<c_void>()
        })
        .unwrap_or(std::ptr::null_mut())
    }
    unsafe extern "C" fn tick<E: Effect>(inst: *mut c_void, frame: *mut Frame, ctx: *const Ctx) {
        // SAFETY: the host passes the pointer `create` returned and a valid frame and context.
        let (s, x, c) = unsafe { (&mut *inst.cast::<Guarded<E>>(), *frame, &*ctx) };
        if s.dead {
            return;
        }
        match catch_unwind(AssertUnwindSafe(|| s.e.tick(x, c))) {
            Ok(y) => unsafe { *frame = y },
            Err(_) => s.dead = true,
        }
    }
    unsafe extern "C" fn reset<E: Effect>(inst: *mut c_void) {
        unsafe { &mut *inst.cast::<Guarded<E>>() }.guard(E::reset);
    }
    unsafe extern "C" fn engage<E: Effect>(inst: *mut c_void) {
        let s = unsafe { &mut *inst.cast::<Guarded<E>>() };
        if !s.dead {
            s.guard(E::engage);
        }
    }
    unsafe extern "C" fn destroy<E: Effect>(inst: *mut c_void) {
        // SAFETY: `inst` came from `create` and is not used again.
        let slot = unsafe { Box::from_raw(inst.cast::<Guarded<E>>()) };
        let _ = catch_unwind(AssertUnwindSafe(|| drop(slot)));
    }
    EffectVTable {
        name: E::NAME.as_ptr(),
        create: Some(create::<E>),
        tick: Some(tick::<E>),
        reset: Some(reset::<E>),
        engage: Some(engage::<E>),
        destroy: Some(destroy::<E>),
        slot: E::SLOT as u32,
    }
}

/// Exports the listed [`Effect`] types from a `cdylib`: defines the `openfader_plugin_v1` entry point.
/// Use it once per library.
///
/// ```ignore
/// openfader_plugin::export_effects!(Crush, Wobble);
/// ```
#[macro_export]
macro_rules! export_effects {
    ($($e:ty),+ $(,)?) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn openfader_plugin_v1() -> *const $crate::PluginInfo {
            const TABLE: &[$crate::EffectVTable] = &[$($crate::vtable::<$e>()),+];
            static INFO: $crate::PluginInfo = $crate::PluginInfo {
                abi: $crate::ABI,
                count: TABLE.len() as u32,
                effects: TABLE.as_ptr(),
            };
            &INFO
        }
    };
}

pub mod controller;
pub mod dsp;

#[cfg(feature = "library")]
pub mod library;

#[cfg(test)]
mod tests;
