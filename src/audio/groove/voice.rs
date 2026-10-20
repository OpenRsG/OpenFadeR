//! One deck's DRUM SWAP playback inside its channel's callback: hand-overs, presses, the beat-synced loops, the stop rules and the
//! loops' own DRUM RELEASE FX unit.
//!
//! `begin` runs once per buffer (UI requests, whether the deck can hold sync, gains), `tick` once per output frame with the deck's source
//! position (the loop phase is the deck's beat advance, see [`super::phase`]), `end` publishes what the UI shows. A pressed slot waits for the
//! next whole beat of the deck and then plays from the start of its loop; its loop phase is a plain beat counter that the grid moves, so a
//! quantized jump of the deck leaves it playing in time. At the source tempo (ratio within 0.1 %) the loop is read by linear
//! interpolation at the exact position, else by the WSOLA stretcher (pitch kept; its grain search moves the audio up to ~9 ms around the
//! nominal position, like the deck's own key lock). Stopping fades out over [`STOP_S`].

use super::super::fx::Unit;
use super::super::gain_at;
use super::super::stretch::Stretch;
use super::phase::Tracker;
use super::{Controls, Drum, SLOTS, Slot, State};
use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

/// Fade-out of a stopping loop, seconds (as long as the player's stem ramp, so the drums stem comes back as the loop goes).
const STOP_S: f32 = 0.01;
/// Playback ratios this close to 1 are read exactly.
const EXACT: f64 = 1e-3;

/// Whether the deck can keep the loops in sync this buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// Playing forward at its tempo: loops start and run.
    Held,
    /// Playing, but backwards, braked, spun up or scratched: every loop fades out and presses are ignored.
    Lost,
    /// Not playing (paused, loading): every loop is dropped at once.
    Stopped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Idle,
    Wait,
    Play,
    /// Fading out.
    Fade,
}

struct SlotVoice {
    drum: Option<Arc<Drum>>,
    st: St,
    /// Beats into the loop, `0..drum.beats`.
    phase: f64,
    /// Frames of fade left while `st` is [`St::Fade`].
    fade: u32,
    /// Gain at the end of the last buffer and the one this buffer ramps to.
    gain: f32,
    to: f32,
    presses: u32,
    stops: u32,
    stretch: Stretch,
}

impl SlotVoice {
    fn new() -> SlotVoice {
        SlotVoice {
            drum: None,
            st: St::Idle,
            phase: 0.0,
            fade: 0,
            gain: 1.0,
            to: 1.0,
            presses: 0,
            stops: 0,
            stretch: Stretch::new(),
        }
    }

    /// Takes a loop the UI handed over, if any.
    fn take(&mut self, s: &Slot) {
        if s.drum.take(&mut self.drum) {
            (self.st, self.fade) = (St::Idle, 0);
            self.stretch.reset();
        }
    }

    fn stop(&mut self, len: u32) {
        match self.st {
            St::Play => (self.st, self.fade) = (St::Fade, len),
            St::Wait => self.st = St::Idle,
            _ => {}
        }
    }

    fn on(&self) -> bool {
        matches!(self.st, St::Wait | St::Play)
    }
}

/// Audio-thread state of one deck's slots, allocated before the stream starts.
pub(crate) struct Voice {
    slots: [SlotVoice; SLOTS],
    t: Tracker,
    /// DRUM RELEASE FX on the playing loops (SINGLE mode).
    fx: Unit,
    halts: u32,
    n: usize,
    rate: u32,
    stop_len: u32,
    single: bool,
    /// The last beat advance per frame, for loops fading out after sync was lost.
    delta: f64,
}

impl Voice {
    pub(crate) fn new(rate: u32) -> Voice {
        Voice {
            slots: std::array::from_fn(|_| SlotVoice::new()),
            t: Tracker::default(),
            fx: Unit::new(rate),
            halts: 0,
            n: 1,
            rate,
            stop_len: 1,
            single: true,
            delta: 0.0,
        }
    }

    #[cfg(test)]
    pub(super) fn prepare_fx(&mut self, c: &Controls) {
        self.fx.prepare(c.release.settings().effect);
    }

    /// The buffer of `n` frames at `dev_rate`: takes hand-overs, presses and stops, applies the link state, sets the gains and the loops' FX.
    pub(crate) fn begin(&mut self, c: &Controls, link: Link, n: usize, dev_rate: u32) {
        (self.n, self.rate) = (n.max(1), dev_rate);
        self.stop_len = ((STOP_S * dev_rate as f32) as u32).max(1);
        let len = self.stop_len;
        let (multi, master) = (c.multi(), c.master());
        self.single = !multi;
        let halts = c.halts.load(Acquire);
        let halted = std::mem::replace(&mut self.halts, halts) != halts;
        for k in 0..SLOTS {
            let (s, v) = (&c.slots[k], &mut self.slots[k]);
            v.take(s);
            let (p, st) = (s.presses.load(Acquire), s.stops.load(Acquire));
            let (pressed, stopped) = (
                std::mem::replace(&mut v.presses, p) != p,
                std::mem::replace(&mut v.stops, st) != st,
            );
            if halted || stopped {
                v.stop(len);
            }
            if pressed && link == Link::Held {
                if v.on() {
                    v.stop(len);
                } else if v.drum.is_some() {
                    v.st = St::Wait;
                    // The twin slot of the other bank never plays together with this one; SINGLE mode lets one slot play at all.
                    let twin = (k + super::PER_BANK) % SLOTS;
                    for j in (0..SLOTS).filter(|&j| j != k && (!multi || j == twin)) {
                        self.slots[j].stop(len);
                    }
                }
            }
        }
        match link {
            Link::Held => {}
            Link::Lost => self.slots.iter_mut().for_each(|v| v.stop(len)),
            Link::Stopped => self
                .slots
                .iter_mut()
                .for_each(|v| (v.st, v.fade) = (St::Idle, 0)),
        }
        if link != Link::Held {
            self.t.reset();
        }
        for (k, v) in self.slots.iter_mut().enumerate() {
            v.to = c.gain(k) * master;
            if !matches!(v.st, St::Play | St::Fade) {
                v.gain = v.to; // a loop starts at its gain, no ramp up from the last one
            }
        }
        if self.single {
            self.fx.set(c.release.settings());
        } else {
            self.fx.release();
        }
    }

    /// The next output frame: `grid` = (beat times in track ms, track sample rate), `pos` the deck's source position of this frame, `kr`
    /// source frames per output frame, `i` the frame's index in the buffer. The loops' sum, through the release unit; silence while nothing plays.
    pub(crate) fn tick(
        &mut self,
        grid: Option<(&[u32], f64)>,
        pos: f64,
        kr: f64,
        i: usize,
    ) -> [f32; 2] {
        if self.fx.idle() && self.slots.iter().all(|v| v.st == St::Idle) {
            self.t.reset(); // nothing to keep in time: the grid is not walked
            return [0.0; 2];
        }
        let tk = grid.and_then(|(b, r)| self.t.step(b, r, pos));
        let len = self.stop_len;
        let tk = match tk {
            Some(t) if !t.lost => Some(t),
            lost => {
                if lost.is_some() {
                    self.t.reset();
                }
                self.slots.iter_mut().for_each(|v| v.stop(len)); // no grid or off it: nothing can stay in time
                None
            }
        };
        let delta = tk.map_or(self.delta, |t| t.delta);
        self.delta = delta;
        let mut out = [0f32; 2];
        for v in &mut self.slots {
            let Some(d) = v.drum.as_deref().filter(|_| v.st != St::Idle) else {
                continue;
            };
            let beats = d.beats as f64;
            match (v.st, tk) {
                (St::Wait, Some(t)) if t.crossed => {
                    v.st = St::Play;
                    v.phase = t.beat.rem_euclid(1.0);
                    v.stretch.reset();
                }
                (St::Wait, _) => continue,
                _ => v.phase = (v.phase + delta).rem_euclid(beats),
            }
            let spb = d.frames_per_beat();
            let per_frame = tk.map_or(delta, |t| t.dens * kr);
            let ratio = per_frame * spb * self.rate as f64 / d.pcm.rate as f64;
            let at = v.phase * spb;
            let x = if (ratio - 1.0).abs() < EXACT {
                v.stretch.reset();
                d.pcm.frame_at(at).unwrap_or([0.0; 2])
            } else {
                v.stretch.next(
                    &d.pcm,
                    at,
                    d.pcm.rate as f64 / self.rate as f64,
                    Some((0.0, beats * spb)),
                )
            };
            let mut g = gain_at(v.gain, v.to, i, self.n);
            if v.st == St::Fade {
                g *= v.fade as f32 / len as f32;
                v.fade -= 1;
                if v.fade == 0 {
                    v.st = St::Idle;
                }
            }
            out[0] += x[0] * g;
            out[1] += x[1] * g;
        }
        if self.single && !self.fx.idle() {
            out = self.fx.process(out);
        }
        out
    }

    /// The buffer is done: what the UI shows, and whether a SINGLE-mode loop has taken the drums.
    pub(crate) fn end(&mut self, c: &Controls) {
        for (v, s) in self.slots.iter_mut().zip(&c.slots) {
            v.gain = v.to;
            let state = match v.st {
                St::Idle | St::Fade => State::Idle,
                St::Wait => State::Waiting,
                St::Play => State::Playing,
            };
            s.state.store(state as u8, Relaxed);
            let at = v
                .drum
                .as_ref()
                .map_or(0.0, |d| (v.phase / d.beats as f64) as f32);
            s.at.store(at.to_bits(), Relaxed);
        }
        c.swapping.store(
            self.single && self.slots.iter().any(|v| v.st == St::Play),
            Release,
        );
    }
}
