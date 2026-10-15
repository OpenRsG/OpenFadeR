//! SWEEP: a band-pass sweep. The knob moves the band's centre exponentially (58 Hz at -1, 700 Hz just
//! off the centre … 8.4 kHz at +1) and narrows it (Q 0.8 → 3), so the sound thins and gates down to a
//! moving band; the first 1/6 of the travel fades the band in so it never jumps. Trapezoidal state-variable
//! filter (Zavalishin, "The Art of VA Filter Design"), whose coefficients move safely every sample. The
//! range and Q law are our own choices ("gate / band sweep").

use super::{Ctx, Stage};
use crate::audio::fx::Frame;
use std::f32::consts::PI;

const CENTRE_HZ: f32 = 700.0;
/// Centre frequency ratio per unit knob: `CENTRE_HZ × SPAN^x`.
const SPAN: f32 = 12.0;
const Q: (f32, f32) = (0.8, 2.2);
/// Knob amount at which the band is fully mixed in.
const FADE_IN: f32 = 1.0 / 6.0;

#[derive(Default)]
pub struct Sweep {
    ic1: Frame,
    ic2: Frame,
    /// Knob the coefficients were made for: `k` (1/Q), then the three SVF terms.
    at: f32,
    k: f32,
    a: [f32; 3],
}

impl Sweep {
    fn aim(&mut self, x: f32, rate: f32) {
        let fc = (CENTRE_HZ * SPAN.powf(x)).min(rate * 0.45);
        let g = (PI * fc / rate).tan();
        self.k = 1.0 / (Q.0 + Q.1 * x.abs());
        let a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a = [a1, g * a1, g * g * a1];
        self.at = x;
    }
}

impl Stage for Sweep {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        if c.x != self.at {
            self.aim(c.x, c.rate);
        }
        let [a1, a2, a3] = self.a;
        let mix = (c.a() / FADE_IN).min(1.0);
        let mut out = x;
        for i in 0..2 {
            let v3 = x[i] - self.ic2[i];
            let v1 = a1 * self.ic1[i] + a2 * v3;
            let v2 = self.ic2[i] + a2 * self.ic1[i] + a3 * v3;
            self.ic1[i] = 2.0 * v1 - self.ic1[i];
            self.ic2[i] = 2.0 * v2 - self.ic2[i];
            out[i] = x[i] + (self.k * v1 - x[i]) * mix;
        }
        out
    }

    fn reset(&mut self) {
        (self.ic1, self.ic2) = ([0.0; 2], [0.0; 2]);
    }
}
