//! DUB ECHO: a feedback delay of one beat (the deck's effective tempo). The knob sets the send (what is
//! fed into the line, so the first repeat is `amount` loud) and the feedback (0.5 at the centre up to
//! 0.85), the repeats get darker through a one-pole low-pass in the feedback path like a tape echo
//! (inferred: classic feedback-delay designs). The dry passes unchanged. Back at the centre the input
//! stops and the repeats decay at feedback 0.5 until silent. Beats longer than the line (below 30 BPM)
//! are halved until they fit ([`crate::audio::fx::fold`]).

use super::{Ctx, Stage};
use crate::audio::fx::{Frame, Line, fold};

const MAX_S: f32 = 2.0;
/// Feedback-path low-pass: 0 = open, towards 1 = darker.
const DAMP: f32 = 0.35;
const FEEDBACK: (f32, f32) = (0.5, 0.35);

pub struct Dub {
    line: Line,
    lp: Frame,
}

impl Dub {
    pub fn new(rate: u32) -> Dub {
        Dub { line: Line::new(rate, MAX_S), lp: [0.0; 2] }
    }
}

impl Stage for Dub {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let a = c.a();
        let tap = self.line.read(fold(c.period, self.line.max()));
        let mut fed = [0.0; 2];
        for i in 0..2 {
            self.lp[i] = tap[i] + (self.lp[i] - tap[i]) * DAMP;
            fed[i] = x[i] * a + self.lp[i] * (FEEDBACK.0 + FEEDBACK.1 * a);
        }
        self.line.push(fed);
        [x[0] + tap[0], x[1] + tap[1]]
    }

    fn reset(&mut self) {
        self.line.clear();
        self.lp = [0.0; 2];
    }

    /// One period (the longest gap between repeats) plus 50 ms.
    fn hold(&self, period: f32, rate: f32) -> u32 {
        (fold(period, self.line.max()) + 0.05 * rate) as u32
    }
}
