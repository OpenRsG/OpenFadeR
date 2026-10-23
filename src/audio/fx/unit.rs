//! The selected effect's audio-thread state: engage ramp, tempo slew, knob smoothing, trails and idle detection.
//! Stage replacements are prepared by one shared worker and exchanged without waiting in [`Unit::set`].
//! A pending selection lets an audible trail of the old effect ring on (its input closed; dry when it had none) until the new
//! stage is ready, and the old stage is then faded out beside it; the old selection is never activated.

use super::prepare::{Exchange, Prepared, Worker};
use super::{
    Assign, BEATS, Controls, Ctx, DEFAULT_BEAT, Effect, FALLBACK_BPM, Frame, MAX_DELAY_S, Settings,
    Stage, echo, filter, finite, flanger, moved, phaser, ramp, reverb, roll, trans,
};
use crate::plugin::effect::{Effect as PluginEffect, Slot};
use std::sync::Arc;
use std::sync::atomic::Ordering::Release;

const ENGAGE_S: f32 = 0.005;
const TEMPO_SLEW_S: f32 = 0.03;
/// Time constant of the knob smoothing.
const KNOB_S: f32 = 0.01;

/// The stage of selector entry `e`; plugin effects come from `plugins`.
pub(super) fn stage(e: Effect, rate: u32, plugins: &[PluginEffect]) -> Box<dyn Stage> {
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
    stage: Option<Prepared>,
    len: usize,
    exchange: Arc<Exchange>,
    worker: Arc<Worker>,
    pending: bool,
    s: Settings,
    g: f32,
    ramp: f32,
    /// Smoothed and target period in samples, and the one-pole coefficient between them.
    period: f32,
    target: f32,
    slew: f32,
    /// Smoothed knob and LEVEL and their one-pole coefficient.
    knob: f32,
    level: f32,
    knob_k: f32,
    /// The stage a switch replaced while its trail was audible: it rings on beside the new one with its input closed, faded
    /// out by `fade` (1 -> 0, over the engage ramp; 0 = none), then goes to the worker.
    fading: Option<Prepared>,
    fade: f32,
    /// A selection is pending and the old stage's trail is carried across it.
    tail: bool,
    /// Consecutive silent frames while off, and how many make the unit idle.
    quiet: u32,
    hold: u32,
}

impl Unit {
    pub fn new(rate: u32) -> Unit {
        Unit::with(rate, crate::plugin::registry().slot(Slot::Beat))
    }

    /// A Beat FX unit with `plugins` after the built-ins.
    pub(crate) fn with(rate: u32, plugins: &[PluginEffect]) -> Unit {
        Unit::of(rate, true, plugins)
    }

    /// A rack of plugin effects of `slot`, selected by [`Controls::set_index`].
    pub fn rack(slot: Slot, rate: u32) -> Unit {
        Unit::rack_with(rate, crate::plugin::registry().slot(slot))
    }

    /// A rack of `effects`.
    pub(crate) fn rack_with(rate: u32, effects: &[PluginEffect]) -> Unit {
        Unit::of(rate, false, effects)
    }

    fn of(rate: u32, beat: bool, effects: &[PluginEffect]) -> Unit {
        let r = rate as f32;
        let len = if beat {
            Effect::ALL.len() + effects.len()
        } else {
            effects.len()
        };
        let selected = if beat {
            Some(stage(Effect::Echo, rate, effects))
        } else {
            effects.first().map(|e| e.stage(rate))
        };
        let exchange = Exchange::new(rate, beat, effects);
        let worker = Worker::shared();
        worker.register(exchange.clone());
        Unit {
            rate: r,
            stage: selected.map(|stage| Prepared { index: 0, stage }),
            len,
            exchange,
            worker,
            pending: false,
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
            level: 0.5,
            fading: None,
            fade: 0.0,
            tail: false,
            quiet: u32::MAX,
            hold: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn prepare(&mut self, effect: usize) {
        use std::time::{Duration, Instant};
        if self.is_empty() {
            return;
        }
        let desired = effect.min(self.len - 1);
        self.exchange.desired.store(desired, Release);
        self.worker.wake();
        let deadline = Instant::now() + Duration::from_secs(5);
        let previous = self.s;
        loop {
            self.set(Settings { effect, ..previous });
            if !self.pending {
                self.s = previous;
                return;
            }
            assert!(Instant::now() < deadline, "FX preparation timed out");
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    /// Effects this unit can select.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Off, faded out and every trail below [`QUIET`]: [`Unit::process`] would return its input unchanged.
    pub fn idle(&self) -> bool {
        !self.s.on && self.g == 0.0 && self.quiet >= self.hold && self.fade == 0.0
    }

    /// Takes the knob values for the next frames: effect switch, ON edge, tempo.
    pub(crate) fn set(&mut self, s: Settings) {
        if self.is_empty() {
            return;
        }
        let desired = s.effect.min(self.len - 1);
        if self.exchange.desired.swap(desired, Release) != desired {
            self.worker.wake();
        }
        let mut installed = false;
        let was_pending = self.pending;
        self.pending = self.stage.as_ref().is_none_or(|p| p.index != desired);
        if self.pending && !was_pending {
            self.tail = !self.idle(); // an audible trail rings on, input closed, until the new stage is ready
        }
        if self.pending || self.fading.is_some() {
            if let Ok(mut h) = self.exchange.handoff.try_lock() {
                if self.fade == 0.0 && self.fading.is_some() && h.retired.is_none() {
                    h.retired = self.fading.take(); // faded out: the worker destroys it
                    self.worker.wake();
                }
                if self.pending
                    && h.retired.is_none()
                    && h.ready.as_ref().is_some_and(|p| p.index == desired)
                {
                    let old = self.stage.take();
                    if self.tail {
                        // the trail goes on beside the new stage and is faded out (an earlier one still fading is cut)
                        h.retired = std::mem::replace(&mut self.fading, old);
                        self.fade = 1.0;
                    } else {
                        h.retired = old;
                    }
                    self.stage = h.ready.take();
                    self.exchange.active.store(desired, Release);
                    self.pending = false;
                    installed = true;
                    self.worker.wake();
                }
            }
        }
        let idle = self.idle();
        let (changed, edge) = (installed || s.effect != self.s.effect, s.on && !self.s.on);
        if !self.pending {
            let st = &mut self.stage.as_mut().unwrap().stage;
            if changed || (edge && idle) {
                st.reset();
            }
            if (changed || edge) && s.on {
                st.engage();
            }
        }
        if edge {
            self.quiet = 0;
        }
        self.target = s.beats * 60.0 / s.bpm * self.rate;
        if changed || idle {
            self.period = self.target;
            self.knob = s.knob;
            self.level = s.level;
        }
        self.hold = (self.target.min(MAX_DELAY_S * self.rate) + 0.05 * self.rate) as u32;
        self.s = s;
        if !self.pending {
            self.tail = false;
        }
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
        if let Some(st) = &mut self.stage {
            st.stage.reset();
        }
        self.s.on = false;
        self.g = 0.0;
        self.fade = 0.0;
        self.quiet = u32::MAX;
    }

    /// One frame through the current effect (the voice's stem-part path calls it per frame).
    pub fn process(&mut self, x: Frame) -> Frame {
        if self.pending && !self.tail {
            return x;
        }
        let Some(selected) = &mut self.stage else {
            return x;
        };
        let st = &mut selected.stage;
        let s = self.s;
        // A pending selection never engages the old effect: its trail just rings on.
        let want = if s.on && !self.pending { 1.0 } else { 0.0 };
        self.g = ramp(self.g, want, self.ramp);
        self.period += (self.target - self.period) * self.slew;
        self.knob += (s.knob - self.knob) * self.knob_k;
        self.level += (s.level - self.level) * self.knob_k;
        let c = Ctx {
            rate: self.rate,
            period: self.period,
            beats: s.beats,
            level: self.level,
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
        if let Some(old) = &mut self.fading {
            self.fade = ramp(self.fade, 0.0, self.ramp);
            if self.fade > 0.0 {
                let z = old.stage.tick(x, &Ctx { g: 0.0, ..c });
                if finite(z) {
                    y = [0, 1].map(|k| y[k] + (z[k] - x[k]) * self.fade);
                } else {
                    old.stage.reset();
                }
            }
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

impl Drop for Unit {
    fn drop(&mut self) {
        self.worker
            .retire(&self.exchange, [self.stage.take(), self.fading.take()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn shutdown_destroys_the_current_stage_on_the_worker() {
        let mut u = Unit::new(48_000);
        let (tx, rx) = mpsc::channel();
        u.stage = Some(Prepared {
            index: 0,
            stage: Box::new(Probe(tx)),
        });
        drop(u);
        let destroyed_on = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(destroyed_on, thread::current().id());
    }

    struct Probe(mpsc::Sender<thread::ThreadId>);

    impl Stage for Probe {
        fn tick(&mut self, _: Frame, _: &Ctx) -> Frame {
            panic!("a pending selection ran the previous stage");
        }
        fn reset(&mut self) {}
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.send(thread::current().id()).unwrap();
        }
    }

    #[test]
    fn rapid_selection_and_pending_on_off_use_only_the_latest_stage() {
        for final_on in [false, true] {
            let mut u = Unit::new(48_000);
            let (tx, rx) = mpsc::channel();
            u.stage = Some(Prepared {
                index: 0,
                stage: Box::new(Probe(tx)),
            });
            let exchange = u.exchange.clone();
            let handoff = exchange.handoff.lock().unwrap();
            let mut s = Settings {
                effect: Effect::Lpf.index(),
                beats: 0.25,
                level: 1.0,
                knob: 0.5,
                on: true,
                bpm: 120.0,
            };
            u.set(s);
            assert_eq!(u.process([0.4, -0.3]), [0.4, -0.3]);
            s.effect = Effect::Trans.index();
            s.on = false;
            u.set(s);
            s.on = final_on;
            u.set(s);
            assert_eq!(u.process([0.4, -0.3]), [0.4, -0.3]);
            drop(handoff);
            u.prepare(s.effect);
            u.set(s);
            let out: Vec<_> = (0..6000).map(|_| u.process([0.4, -0.3])).collect();
            if final_on {
                assert!(out[1000][0] > 0.39, "the latest gate opens");
                assert!(out[4500][0].abs() < 0.01, "the latest gate closes");
            } else {
                assert!(
                    out.iter().all(|&x| x == [0.4, -0.3]),
                    "pending OFF never activates"
                );
            }
            let destroyed_on = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_ne!(
                destroyed_on,
                thread::current().id(),
                "retired stages are destroyed by the worker"
            );
        }
    }

    /// Picking another effect while ECHO repeats ring cuts them neither when the selection is asked for (the new stage is
    /// still being built) nor when it arrives: the tail rings on, then fades out beside the new stage.
    #[test]
    fn a_switch_fades_a_ringing_tail_instead_of_cutting_it() {
        let mut u = Unit::new(48_000);
        let s = Settings {
            effect: Effect::Echo.index(),
            beats: 0.25, // a repeat every 6000 frames at 120 BPM
            level: 1.0,
            knob: 0.5,
            on: true,
            bpm: 120.0,
        };
        u.prepare(s.effect);
        u.set(s);
        let reverb = Settings {
            effect: Effect::Reverb.index(),
            on: false,
            ..s
        };
        let exchange = u.exchange.clone();
        let mut building = None;
        let mut out = vec![];
        for i in 0..12_000usize {
            match i {
                6_500 => u.set(Settings { on: false, ..s }),
                9_000 => {
                    building = Some(exchange.handoff.lock().unwrap()); // the worker cannot deliver the new stage yet
                    u.set(reverb);
                }
                9_500 => {
                    drop(building.take());
                    u.prepare(reverb.effect);
                    u.set(reverb);
                }
                _ => {}
            }
            // 6000 frames of a 100 Hz sine, then silence: its repeat peaks at frame 9000
            let x = if i < 6_000 {
                [(i as f32 * 0.01309).sin(); 2]
            } else {
                [0.0; 2]
            };
            out.push(u.process(x)[0]);
        }
        let worst = out[8_900..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(
            worst < 0.05,
            "the repeats vanish in one frame: step {worst:.3}"
        );
        let ringing = out[9_100..9_400].iter().fold(0.0f32, |m, y| m.max(y.abs()));
        assert!(
            ringing > 0.3,
            "the tail rings on while the new stage is built: {ringing:.3}"
        );
    }
}
