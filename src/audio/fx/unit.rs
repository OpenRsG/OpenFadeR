//! The unit: audio-thread state around a list of [`Stage`]s, with the engage ramp, tempo slew, knob smoothing, trails and
//! idle detection they share. Everything is allocated in [`Unit::new`] / [`Unit::rack`]. A Beat FX unit holds the nine
//! built-ins and one stage per Beat FX plugin; a rack holds one stage per plugin effect of another slot (mic, send,
//! lever, fader) and nothing built in. A unit is either a whole-channel unit (the engine's [`Unit::apply`]) or one its
//! owner feeds frame by frame (`set` / `process` / `release`: a voice's stem part, the mic, the send bus, a fader).

use super::{
    Assign, BEATS, Controls, Ctx, DEFAULT_BEAT, Effect, FALLBACK_BPM, Frame, MAX_DELAY_S, Settings,
    Stage, echo, filter, finite, flanger, moved, phaser, ramp, reverb, roll, trans,
};
use crate::plugin::effect::{Effect as PluginEffect, Slot};

const ENGAGE_S: f32 = 0.005;
const TEMPO_SLEW_S: f32 = 0.03;
/// Time constant of the knob smoothing.
const KNOB_S: f32 = 0.01;

/// The stage of selector entry `e`; plugin effects come from `plugins`.
fn stage(e: Effect, rate: u32, plugins: &[PluginEffect]) -> Box<dyn Stage> {
    match e {
        Effect::Echo => Box::new(echo::Echo::echo(rate)),
        Effect::Delay => Box::new(echo::Echo::delay(rate)),
        Effect::Reverb => Box::new(reverb::Reverb::new(rate)),
        Effect::Flanger => Box::new(flanger::Flanger::new(rate)),
        Effect::Phaser => Box::new(phaser::Phaser::default()),
        Effect::Roll => Box::new(roll::Roll::new(rate)),
        Effect::Lpf => Box::new(filter::Filter::new(false)),
        Effect::Hpf => Box::new(filter::Filter::new(true)),
        Effect::Trans => Box::new(trans::Trans::default()),
        Effect::Plugin(n) => plugins[n as usize].stage(rate),
    }
}

pub struct Unit {
    rate: f32,
    /// One per selector entry, in [`Effect::index`] order (a rack: its slot's order). May be empty (a rack of a slot no
    /// plugin fills): the unit then stays idle.
    stages: Vec<Box<dyn Stage>>,
    /// Index into `stages` of `s.effect`.
    cur: usize,
    s: Settings,
    g: f32,
    ramp: f32,
    /// Smoothed and target period in samples, and the one-pole coefficient between them.
    period: f32,
    target: f32,
    slew: f32,
    /// Smoothed knob and its one-pole coefficient.
    knob: f32,
    knob_k: f32,
    /// Consecutive silent frames while off, and how many make the unit idle.
    quiet: u32,
    hold: u32,
}

impl Unit {
    pub fn new(rate: u32) -> Unit {
        Unit::with(rate, crate::plugin::registry().slot(Slot::Beat))
    }

    /// A unit with the built-ins and `plugins` as the plugin effects.
    pub(crate) fn with(rate: u32, plugins: &[PluginEffect]) -> Unit {
        Unit::of(
            rate,
            (0..Effect::ALL.len() + plugins.len())
                .map(|i| stage(Effect::from_index_in(i, plugins.len()), rate, plugins))
                .collect(),
        )
    }

    /// A rack: one stage per plugin effect of `slot`, selected by [`Controls::set_index`].
    pub fn rack(slot: Slot, rate: u32) -> Unit {
        Unit::rack_with(rate, crate::plugin::registry().slot(slot))
    }

    /// A rack of `effects`.
    pub(crate) fn rack_with(rate: u32, effects: &[PluginEffect]) -> Unit {
        Unit::of(rate, effects.iter().map(|e| e.stage(rate)).collect())
    }

    fn of(rate: u32, stages: Vec<Box<dyn Stage>>) -> Unit {
        let r = rate as f32;
        Unit {
            rate: r,
            stages,
            cur: 0,
            s: Settings {
                effect: 0,
                beats: BEATS[DEFAULT_BEAT],
                level: 0.5,
                knob: 0.5,
                on: false,
                bpm: FALLBACK_BPM,
            },
            g: 0.0,
            ramp: 1.0 / (ENGAGE_S * r),
            period: 0.0,
            target: 0.0,
            slew: 1.0 - (-1.0 / (TEMPO_SLEW_S * r)).exp(),
            knob: 0.5,
            knob_k: 1.0 - (-1.0 / (KNOB_S * r)).exp(),
            quiet: u32::MAX,
            hold: 0,
        }
    }

    /// Effects this unit can select.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// Off, faded out and every trail below [`QUIET`]: [`Unit::process`] would return its input unchanged.
    pub fn idle(&self) -> bool {
        !self.s.on && self.g == 0.0 && self.quiet >= self.hold
    }

    /// Takes the knob values for the next frames: effect switch, ON edge, tempo.
    pub(crate) fn set(&mut self, s: Settings) {
        if self.stages.is_empty() {
            return;
        }
        let idle = self.idle();
        let (changed, edge) = (s.effect != self.s.effect, s.on && !self.s.on);
        // An entry the unit was not built with (a different plugin list) falls back to the last stage.
        self.cur = s.effect.min(self.stages.len() - 1);
        let st = &mut self.stages[self.cur];
        if changed || (edge && idle) {
            st.reset();
        }
        if (changed || edge) && s.on {
            st.engage();
        }
        if edge {
            self.quiet = 0;
        }
        self.target = s.beats * 60.0 / s.bpm * self.rate;
        if changed || idle {
            self.period = self.target;
            self.knob = s.knob;
        }
        self.hold = (self.target.min(MAX_DELAY_S * self.rate) + 0.05 * self.rate) as u32;
        self.s = s;
    }

    /// Switch the effect OFF: it fades out and its trails ring out ([`Unit::idle`] says when it is done).
    pub fn release(&mut self) {
        self.set(Settings {
            on: false,
            ..self.s
        });
    }

    /// Drop everything and idle at once (the unit's controls moved to a stem part: [`Unit::apply`] must not touch it).
    fn silence(&mut self) {
        if let Some(st) = self.stages.get_mut(self.cur) {
            st.reset();
        }
        self.s.on = false;
        self.g = 0.0;
        self.quiet = u32::MAX;
    }

    /// One frame through the current effect (the voice's stem-part path calls it per frame).
    pub fn process(&mut self, x: Frame) -> Frame {
        let Some(st) = self.stages.get_mut(self.cur) else {
            return x;
        };
        let s = self.s;
        let want = if s.on { 1.0 } else { 0.0 };
        self.g = ramp(self.g, want, self.ramp);
        self.period += (self.target - self.period) * self.slew;
        self.knob += (s.knob - self.knob) * self.knob_k;
        let c = Ctx {
            rate: self.rate,
            period: self.period,
            beats: s.beats,
            level: s.level,
            g: self.g,
            knob: self.knob,
        };
        let mut y = st.tick(x, &c);
        if !finite(y) {
            st.reset(); // a blown-up feedback loop: drop it rather than play it
            y = x;
        }
        if s.on || self.g > 0.0 || moved(x, y) {
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(1);
        }
        y
    }

    /// Runs the unit over the first `n` frames of the channels' master paths (`.0` of each pair; the cue
    /// path is never touched): in place on one channel (A to D), or on the sum of all four, where the difference the effect
    /// makes is added back to channel A so the mixer's own sum stays the only sum. Idle = no work, no change.
    pub fn apply(&mut self, c: &Controls, buf: &mut [Vec<(Frame, Frame)>], n: usize) {
        if c.part() != 0 && c.assign() != Assign::Master {
            if !self.idle() {
                self.silence();
            }
            return;
        }
        self.set(c.settings());
        if self.idle() {
            return;
        }
        match c.assign() {
            Assign::Master => {
                for i in 0..n {
                    let dry = buf
                        .iter()
                        .fold([0.0; 2], |s, b| [s[0] + b[i].0[0], s[1] + b[i].0[1]]);
                    let wet = self.process(dry);
                    for k in 0..2 {
                        buf[0][i].0[k] += wet[k] - dry[k];
                    }
                }
            }
            a => {
                if let Some(b) = buf.get_mut(a as usize) {
                    for f in &mut b[..n] {
                        f.0 = self.process(f.0);
                    }
                }
            }
        }
    }
}
