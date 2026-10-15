//! TRANS: a transformer gate. The signal is open for the first half of each beat-fraction period and
//! cut for the second; the edges are smoothed with a one-pole slew (1 ms) so the chop does not click.
//! LEVEL is the depth: 1 cuts completely, 0.5 ducks by half. The gate restarts open when the unit goes ON
//! (a plain 50 % gate).

use super::{Ctx, Frame, Lfo, Stage};

const SLEW_S: f32 = 0.001;

#[derive(Default)]
pub struct Trans {
    lfo: Lfo,
    env: f32,
}

impl Stage for Trans {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let open = if self.lfo.tick(c.period) < 0.5 {
            1.0
        } else {
            0.0
        };
        self.env += (open - self.env) / (SLEW_S * c.rate);
        c.mix(x, [x[0] * self.env, x[1] * self.env])
    }

    fn reset(&mut self) {
        *self = Trans::default();
    }

    fn engage(&mut self) {
        self.lfo.reset();
    }
}
