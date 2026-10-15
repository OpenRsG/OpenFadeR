//! ECHO and DELAY: a feedback delay line whose time is the selected beat fraction. DELAY repeats the
//! signal cleanly (feedback 0.4); ECHO feeds back through a one-pole low-pass, so each repeat is darker
//! and louder ones last longer (feedback 0.55) like a tape echo (classic feedback-delay
//! design). LEVEL is how loud the repeats are against the dry signal,
//! which always passes unchanged. The line is fed `x × g`: at OFF the input stops but the repeats keep
//! sounding until they fade (trails). Delay times beyond the buffer are folded ([`super::fold`]).

use super::{Ctx, Frame, Line, MAX_DELAY_S, Stage, fold};

pub struct Echo {
    line: Line,
    /// Low-pass state of the feedback path.
    lp: Frame,
    feedback: f32,
    /// One-pole coefficient, 0 = no filtering.
    damp: f32,
}

impl Echo {
    pub fn echo(rate: u32) -> Echo {
        Echo {
            line: Line::new(rate, MAX_DELAY_S),
            lp: [0.0; 2],
            feedback: 0.55,
            damp: 0.35,
        }
    }

    pub fn delay(rate: u32) -> Echo {
        Echo {
            feedback: 0.4,
            damp: 0.0,
            ..Echo::echo(rate)
        }
    }
}

impl Stage for Echo {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let tap = self.line.read(fold(c.period, self.line.max()));
        let mut fed = [0.0; 2];
        for k in 0..2 {
            self.lp[k] = tap[k] + (self.lp[k] - tap[k]) * self.damp;
            fed[k] = x[k] * c.g + self.lp[k] * self.feedback;
        }
        self.line.push(fed);
        [x[0] + tap[0] * c.level, x[1] + tap[1] * c.level]
    }

    fn reset(&mut self) {
        self.line.clear();
        self.lp = [0.0; 2];
    }
}
