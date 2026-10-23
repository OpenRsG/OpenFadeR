//! The knobs the UI sets ([`Controls`], plain atomics, `f32` as bits) and the values they select.

use super::{BEATS, DEFAULT_BEAT, FALLBACK_BPM};
use crate::audio::store_clamped;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering::Relaxed};

/// A Beat FX selector entry: one of the nine built-ins, or the n-th effect of the plugin libraries
/// ([`crate::plugin::registry`]`().effects`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Echo,
    Delay,
    Reverb,
    Flanger,
    Phaser,
    Roll,
    /// Low-pass sweep.
    Lpf,
    /// High-pass sweep.
    Hpf,
    /// Transformer gate.
    Trans,
    Plugin(u8),
}

impl Effect {
    /// The built-ins, in selector order; plugin effects follow them.
    pub const ALL: [Effect; 9] = [
        Effect::Echo,
        Effect::Delay,
        Effect::Reverb,
        Effect::Flanger,
        Effect::Phaser,
        Effect::Roll,
        Effect::Lpf,
        Effect::Hpf,
        Effect::Trans,
    ];
    /// Selector labels of the built-ins, in the order of [`Effect::ALL`].
    pub const NAMES: [&str; 9] = [
        "ECHO", "DELAY", "REVERB", "FLANGER", "PHASER", "ROLL", "LPF", "HPF", "TRANS",
    ];

    /// Position in the selector (built-ins first, then plugin effects).
    pub fn index(self) -> usize {
        match self {
            Effect::Echo => 0,
            Effect::Delay => 1,
            Effect::Reverb => 2,
            Effect::Flanger => 3,
            Effect::Phaser => 4,
            Effect::Roll => 5,
            Effect::Lpf => 6,
            Effect::Hpf => 7,
            Effect::Trans => 8,
            Effect::Plugin(n) => Effect::ALL.len() + n as usize,
        }
    }

    /// The selector entry at `i` with `plugins` plugin effects loaded; an out-of-range index gives the last entry.
    pub fn from_index_in(i: usize, plugins: usize) -> Effect {
        let plugins = plugins.min(u8::MAX as usize + 1);
        match (i.checked_sub(Effect::ALL.len()), plugins) {
            (None, _) => Effect::ALL[i],
            (Some(_), 0) => Effect::Trans,
            (Some(p), n) => Effect::Plugin(p.min(n - 1) as u8),
        }
    }

    /// [`Effect::from_index_in`] with the effects the plugin libraries brought.
    pub fn from_index(i: usize) -> Effect {
        Effect::from_index_in(i, Effect::plugins().len())
    }

    /// The Beat FX the plugin libraries brought.
    fn plugins() -> &'static [crate::plugin::effect::Effect] {
        crate::plugin::registry().slot(crate::plugin::effect::Slot::Beat)
    }

    /// Every selector label: the built-ins, then the plugin effects.
    pub fn names() -> Vec<String> {
        Effect::NAMES
            .iter()
            .map(|n| n.to_string())
            .chain(Effect::plugins().iter().map(|e| e.name.clone()))
            .collect()
    }
}

/// Where the unit sits: before the fader sum of one channel (A to D = decks 1 to 4), on the AUX channel, or on the sum of all of them.
/// The discriminants of A to D are the channel indices ([`super::Unit::apply`] indexes the buffers with them); the AUX
/// channel's buffer is [`crate::audio::AUX`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assign {
    A,
    B,
    C,
    D,
    Master,
    Aux,
}

impl Assign {
    /// Selector order CH1..CH4, MASTER, AUX; out-of-range indices give MASTER.
    pub fn from_index(i: usize) -> Assign {
        match i {
            0 => Assign::A,
            1 => Assign::B,
            2 => Assign::C,
            3 => Assign::D,
            5 => Assign::Aux,
            _ => Assign::Master,
        }
    }
}
/// A deck's effective BPM (`f32` as bits); anything not finite and positive means [`FALLBACK_BPM`].
pub struct Bpm(AtomicU32);

impl Default for Bpm {
    fn default() -> Bpm {
        Bpm(AtomicU32::new(FALLBACK_BPM.to_bits()))
    }
}

impl Bpm {
    pub fn set(&self, bpm: f32) {
        let bpm = if bpm.is_finite() && bpm > 0.0 {
            bpm
        } else {
            FALLBACK_BPM
        };
        self.0.store(bpm.to_bits(), Relaxed);
    }

    /// Clamped to 20..=400.
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed)).clamp(20.0, 400.0)
    }
}

/// Knobs shared with the audio thread: a Beat FX unit's, or a rack's ([`super::Unit::rack`]: mic, send, lever, fader).
pub struct Controls {
    /// Selector position: [`Effect::index`] for a Beat FX unit, the effect's place in its slot for a rack.
    effect: AtomicU16,
    beat: AtomicU8,
    level: AtomicU32,
    knob: AtomicU32,
    on: AtomicBool,
    assign: AtomicU8,
    bpm: Bpm,
    part: AtomicU8,
}

impl Default for Controls {
    fn default() -> Controls {
        Controls {
            effect: AtomicU16::new(Effect::Echo.index() as u16),
            beat: AtomicU8::new(DEFAULT_BEAT as u8),
            level: AtomicU32::new(0.5f32.to_bits()),
            knob: AtomicU32::new(0.5f32.to_bits()),
            on: AtomicBool::new(false),
            assign: AtomicU8::new(Assign::Master as u8),
            bpm: Bpm::default(),
            part: AtomicU8::new(0),
        }
    }
}

impl Controls {
    pub fn set_effect(&self, e: Effect) {
        self.set_index(e.index());
    }

    /// Selector position (a rack's effect: its place among its slot's plugin effects); out of range = the last one.
    pub fn set_index(&self, i: usize) {
        self.effect.store(i.min(u16::MAX as usize) as u16, Relaxed);
    }

    /// Index into [`BEATS`], clamped.
    pub fn set_beat(&self, i: usize) {
        self.beat.store(i.min(BEATS.len() - 1) as u8, Relaxed);
    }

    /// LEVEL / DEPTH, 0..=1 (non-finite = 0): how much of the effect is heard.
    pub fn set_level(&self, v: f32) {
        store_clamped(&self.level, v, 1.0, 0.0);
    }

    /// The slot's own control ([`openfader_plugin::Slot`]), -1..=1 (non-finite = 0.5); smoothed by the unit.
    pub fn set_knob(&self, v: f32) {
        let v = if v.is_finite() {
            v.clamp(-1.0, 1.0)
        } else {
            0.5
        };
        self.knob.store(v.to_bits(), Relaxed);
    }

    pub fn set_on(&self, on: bool) {
        self.on.store(on, Relaxed);
    }

    pub fn set_assign(&self, a: Assign) {
        self.assign.store(a as u8, Relaxed);
    }

    /// Tempo of the effects; anything not finite and positive means [`FALLBACK_BPM`].
    pub fn set_bpm(&self, bpm: f32) {
        self.bpm.set(bpm);
    }

    pub(super) fn assign(&self) -> Assign {
        Assign::from_index(self.assign.load(Relaxed) as usize)
    }

    /// Stem parts the unit is limited to (bits as in [`crate::audio::stems`]: VOCAL 0, INST 1, BASS 2, DRUMS 3); 0 = the whole channel.
    /// Only meaningful with an assign of CH1..CH4; MASTER ignores it. The unit then runs inside that channel's voice on those
    /// stems only (if the track has none it does nothing).
    pub fn set_part(&self, mask: u8) {
        self.part.store(mask & 0xF, Relaxed);
    }

    pub(super) fn part(&self) -> u8 {
        self.part.load(Relaxed)
    }

    /// For channel `ch`: the stem mask and settings if this unit targets a part of it.
    pub(crate) fn stem_target(&self, ch: usize) -> Option<(u8, Settings)> {
        let m = self.part();
        (m != 0 && self.assign() as usize == ch && ch < 4).then(|| (m, self.settings()))
    }

    pub(crate) fn settings(&self) -> Settings {
        Settings {
            effect: self.effect.load(Relaxed) as usize,
            beats: BEATS[(self.beat.load(Relaxed) as usize).min(BEATS.len() - 1)],
            level: f32::from_bits(self.level.load(Relaxed)),
            knob: f32::from_bits(self.knob.load(Relaxed)),
            on: self.on.load(Relaxed),
            bpm: self.bpm.get(),
        }
    }
}

/// A snapshot of [`Controls`] for one buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Settings {
    /// Selector position (see [`Controls::set_index`]).
    pub effect: usize,
    pub beats: f32,
    pub level: f32,
    pub knob: f32,
    pub on: bool,
    pub bpm: f32,
}
