//! One slot's playback: slice hand-over, press / gate / loop state machine, tempo sync via WSOLA.
//!
//! The position runs on [`advance`] like a deck's clock (a loop is the whole slice, so it wraps
//! sample-accurately, also across buffers). At ratio 1 the slice is read by [`Pcm::frame_at`]; with SYNC
//! and a known tempo the ratio is `master tempo / slice tempo` (clamped to the decks' 0.5..2) and the
//! audio comes from [`Stretch`] (pitch kept). Stopping (second loop press, gate release, new slice) fades
//! out over [`RELEASE`] frames; the end of a one-shot just ends.

use super::super::stretch::Stretch;
use super::super::{Loop, Pcm, advance, gain_at};
use super::{Slot, load_f32};
use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed};

/// Fade-out length of a stop, output frames (~1.3 ms at 48 kHz).
pub(super) const RELEASE: u32 = 64;

pub(super) struct Voice {
    pcm: Option<Arc<Pcm>>,
    /// Source-frame position of the next output frame.
    pos: f64,
    on: bool,
    /// Frames of fade-out left once a stop was asked for; 0 = not stopping.
    fade: u32,
    /// Presses and stops already acted on.
    seen: u32,
    seen_stops: u32,
    gain: f32,
    /// The sequencer asked for a restart ([`Voice::fire`]); acted on at the next [`Voice::add`].
    fired: bool,
    stretch: Stretch,
}

impl Voice {
    pub(super) fn new() -> Voice {
        Voice {
            pcm: None,
            pos: 0.0,
            on: false,
            fade: 0,
            seen: 0,
            seen_stops: 0,
            gain: 1.0,
            fired: false,
            stretch: Stretch::new(),
        }
    }

    /// The sequencer's trigger: restarts the slice from its first frame at the next `add`, like a one-shot press (a loop slot restarts too, it never toggles).
    pub(super) fn fire(&mut self) {
        self.fired = true;
    }
    /// Takes a slice the UI handed over, if any.
    fn take(&mut self, s: &Slot) {
        if s.slice.take(&mut self.pcm) {
            (self.on, self.fade) = (false, 0);
            self.stretch.reset();
        }
    }

    /// Adds this slot's next `buf.len()` frames into `buf`.
    pub(super) fn add(
        &mut self,
        s: &Slot,
        master: f32,
        cue: bool,
        tempo: f32,
        dev_rate: u32,
        buf: &mut [([f32; 2], [f32; 2])],
    ) {
        self.take(s);
        let frames = self.pcm.as_ref().map_or(0, |p| p.samples.len() / 2);
        let (looped, gate) = (s.looped.load(Relaxed), s.gate.load(Relaxed));
        let t = s.trig.load(Acquire);
        let pressed = t != self.seen;
        let fired = std::mem::take(&mut self.fired);
        let started = pressed || fired;
        if started {
            self.seen = t;
            if pressed && !fired && frames > 0 && looped && !gate && self.on && self.fade == 0 {
                self.fade = RELEASE; // second press of a loop stops it
            } else {
                (self.on, self.pos, self.fade) = (frames > 0, 0.0, 0);
                self.stretch.reset();
            }
        } else if self.on && gate && self.fade == 0 && !s.held.load(Acquire) {
            self.fade = RELEASE;
        }
        let stops = s.stops.load(Acquire);
        if stops != self.seen_stops {
            self.seen_stops = stops;
            if self.on && self.fade == 0 {
                self.fade = RELEASE;
            }
        }
        let Some(pcm) = self.pcm.as_deref().filter(|_| self.on) else {
            s.playing.store(false, Relaxed);
            return;
        };
        let step = pcm.rate as f64 / dev_rate as f64;
        let bpm = load_f32(&s.bpm);
        let ratio = if s.sync.load(Relaxed) && bpm > 0.0 && tempo > 0.0 {
            (tempo / bpm).clamp(0.5, 2.0) as f64
        } else {
            1.0
        };
        let stretched = (ratio - 1.0).abs() > 1e-4;
        if !stretched {
            self.stretch.reset();
        }
        let lp: Loop = looped.then_some((0.0, frames as f64));
        let g1 = load_f32(&s.volume) * master;
        let g0 = if started { g1 } else { self.gain }; // a fresh start begins at its gain, no ramp up from the last one
        self.gain = g1;
        let n = buf.len();
        for (i, out) in buf.iter_mut().enumerate() {
            let fr = if !stretched {
                pcm.frame_at(self.pos)
            } else if self.pos < frames as f64 {
                Some(self.stretch.next(pcm, self.pos, step, lp))
            } else {
                None
            };
            let Some([l, r]) = fr else {
                self.on = false; // one-shot reached its end
                break;
            };
            let mut g = gain_at(g0, g1, i, n);
            if self.fade > 0 {
                g *= self.fade as f32 / RELEASE as f32;
                self.fade -= 1;
                self.on = self.fade > 0;
            }
            out.0[0] += l * g;
            out.0[1] += r * g;
            if cue {
                out.1[0] += l * g;
                out.1[1] += r * g;
            }
            self.pos = advance(self.pos, ratio * step, lp);
            if !self.on {
                break;
            }
        }
        s.playing.store(self.on, Relaxed);
    }
}
