//! FLANGER: a short feedback delay (0.5 to 7 ms) swept by a sine LFO whose period is the selected beat
//! fraction; LEVEL = how far the output moves from dry to the delayed signal (at 0.5 the classic
//! `½ dry + ½ delayed` comb). Feedback 0.5. The LFO restarts when the unit goes ON, with the shortest
//! delay (the textbook flanger).

use super::{Ctx, Frame, Lfo, Line, Stage, sweep};

const MIN_S: f32 = 0.0005;
const SPAN_S: f32 = 0.0065;
const FEEDBACK: f32 = 0.5;

pub struct Flanger {
    line: Line,
    lfo: Lfo,
}

impl Flanger {
    pub fn new(rate: u32) -> Flanger {
        Flanger {
            line: Line::new(rate, MIN_S + SPAN_S + 0.002),
            lfo: Lfo::default(),
        }
    }
}

impl Stage for Flanger {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let d = (MIN_S + SPAN_S * sweep(self.lfo.tick(c.period))) * c.rate;
        let tap = self.line.read(d);
        self.line
            .push([x[0] + tap[0] * FEEDBACK, x[1] + tap[1] * FEEDBACK]);
        c.mix(x, tap)
    }

    fn reset(&mut self) {
        self.line.clear();
        self.lfo.reset();
    }

    fn engage(&mut self) {
        self.lfo.reset();
    }
}
