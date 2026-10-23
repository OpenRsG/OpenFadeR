//! ROLL: when the unit goes ON the next beat-fraction's worth of input is recorded (and still heard),
//! then that slice repeats until OFF. LEVEL moves the output from the live signal to the loop. The slice
//! length is fixed at capture (a tempo change mid-roll does not stretch it; a BEAT step captures again at the new length)
//! and folded to the buffer ([`super::fold`]). ponytail: records forward from ON; the slice before ON (a stutter without the
//! first-period gap) would need the line fed while the unit is idle.

use super::{Ctx, Frame, MAX_DELAY_S, Stage, fold};

pub struct Roll {
    buf: Vec<Frame>,
    /// Slice length in frames, 0 = not decided yet (set by the first tick after ON).
    len: usize,
    pos: usize,
    looping: bool,
    /// The tempo at capture (frames per beat) and the BEAT fraction the slice was cut at.
    per_beat: f32,
    beats: f32,
}

impl Roll {
    pub fn new(rate: u32) -> Roll {
        Roll {
            buf: vec![[0.0; 2]; (rate as f32 * MAX_DELAY_S) as usize],
            len: 0,
            pos: 0,
            looping: false,
            per_beat: 0.0,
            beats: 0.0,
        }
    }

    /// The slice for `period` frames, folded to the buffer.
    fn slice(&self, period: f32) -> usize {
        (fold(period, self.buf.len() as f32).round() as usize).clamp(1, self.buf.len())
    }
}

impl Stage for Roll {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        if self.len == 0 {
            (self.per_beat, self.beats) = (c.period / c.beats, c.beats);
            self.len = self.slice(c.period);
        } else if c.beats != self.beats {
            // BEAT stepped while rolling: capture again at the new length (the period is still gliding there)
            self.beats = c.beats;
            self.len = self.slice(self.per_beat * c.beats);
            (self.pos, self.looping) = (0, false);
        }
        if !self.looping {
            self.buf[self.pos] = x;
            self.pos += 1;
            if self.pos == self.len {
                self.pos = 0;
                self.looping = true;
            }
            return x;
        }
        let w = self.buf[self.pos];
        self.pos = (self.pos + 1) % self.len;
        c.mix(x, w)
    }

    fn reset(&mut self) {
        self.len = 0;
        self.pos = 0;
        self.looping = false;
    }

    fn engage(&mut self) {
        self.reset();
    }
}
