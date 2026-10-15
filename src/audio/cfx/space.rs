//! SPACE: a reverb send that rises with the knob. The reverb is the Beat FX one ([`crate::audio::fx::Reverb`],
//! Freeverb, public domain) fed `x × amount`, so turning the knob back to the centre stops the input and
//! the tail rings out. The wet return gets Freeverb's own `scalewet` gain of 3 and the dry passes
//! unchanged. Left of the centre the wet loses its lows (one-pole high-pass, 150 Hz up to 1.2 kHz with the
//! knob: a thin wash), right of it the wet is full-range ("hi"). The room size ([`ROOM_BEATS`]) and the
//! tone sweep are our own choices.

use super::{Ctx, Stage};
use crate::audio::fx::{self, Frame, Reverb, Stage as _};
use std::f32::consts::TAU;

/// Passed to the Beat FX reverb as its BEAT selector: a medium room (comb feedback 0.91).
const ROOM_BEATS: f32 = 2.0;
const WET: f32 = 3.0;
const LOW_CUT_HZ: (f32, f32) = (150.0, 1200.0);

pub struct Space {
    rev: Reverb,
    /// Low-pass state of the wet return, whose difference to the wet is the high-passed wash.
    lp: Frame,
    /// The knob was last left of the centre (kept while the tail rings out with the knob at the centre).
    left: bool,
}

impl Space {
    pub fn new(rate: u32) -> Space {
        Space {
            rev: Reverb::new(rate),
            lp: [0.0; 2],
            left: false,
        }
    }
}

impl Stage for Space {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let a = c.a();
        if a > 0.0 {
            self.left = c.x < 0.0;
        }
        let y = self.rev.tick(
            x,
            &fx::Ctx {
                rate: c.rate,
                period: 0.0,
                beats: ROOM_BEATS,
                level: WET,
                g: a,
                knob: 0.5,
            },
        );
        let mut out = x;
        let k = (TAU * (LOW_CUT_HZ.0 + (LOW_CUT_HZ.1 - LOW_CUT_HZ.0) * a) / c.rate).min(1.0);
        for i in 0..2 {
            let wet = y[i] - x[i];
            self.lp[i] += (wet - self.lp[i]) * k;
            out[i] += if self.left { wet - self.lp[i] } else { wet };
        }
        out
    }

    fn reset(&mut self) {
        self.rev.reset();
        (self.lp, self.left) = ([0.0; 2], false);
    }

    /// Longer than the gap between the comb delays' echoes (≤ 40 ms).
    fn hold(&self, _period: f32, rate: f32) -> u32 {
        (0.1 * rate) as u32
    }
}
