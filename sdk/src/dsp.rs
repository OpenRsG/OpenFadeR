//! DSP building blocks the host's Beat FX and the bundled plugins share: delay [`Line`], phase [`Lfo`], [`sweep`],
//! [`fold`], [`ramp`], one-pole filters ([`OnePole`]), a state-variable filter ([`Svf`]), a two-tap pitch [`Shifter`]
//! and the Freeverb [`Reverb`]. Everything allocates in its constructor and never on `tick`, so it is safe on the audio
//! thread. Stereo state is `[f32; 2]` ([`Frame`]).

use crate::{Ctx, Frame};
use std::f32::consts::PI;
use std::f64::consts::TAU;

/// `x` moved towards `want` by at most `step` (the engage and fade ramps, one frame at a time).
pub fn ramp(x: f32, want: f32, step: f32) -> f32 {
    if (want - x).abs() <= step {
        want
    } else {
        x + step.copysign(want - x)
    }
}

/// `p` halved until it is at most `max` samples (and at least 1).
pub fn fold(mut p: f32, max: f32) -> f32 {
    while p > max {
        p *= 0.5;
    }
    p.max(1.0)
}

/// Delay line of stereo frames with linearly interpolated reads.
pub struct Line {
    buf: Vec<Frame>,
    head: usize,
}

impl Line {
    pub fn new(rate: u32, seconds: f32) -> Line {
        Line {
            buf: vec![[0.0; 2]; ((rate as f32 * seconds) as usize).max(4)],
            head: 0,
        }
    }

    /// Longest readable delay in samples.
    pub fn max(&self) -> f32 {
        (self.buf.len() - 2) as f32
    }

    pub fn push(&mut self, x: Frame) {
        self.buf[self.head] = x;
        self.head = (self.head + 1) % self.buf.len();
    }

    /// The frame pushed `d` frames ago (1 = the last push), clamped to `1..=max`.
    pub fn read(&self, d: f32) -> Frame {
        let d = d.clamp(1.0, self.max());
        let (i, f) = (d as usize, d.fract());
        let n = self.buf.len();
        let (a, b) = (
            self.buf[(self.head + n - i) % n],
            self.buf[(self.head + n - i - 1) % n],
        );
        [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
    }

    pub fn clear(&mut self) {
        self.buf.fill([0.0; 2]);
    }
}

/// Phase accumulator, 0..1 per period (f64: no drift over hours of playback).
#[derive(Default)]
pub struct Lfo(f64);

impl Lfo {
    /// The current phase, then advances by one sample of a `period`-sample cycle.
    pub fn tick(&mut self, period: f32) -> f32 {
        let p = self.0;
        self.0 += 1.0 / period.max(1.0) as f64;
        if self.0 >= 1.0 {
            self.0 -= 1.0;
        }
        p as f32
    }

    pub fn reset(&mut self) {
        self.0 = 0.0;
    }
}

/// `0` at phase 0, `1` at phase ½, back to `0`: sine-shaped sweep position.
pub fn sweep(phase: f32) -> f32 {
    0.5 - 0.5 * (phase as f64 * TAU).cos() as f32
}

/// [`OnePole`] coefficient for a corner at `fc` Hz.
pub fn pole(fc: f32, rate: f32) -> f32 {
    1.0 - (-std::f32::consts::TAU * fc / rate).exp()
}

/// One-pole filter (6 dB per octave) with the state of both channels; the coefficient comes from [`pole`] per call, so
/// it may sweep.
#[derive(Default)]
pub struct OnePole(Frame);

impl OnePole {
    /// Low-passed `x`.
    pub fn low(&mut self, x: Frame, a: f32) -> Frame {
        for (z, x) in self.0.iter_mut().zip(x) {
            *z += (x - *z) * a;
        }
        self.0
    }

    /// High-passed `x` (`x` minus [`OnePole::low`]).
    pub fn high(&mut self, x: Frame, a: f32) -> Frame {
        let l = self.low(x, a);
        [x[0] - l[0], x[1] - l[1]]
    }

    pub fn clear(&mut self) {
        self.0 = [0.0; 2];
    }
}

/// The three outputs of an [`Svf`] for one frame.
pub struct Bands {
    pub low: Frame,
    /// Band-pass normalised to unity gain at the centre.
    pub band: Frame,
    pub high: Frame,
}

/// Resonant state-variable filter, zero-delay-feedback form (Zavalishin, The Art of VA Filter Design, §3), the one the
/// host's LPF / HPF sweep uses. Cutoff and Q may change on every frame.
#[derive(Default)]
pub struct Svf {
    /// Integrator states per channel.
    s: [[f32; 2]; 2],
}

impl Svf {
    /// Filters `x` with cutoff `fc` (Hz, kept below 0.45 × `rate`) and quality `q` (0.707 = Butterworth).
    pub fn tick(&mut self, x: Frame, fc: f32, q: f32, rate: f32) -> Bands {
        let k = 1.0 / q;
        let g = (PI * fc.clamp(1.0, 0.45 * rate) / rate).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        let (a2, a3) = (g * a1, g * g * a1);
        let mut b = Bands {
            low: [0.0; 2],
            band: [0.0; 2],
            high: [0.0; 2],
        };
        for ((s, x), ch) in self.s.iter_mut().zip(x).zip(0..2) {
            let v3 = x - s[1];
            let v1 = a1 * s[0] + a2 * v3;
            let v2 = s[1] + a2 * s[0] + a3 * v3;
            s[0] = 2.0 * v1 - s[0];
            s[1] = 2.0 * v2 - s[1];
            (b.low[ch], b.band[ch], b.high[ch]) = (v2, k * v1, x - k * v1 - v2);
        }
        b
    }

    pub fn clear(&mut self) {
        self.s = [[0.0; 2]; 2];
    }
}

/// Pitch shifter of two delay taps that slide against the write head and crossfade (sin² / cos², so they sum to
/// one): the classic delay-line harmonizer. Inferred design; the window trades latency (about half of it) against
/// warble. At a ratio of exactly 1 the taps do not move and comb the signal: bypass instead.
pub struct Shifter {
    line: Line,
    /// Tap position, 0..1 of the window.
    phase: f32,
    /// Window in samples.
    win: f32,
}

impl Shifter {
    /// A shifter with a `window_s` second window (0.03 to 0.06 suits voices, 0.5 or more gives a tape-slowing effect).
    pub fn new(rate: u32, window_s: f32) -> Shifter {
        Shifter {
            line: Line::new(rate, window_s + 0.01),
            phase: 0.0,
            win: window_s * rate as f32,
        }
    }

    /// `x` shifted by the frequency `ratio` (2 = an octave up, 0.5 = down).
    pub fn tick(&mut self, x: Frame, ratio: f32) -> Frame {
        self.line.push(x);
        self.phase = (self.phase + (1.0 - ratio) / self.win).rem_euclid(1.0);
        let (a, b) = (self.phase, (self.phase + 0.5).fract());
        let (ta, tb) = (
            self.line.read(1.0 + a * self.win),
            self.line.read(1.0 + b * self.win),
        );
        let wa = (a * PI).sin().powi(2);
        [
            ta[0] * wa + tb[0] * (1.0 - wa),
            ta[1] * wa + tb[1] * (1.0 - wa),
        ]
    }

    pub fn clear(&mut self) {
        self.line.clear();
        self.phase = 0.0;
    }
}

/// Freeverb's allpass of one channel (feedback 0.5), a smearing diffuser. Its gain is not 1: it peaks at 5/3 (at half
/// the sampling rate over the delay length), so keep it out of feedback loops that must stay below unity.
pub struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

impl Allpass {
    pub fn new(len: usize) -> Allpass {
        Allpass {
            buf: vec![0.0; len.max(1)],
            i: 0,
        }
    }

    pub fn tick(&mut self, x: f32) -> f32 {
        let b = self.buf[self.i];
        self.buf[self.i] = x + b * ALLPASS_FEEDBACK;
        self.i = (self.i + 1) % self.buf.len();
        b - x
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;
const DAMP: f32 = 0.2;
const INPUT_GAIN: f32 = 0.015;
const ALLPASS_FEEDBACK: f32 = 0.5;

/// Freeverb's output scale for the wet signal (the original code's `scalewet`); [`Reverb::tick`] leaves it out, a
/// standalone reverb multiplies [`Reverb::wet`] by it.
pub const WET: f32 = 3.0;

struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
}

impl Comb {
    fn tick(&mut self, x: f32, feedback: f32) -> f32 {
        let out = self.buf[self.i];
        self.store = out * (1.0 - DAMP) + self.store * DAMP;
        self.buf[self.i] = x + self.store * feedback;
        self.i = (self.i + 1) % self.buf.len();
        out
    }
}

/// Freeverb (Jezar at Dreampoint, public domain, 2000): per channel 8 parallel low-pass-feedback comb filters into 4
/// series allpasses (feedback 0.5); the right channel's delays are 23 samples longer for stereo width. The tunings (in
/// samples at 44.1 kHz, scaled to the stream's rate) and the 0.015 input gain are Freeverb's.
pub struct Reverb {
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    beats: f32,
    feedback: f32,
}

impl Reverb {
    pub fn new(rate: u32) -> Reverb {
        let len = |n: usize, ch: usize| {
            (((n + ch * SPREAD) as f32 * rate as f32 / 44_100.0) as usize).max(1)
        };
        Reverb {
            combs: [0, 1].map(|ch| {
                COMBS
                    .iter()
                    .map(|&n| Comb {
                        buf: vec![0.0; len(n, ch)],
                        i: 0,
                        store: 0.0,
                    })
                    .collect()
            }),
            allpasses: [0, 1].map(|ch| {
                ALLPASSES
                    .iter()
                    .map(|&n| Allpass::new(len(n, ch)))
                    .collect()
            }),
            beats: 0.0,
            feedback: 0.0,
        }
    }

    /// Comb feedback of a room size `room` (0 to 1; Freeverb's `0.7 + 0.28 × room`): the longer, the longer the tail.
    pub fn feedback(room: f32) -> f32 {
        0.7 + 0.28 * room
    }

    /// The reverb's output for the input `(x[0] + x[1]) × 0.015 × g` (no dry signal), comb feedback `feedback`.
    pub fn wet(&mut self, x: Frame, feedback: f32, g: f32) -> Frame {
        let input = (x[0] + x[1]) * INPUT_GAIN * g;
        let mut wet = [0.0; 2];
        for ch in 0..2 {
            let mut o: f32 = self.combs[ch]
                .iter_mut()
                .map(|cb| cb.tick(input, feedback))
                .sum();
            for a in &mut self.allpasses[ch] {
                o = a.tick(o);
            }
            wet[ch] = o;
        }
        wet
    }

    /// The host's Beat FX REVERB for one frame: `x` plus the tail times LEVEL, the room from the beat fraction
    /// (1/8 beat = small room, 16 = long tail: `room = 0.6 + 0.04 × (log2 beats + 3)`, capped at 0.9; this mapping
    /// is the port's choice). Fed `g`: at OFF the tail rings out.
    pub fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        if c.beats != self.beats {
            self.beats = c.beats;
            self.feedback = Self::feedback((0.6 + 0.04 * (c.beats.log2() + 3.0)).min(0.9));
        }
        let wet = self.wet(x, self.feedback, c.g);
        [x[0] + wet[0] * c.level, x[1] + wet[1] * c.level]
    }

    pub fn clear(&mut self) {
        for cb in self.combs.iter_mut().flatten() {
            cb.buf.fill(0.0);
            cb.store = 0.0;
        }
        for a in self.allpasses.iter_mut().flatten() {
            a.clear();
        }
    }
}
