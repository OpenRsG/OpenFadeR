//! BITCRUSH: bit-depth and sample-rate reduction that sweeps with the beat. Over one period of the selected beat
//! fraction the depth goes from 12 bits down to 4 and back while the hold time (sample-rate reduction) grows from
//! 1 to 16 samples. The sweep restarts when the unit is switched ON. LEVEL is the wet amount.

use openfader_plugin::{Ctx, Effect, Frame, export_effects};
use std::f32::consts::TAU;
use std::ffi::CStr;

struct Bitcrush {
    /// 0..1 through one period.
    phase: f32,
    /// Frames left on the held sample, and that sample.
    left: f32,
    held: Frame,
}

impl Effect for Bitcrush {
    const NAME: &'static CStr = c"BITCRUSH";

    fn new(_rate: u32) -> Bitcrush {
        Bitcrush {
            phase: 0.0,
            left: 0.0,
            held: [0.0; 2],
        }
    }

    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        self.phase = (self.phase + 1.0 / c.period.max(1.0)).fract();
        // 0 = most crushed (phase 0), 1 = mildest (half a period).
        let s = 0.5 - 0.5 * (self.phase * TAU).cos();
        if self.left <= 0.0 {
            let bits = 4.0 + 8.0 * s;
            let step = (1.0 - bits).exp2();
            self.held = x.map(|v| (v / step).round() * step);
            self.left += 1.0 + 15.0 * (1.0 - s);
        }
        self.left -= 1.0;
        c.mix(x, self.held)
    }

    fn reset(&mut self) {
        *self = Bitcrush::new(0);
    }

    fn engage(&mut self) {
        self.phase = 0.0;
    }
}

export_effects!(Bitcrush);
