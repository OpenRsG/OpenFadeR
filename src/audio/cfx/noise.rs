//! NOISE: white noise (xorshift32, one generator per channel) is added to the signal through a
//! one-pole filter the knob sweeps: left of the centre a low-pass from 20 kHz down to 300 Hz, right of it
//! a high-pass from 40 Hz up to 8 kHz. The noise is scaled so its RMS is `amount × `[`LEVEL`] whatever
//! the filter does (the one-pole's noise power gain is divided out), so the level follows the knob and
//! the knob's sweep only changes the colour. The dry passes unchanged. Range and level are our own
//! choices.

use super::{Ctx, Stage};
use crate::audio::fx::Frame;
use std::f32::consts::TAU;

/// RMS of the added noise at full knob (-14 dBFS).
const LEVEL: f32 = 0.2;
const LOW_PASS_HZ: (f32, f32) = (20_000.0, 300.0);
const HIGH_PASS_HZ: (f32, f32) = (40.0, 8_000.0);

pub struct Noise {
    seed: [u32; 2],
    /// One-pole low-pass state; the high-pass is the white noise minus it.
    lp: Frame,
    /// Knob the coefficients were made for, the pole, the output gain and the side.
    at: f32,
    k: f32,
    gain: f32,
    high: bool,
}

impl Default for Noise {
    fn default() -> Noise {
        Noise {
            seed: [0x9E37_79B9, 0x85EB_CA6B],
            lp: [0.0; 2],
            at: 0.0,
            k: 1.0,
            gain: 0.0,
            high: false,
        }
    }
}

impl Noise {
    fn aim(&mut self, x: f32, rate: f32) {
        let (a, (from, to)) = (x.abs(), if x < 0.0 { LOW_PASS_HZ } else { HIGH_PASS_HZ });
        let fc = (from * (to / from).powf(a)).min(rate * 0.45);
        let k = 1.0 - (-TAU * fc / rate).exp();
        let low = k / (2.0 - k); // power gain of white noise through the pole
        let power = if x < 0.0 { low } else { 1.0 - 2.0 * k + low };
        (self.at, self.k, self.high, self.gain) = (x, k, x > 0.0, LEVEL * a / power.sqrt());
    }

    /// Uniform in -√3..√3 (unit RMS).
    fn white(&mut self, ch: usize) -> f32 {
        let s = &mut self.seed[ch];
        *s ^= *s << 13;
        *s ^= *s >> 17;
        *s ^= *s << 5;
        ((*s as f32 / 2_147_483_648.0) - 1.0) * 3f32.sqrt()
    }
}

impl Stage for Noise {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        if c.x != self.at {
            self.aim(c.x, c.rate);
        }
        let mut out = x;
        for i in 0..2 {
            let w = self.white(i);
            self.lp[i] += (w - self.lp[i]) * self.k;
            out[i] += if self.high {
                w - self.lp[i]
            } else {
                self.lp[i]
            } * self.gain;
        }
        out
    }

    fn reset(&mut self) {
        self.lp = [0.0; 2];
    }
}
