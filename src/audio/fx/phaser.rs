//! PHASER: four first-order allpass sections `H(z) = (a + z⁻¹) / (1 + a·z⁻¹)`, `a = (t - 1) / (t + 1)`,
//! `t = tan(π·fc/fs)` (Julius O. Smith, Physical Audio Signal Processing, "Phasing"), whose corner `fc`
//! sweeps 250 Hz to 3 kHz exponentially under a sine LFO with the selected beat fraction as its period.
//! LEVEL moves the output from dry to the allpass chain (0.5 gives the two moving notches). The LFO
//! restarts when the unit goes ON. No feedback.

use super::{Ctx, Frame, Lfo, Stage, sweep};

const LOW_HZ: f32 = 250.0;
const RATIO: f32 = 12.0;
const SECTIONS: usize = 4;

#[derive(Default)]
pub struct Phaser {
    lfo: Lfo,
    z: [[f32; SECTIONS]; 2],
}

impl Stage for Phaser {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let fc = LOW_HZ * RATIO.powf(sweep(self.lfo.tick(c.period)));
        let t = (std::f32::consts::PI * fc / c.rate).tan();
        let a = (t - 1.0) / (t + 1.0);
        let mut w = x;
        for (y, z) in w.iter_mut().zip(&mut self.z) {
            for s in z {
                let o = a * *y + *s;
                *s = *y - a * o;
                *y = o;
            }
        }
        c.mix(x, w)
    }

    fn reset(&mut self) {
        *self = Phaser::default();
    }

    fn engage(&mut self) {
        self.lfo.reset();
    }
}
