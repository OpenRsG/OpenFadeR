//! LPF / HPF: a resonant (Q = 2) state-variable filter ([`Svf`], the SDK's, which plugins share) whose cutoff sweeps
//! exponentially once per beat-fraction period: the LPF closes from 18 kHz to 150 Hz at mid-period and opens again, the
//! HPF opens from 30 Hz to 8 kHz and back (the sweep ranges are this port's choice). LEVEL moves the output from dry to
//! the filtered signal. The sweep restarts, fully open, when the unit goes ON.

use super::{Ctx, Frame, Lfo, Stage, sweep};
use openfader_plugin::dsp::Svf;

const Q: f32 = 2.0;

pub struct Filter {
    high: bool,
    lfo: Lfo,
    svf: Svf,
}

impl Filter {
    pub fn new(high: bool) -> Filter {
        Filter {
            high,
            lfo: Lfo::default(),
            svf: Svf::default(),
        }
    }
}

impl Stage for Filter {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let m = sweep(self.lfo.tick(c.period));
        let (from, to) = if self.high {
            (30.0f32, 8000.0f32)
        } else {
            (18_000.0, 150.0)
        };
        let b = self.svf.tick(x, from * (to / from).powf(m), Q, c.rate);
        c.mix(x, if self.high { b.high } else { b.low })
    }

    fn reset(&mut self) {
        self.svf.clear();
        self.lfo.reset();
    }

    fn engage(&mut self) {
        self.lfo.reset();
    }
}
