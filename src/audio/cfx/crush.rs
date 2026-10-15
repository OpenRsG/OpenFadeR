//! CRUSH: bit-depth and sample-rate reduction. With the knob's amount `a` the signal is rounded to
//! `16 - 12a` bits (mid-tread, step `2^(1-bits)`: full knob = 4 bits, steps of 1/8) and held for
//! `1 + round(23a²)` frames (full knob = 24 frames, 2 kHz at 48 kHz). The first tenth of the travel
//! crossfades from the dry signal so the effect never jumps in. Both sides of the centre do the same.
//! The law is our own choice.

use super::{Ctx, Stage};
use crate::audio::fx::Frame;

const BITS: (f32, f32) = (16.0, 4.0);
const MAX_HOLD: f32 = 24.0;
/// Knob amount at which the crushed signal is fully mixed in.
const FADE_IN: f32 = 0.1;

#[derive(Default)]
pub struct Crush {
    held: Frame,
    /// Frames the held value still covers.
    left: u32,
    /// Knob the parameters were made for, the quantiser step and the hold in frames.
    at: f32,
    step: f32,
    hold: u32,
}

impl Stage for Crush {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let a = c.a();
        if c.x != self.at {
            self.at = c.x;
            self.step = (1.0 - (BITS.0 + (BITS.1 - BITS.0) * a)).exp2();
            self.hold = 1 + ((MAX_HOLD - 1.0) * a * a).round() as u32;
        }
        if self.left == 0 {
            self.held = x.map(|v| (v / self.step).round() * self.step);
            self.left = self.hold;
        }
        self.left -= 1;
        let mix = (a / FADE_IN).min(1.0);
        [0, 1].map(|i| x[i] * (1.0 - mix) + self.held[i] * mix)
    }

    fn reset(&mut self) {
        (self.held, self.left) = ([0.0; 2], 0);
    }
}
