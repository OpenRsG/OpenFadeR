//! COLOR FX (CFX): the one knob per channel, with a global type (FILTER,
//! SPACE, DUB ECHO, SWEEP, NOISE, CRUSH; each child is a classic
//! published design and says so). Sits after the EQ and before the fader and cue split in the channel
//! strip ([`super::player`]'s voice), so the headphones hear it too.
//!
//! Base: [`Kind`], [`Controls`] (kind, knob and the deck's tempo, plain atomics the UI sets and the audio
//! thread reads), [`Cfx`] (audio-thread state, everything allocated in [`Cfx::new`]) and [`Stage`].
//! Children, one [`Stage`] each: [`space`] (reverb send), [`dub`] (beat-synced echo), [`sweep`] (band-pass
//! sweep), [`noise`] (filtered white noise), [`crush`] (bit / sample-rate reduction). FILTER is not a
//! child: it is the channel's existing biquad ([`super::dsp`]), so [`Cfx::begin`] hands the knob to it
//! unchanged and [`Cfx::process`] does nothing: bit-identical to the plain filter.
//!
//! Knob `x` is -1..=1 with the filter's dead zone ([`DEAD`]): inside it every kind is off. Outside it the
//! stage gets `x` rescaled to start at 0 (`±amount`), smoothed over [`SMOOTH_S`] so a knob move never
//! clicks. SPACE and DUB ECHO keep ringing when the knob returns to the centre until their output is
//! below -120 dBFS for longer than the stage's hold; then the channel is passed through untouched
//! (bit-exact, no CPU), as the Beat FX unit does ([`super::fx`]). Switching kind drops the old tail.
//! COLOR slot plugin effects ([`crate::plugin`]) follow CRUSH in the selector: [`Controls::set_index`] picks one and
//! [`names`] lists all. A [`Plugin`] wraps the effect's stage with the SDK context (`beats` 1, `level` 1, `g` 1 while
//! the knob is off centre, `knob` the signed amount) under the same dead zone, smoothing and idle rules; its tail
//! rings for one beat plus 50 ms (inferred, like a Beat FX unit) after the knob returns to the centre.
//! Tempo: [`Controls::set_bpm`] is the channel's deck's effective BPM (pushed by the mixer).

mod crush;
mod dub;
mod noise;
mod space;
mod sweep;
#[cfg(test)]
mod tests;

use super::dsp::DEAD;
use super::fx::{self, Bpm, Frame, MAX_DELAY_S, finite, moved};
use crate::plugin::{effect::Slot, registry};
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};

/// Time constant of the knob smoothing.
const SMOOTH_S: f32 = 0.01;
const TEMPO_SLEW_S: f32 = 0.03;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Filter,
    Space,
    DubEcho,
    Sweep,
    Noise,
    Crush,
}

impl Kind {
    /// In selector order.
    pub const ALL: [Kind; 6] = [
        Kind::Filter,
        Kind::Space,
        Kind::DubEcho,
        Kind::Sweep,
        Kind::Noise,
        Kind::Crush,
    ];
    pub const NAMES: [&'static str; 6] = ["FILTER", "SPACE", "DUB ECHO", "SWEEP", "NOISE", "CRUSH"];

    /// Out-of-range indices give the last kind.
    pub fn from_index(i: usize) -> Kind {
        Kind::ALL[i.min(Kind::ALL.len() - 1)]
    }
}

/// Knob, type and tempo of one channel (`f32` as bits). Defaults: FILTER, centre, [`FALLBACK_BPM`](super::fx::FALLBACK_BPM).
#[derive(Default)]
pub struct Controls {
    kind: AtomicU8,
    knob: AtomicU32,
    bpm: Bpm,
}

impl Controls {
    pub fn set_kind(&self, k: Kind) {
        self.set_index(k as usize);
    }

    /// Selector position: a [`Kind`] as its index, then the COLOR plugin effects in registry order (out of range = the last).
    pub fn set_index(&self, i: usize) {
        self.kind.store(i.min(u8::MAX as usize) as u8, Relaxed);
    }

    /// The CFX knob, clamped to -1..=1 (non-finite = 0): 0 off, negative left of centre, positive right.
    pub fn set_knob(&self, x: f32) {
        self.knob.store(
            if x.is_finite() {
                x.clamp(-1.0, 1.0)
            } else {
                0.0
            }
            .to_bits(),
            Relaxed,
        );
    }

    /// Effective BPM of the channel's deck.
    pub fn set_bpm(&self, bpm: f32) {
        self.bpm.set(bpm);
    }

    fn index(&self) -> usize {
        self.kind.load(Relaxed) as usize
    }

    fn knob(&self) -> f32 {
        f32::from_bits(self.knob.load(Relaxed))
    }
}

/// What a stage needs for one frame.
struct Ctx {
    /// Knob past the dead zone, smoothed: `-1..=1`, 0 = off (never passed to a stage while 0 and idle).
    x: f32,
    rate: f32,
    /// Samples per beat (tempo-slewed, so a BPM change glides instead of clicking).
    period: f32,
}

impl Ctx {
    /// How much of the effect, `0..=1`.
    fn a(&self) -> f32 {
        self.x.abs()
    }
}

trait Stage: Send {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame;
    /// Forget everything (the stage was selected, or woke from idle).
    fn reset(&mut self);
    /// Silent frames (no [`moved`] output, knob at the centre) after which the stage is idle; 0 = no tail.
    fn hold(&self, _period: f32, _rate: f32) -> u32 {
        0
    }
}

/// Stages in the order of [`Kind`] after FILTER.
fn stages(rate: u32) -> Vec<Box<dyn Stage>> {
    vec![
        Box::new(space::Space::new(rate)),
        Box::new(dub::Dub::new(rate)),
        Box::new(sweep::Sweep::default()),
        Box::new(noise::Noise::default()),
        Box::new(crush::Crush::default()),
    ]
}

/// Every selector entry's label: the [`Kind`]s, then the COLOR plugin effects.
pub fn names() -> Vec<String> {
    let plugins = registry().slot(Slot::Color).iter().map(|e| e.name.clone());
    Kind::NAMES
        .iter()
        .map(|n| n.to_string())
        .chain(plugins)
        .collect()
}

/// A COLOR plugin effect as a stage (see the module docs).
struct Plugin {
    st: Box<dyn fx::Stage>,
    /// Last `g`: the effect's `engage` runs when the knob leaves the centre.
    on: bool,
}

impl Stage for Plugin {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let on = c.x != 0.0;
        if on && !self.on {
            self.st.engage();
        }
        self.on = on;
        self.st.tick(
            x,
            &fx::Ctx {
                rate: c.rate,
                period: c.period,
                beats: 1.0,
                level: 1.0,
                g: on as u8 as f32,
                knob: c.x,
            },
        )
    }

    fn reset(&mut self) {
        self.st.reset();
        self.on = false;
    }

    fn hold(&self, period: f32, rate: f32) -> u32 {
        (period.min(MAX_DELAY_S * rate) + 0.05 * rate) as u32
    }
}

/// One channel's CFX, audio thread only.
pub struct Cfx {
    rate: f32,
    stages: Vec<Box<dyn Stage>>,
    /// Selector position ([`Controls::set_index`]); 0 is FILTER.
    sel: usize,
    /// Knob target and its smoothed value, and the smoothing coefficient.
    x: f32,
    xs: f32,
    smooth: f32,
    /// Smoothed and target period in samples, and the one-pole coefficient between them.
    period: f32,
    target: f32,
    slew: f32,
    /// Bypassed: the stage holds old state, reset it before the next use.
    idle: bool,
    /// Consecutive silent frames at the centre, and how many make the stage idle.
    quiet: u32,
    hold: u32,
}

impl Cfx {
    pub fn new(rate: u32) -> Cfx {
        Cfx::with(
            rate,
            registry()
                .slot(Slot::Color)
                .iter()
                .map(|e| e.stage(rate))
                .collect(),
        )
    }

    /// A CFX whose plugin effects are `plugins`.
    fn with(rate: u32, plugins: Vec<Box<dyn fx::Stage>>) -> Cfx {
        let r = rate as f32;
        let mut stages = stages(rate);
        stages.extend(
            plugins
                .into_iter()
                .map(|st| Box::new(Plugin { st, on: false }) as Box<dyn Stage>),
        );
        Cfx {
            rate: r,
            stages,
            sel: 0,
            x: 0.0,
            xs: 0.0,
            smooth: 1.0 - (-1.0 / (SMOOTH_S * r)).exp(),
            period: 0.0,
            target: 0.0,
            slew: 1.0 - (-1.0 / (TEMPO_SLEW_S * r)).exp(),
            idle: true,
            quiet: 0,
            hold: 0,
        }
    }

    /// Takes the controls for the next buffer. Returns the knob value for the channel's own filter:
    /// the knob itself while the type is FILTER, else 0 (filter off).
    pub fn begin(&mut self, c: &Controls) -> f32 {
        let sel = c.index().min(self.stages.len());
        if sel != self.sel {
            self.sel = sel;
            self.idle = true; // the old kind's tail is dropped, the new stage starts clean
        }
        self.x = c.knob();
        self.target = 60.0 / c.bpm.get() * self.rate;
        if self.idle {
            self.period = self.target;
        }
        if sel == 0 {
            return self.x;
        }
        self.hold = self.stages[sel - 1].hold(self.target, self.rate);
        0.0
    }

    /// Silent frames at the centre after which the current kind is idle (0 for FILTER).
    pub fn hold(&self) -> u32 {
        self.hold
    }

    /// Drops the tail and the knob smoothing at once: the next frame at knob 0 is the plain input, a later knob starts clean.
    pub fn flush(&mut self) {
        self.idle = true;
        self.xs = 0.0;
    }

    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.sel == 0 {
            return (l, r);
        }
        let d = self.x - self.xs;
        self.xs = if d.abs() < 1e-4 {
            self.x
        } else {
            self.xs + d * self.smooth
        }; // f32 would stall just short of the target
        let a = ((self.xs.abs() - DEAD) / (1.0 - DEAD)).max(0.0);
        if a == 0.0 && (self.idle || self.quiet >= self.hold) {
            self.idle = true;
            return (l, r);
        }
        self.period += (self.target - self.period) * self.slew;
        let st = &mut self.stages[self.sel - 1];
        if self.idle {
            st.reset();
            (self.idle, self.quiet) = (false, 0);
        }
        let x = [l, r];
        let mut y = st.tick(
            x,
            &Ctx {
                x: a.copysign(self.xs),
                rate: self.rate,
                period: self.period,
            },
        );
        if !finite(y) {
            st.reset(); // a blown-up feedback loop: drop it rather than play it
            y = x;
        }
        self.quiet = if a > 0.0 || moved(x, y) {
            0
        } else {
            self.quiet.saturating_add(1)
        };
        (y[0], y[1])
    }
}
