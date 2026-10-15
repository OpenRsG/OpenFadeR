//! MERGE FX (MFX): a build-up in the usual DJ
//! sense, assembled from the blocks the other effects already have: per channel one [`Unit`], driven by one
//! knob `k` (0..=1) and a preset. A [`Preset`] is three weights; at knob `k` the unit runs
//!   * a rising high-pass (the channel's own one-knob filter, [`super::dsp`], 20 Hz up to 5 kHz),
//!   * a rising noise sweep (COLOR FX NOISE, [`super::cfx`]: high-passed white noise whose colour and level follow the amount),
//!   * a beat-synced echo whose feed and feedback grow with the amount (COLOR FX DUB ECHO),
//!
//! each at `weight × k`, so every part is silent at `k = 0` and none gets weaker as the knob rises
//! ([`Preset::amounts`]). The noise is scaled by the SAMPLE VOLUME knob (the controller's MFXSampleVolume):
//! 0.5 is the nominal level, 1 is +6 dB ([`NOISE_MAX`]).
//!
//! ON is held: pressing starts the build-up, letting go is the drop: the high-pass and noise end within the
//! knob smoothing (~10 ms) and the echo's feed stops, its repeats ring out. ponytail: it could end on the
//! next beat instead; the audio thread has no beat position (as in [`super::fx`]), so it always ends at once. ABORT
//! ([`Controls::abort`]) is instant: the whole effect, tail included, fades out over [`FADE_S`] and is dropped.
//! Off and silent the unit is idle: the signal is not touched at all (bit-exact, no CPU).
//! Everything is allocated in [`Unit::new`]; [`Unit::apply`] does not allocate or block.

#[cfg(test)]
mod tests;

use super::cfx::{self, Cfx, Kind};
use super::dsp::{self, DEAD};
use super::fx::{Bpm, Frame, finite, moved, ramp};
use super::{clamped, store_clamped};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering::Relaxed};

/// Noise gain at full SAMPLE VOLUME.
pub const NOISE_MAX: f32 = 2.0;
/// Abort fade-out.
pub const FADE_S: f32 = 0.005;
/// SAMPLE VOLUME at start (its controller knob rests at the centre).
pub const DEFAULT_VOLUME: f32 = 0.5;
/// Rising high-pass: the one-knob filter's high side.
const HPF_EQ: [f32; 3] = [0.0; 3];

/// One build-up recipe: how far each part goes when the knob is full (`0..=1` each).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub hpf: f32,
    pub noise: f32,
    pub echo: f32,
}

pub const PRESETS: [Preset; 4] = [
    Preset {
        name: "RISE",
        hpf: 1.0,
        noise: 1.0,
        echo: 0.0,
    },
    Preset {
        name: "ECHO",
        hpf: 0.5,
        noise: 0.0,
        echo: 1.0,
    },
    Preset {
        name: "WASH",
        hpf: 0.3,
        noise: 1.0,
        echo: 0.5,
    },
    Preset {
        name: "FULL",
        hpf: 1.0,
        noise: 0.7,
        echo: 1.0,
    },
];

impl Preset {
    /// `[hpf, noise, echo]` amounts (`0..=1`) at knob `k`; `k` outside 0..=1 (or not finite) is clamped (0).
    pub fn amounts(&self, k: f32) -> [f32; 3] {
        let k = clamped(k, 1.0, 0.0);
        [self.hpf * k, self.noise * k, self.echo * k]
    }
}

/// Knobs of one side (`f32` as bits), set by the UI / controller, read by the audio thread.
pub struct Controls {
    preset: AtomicU8,
    knob: AtomicU32,
    volume: AtomicU32,
    on: AtomicBool,
    aborts: AtomicU32,
    bpm: Bpm,
}

impl Default for Controls {
    fn default() -> Controls {
        Controls {
            preset: AtomicU8::new(0),
            knob: AtomicU32::new(0f32.to_bits()),
            volume: AtomicU32::new(DEFAULT_VOLUME.to_bits()),
            on: AtomicBool::new(false),
            aborts: AtomicU32::new(0),
            bpm: Bpm::default(),
        }
    }
}

impl Controls {
    /// Index into [`PRESETS`], clamped.
    pub fn set_preset(&self, i: usize) {
        self.preset.store(i.min(PRESETS.len() - 1) as u8, Relaxed);
    }

    /// The build-up knob, 0..=1 (non-finite = 0).
    pub fn set_knob(&self, k: f32) {
        store_clamped(&self.knob, k, 1.0, 0.0);
    }

    /// SAMPLE VOLUME: the noise level, 0..=1 (non-finite = [`DEFAULT_VOLUME`]).
    pub fn set_volume(&self, v: f32) {
        store_clamped(&self.volume, v, 1.0, DEFAULT_VOLUME);
    }

    /// Held: the build-up runs while true; false is the drop.
    pub fn set_on(&self, on: bool) {
        self.on.store(on, Relaxed);
    }

    /// Instant off: the effect and its tail are dropped (ON is cleared; the next ON press starts afresh).
    pub fn abort(&self) {
        self.on.store(false, Relaxed);
        self.aborts.fetch_add(1, Relaxed);
    }

    /// Effective BPM of the side's deck.
    pub fn set_bpm(&self, bpm: f32) {
        self.bpm.set(bpm);
    }

    fn preset(&self) -> &'static Preset {
        &PRESETS[(self.preset.load(Relaxed) as usize).min(PRESETS.len() - 1)]
    }

    fn knob(&self) -> f32 {
        f32::from_bits(self.knob.load(Relaxed))
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Relaxed))
    }
}

/// COLOR FX knob (the dead zone, then `0..=1` amount) that makes a COLOR FX stage run at `amount`.
fn cfx_knob(amount: f32) -> f32 {
    if amount > 0.0 {
        DEAD + (1.0 - DEAD) * amount
    } else {
        0.0
    }
}

/// A COLOR FX stage with the controls the unit drives it by.
struct Part {
    cfx: Cfx,
    c: cfx::Controls,
}

impl Part {
    fn new(rate: u32, kind: Kind) -> Part {
        let c = cfx::Controls::default();
        c.set_kind(kind);
        Part {
            cfx: Cfx::new(rate),
            c,
        }
    }

    fn aim(&mut self, amount: f32, bpm: f32) {
        self.c.set_knob(cfx_knob(amount));
        self.c.set_bpm(bpm);
        self.cfx.begin(&self.c);
    }
}

/// One side's MERGE FX, audio thread only.
pub struct Unit {
    rate: f32,
    hpf: dsp::Fx,
    noise: Part,
    echo: Part,
    /// Wet share: 1, ramping to 0 after an abort; `ramp` per frame.
    fade: f32,
    ramp: f32,
    /// Aborted and not yet re-engaged; the knob value held while the fade runs.
    cut: bool,
    k: f32,
    aborts: u32,
    on: bool,
    /// Consecutive silent frames while off, and how many make the unit idle.
    quiet: u32,
    hold: u32,
}

impl Unit {
    pub fn new(rate: u32) -> Unit {
        let r = rate as f32;
        Unit {
            rate: r,
            hpf: dsp::Fx::new(),
            noise: Part::new(rate, Kind::Noise),
            echo: Part::new(rate, Kind::DubEcho),
            fade: 1.0,
            ramp: 1.0 / (FADE_S * r),
            cut: false,
            k: 0.0,
            aborts: 0,
            on: false,
            quiet: u32::MAX,
            hold: 0,
        }
    }

    /// Off, aborted or faded out, and every echo repeat silent (-120 dBFS): the signal is passed untouched.
    pub fn idle(&self) -> bool {
        !self.on && self.quiet >= self.hold
    }

    /// Takes the knobs for the next `n` frames.
    fn begin(&mut self, c: &Controls, n: usize) {
        let on = c.on.load(Relaxed);
        let aborts = c.aborts.load(Relaxed);
        if aborts != self.aborts {
            self.aborts = aborts;
            self.cut = true;
        } else if on && !self.on {
            self.cut = false;
        }
        if on && !self.on {
            self.quiet = 0;
        }
        self.on = on;
        if !self.cut {
            self.k = if on { c.knob() } else { 0.0 };
        }
        let bpm = c.bpm.get();
        let [h, nz, e] = c.preset().amounts(self.k);
        self.hpf.set(self.rate as f64, HPF_EQ, h, n);
        self.noise.aim(nz, bpm);
        self.echo.aim(e, bpm);
        self.hold = self.echo.cfx.hold();
    }

    fn frame(&mut self, x: Frame, vol: f32) -> Frame {
        let want = if self.cut { 0.0 } else { 1.0 };
        self.fade = ramp(self.fade, want, self.ramp);
        let (hl, hr) = self.hpf.process(x[0], x[1]);
        let (el, er) = self.echo.cfx.process(hl, hr);
        let (nl, nr) = self.noise.cfx.process(0.0, 0.0);
        let g = vol * NOISE_MAX;
        let mut y = [el + nl * g, er + nr * g];
        if !finite(y) {
            y = x;
        }
        let out = [
            x[0] + (y[0] - x[0]) * self.fade,
            x[1] + (y[1] - x[1]) * self.fade,
        ];
        self.quiet = if self.on || moved(x, out) {
            0
        } else {
            self.quiet.saturating_add(1)
        };
        out
    }

    /// Runs the unit over a channel's master path (the cue path is never touched). Idle = no work, no change.
    pub fn apply(&mut self, c: &Controls, buf: &mut [(Frame, Frame)]) {
        self.begin(c, buf.len());
        if self.idle() {
            return;
        }
        let vol = c.volume();
        for f in buf.iter_mut() {
            f.0 = self.frame(f.0, vol);
        }
        if self.cut && self.fade == 0.0 {
            self.hpf = dsp::Fx::new();
            self.noise.cfx.flush();
            self.echo.cfx.flush();
            self.quiet = u32::MAX; // dropped: idle until the next ON
        }
    }
}
