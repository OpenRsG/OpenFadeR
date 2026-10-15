//! The time-domain blocks: `delay`, `allpass`, `phaser`, `freeverb`, `pitch`. State comes from the SDK's DSP helpers.

use super::{Args, Block, Chain, Expr, Frame, MAX_FEEDBACK, R, Vars, add, scale};
use openfader_plugin::dsp::{Line, Reverb, Shifter, WET, fold};
use std::f32::consts::PI;

/// `delay`: the signal `time` seconds late; only the delayed signal comes out. Longer times are halved until they fit
/// the line (`max` seconds), which keeps a beat delay on the beat. With a `feedback` (a formula, limited to ±0.99) the
/// delayed signal runs through the blocks of `loop` (a damping filter, saturation), is scaled by `feedback` and goes
/// back into the line: repeats, each `time` after the last.
pub struct Delay {
    time: Expr,
    feedback: Expr,
    back: Chain,
    line: Line,
    rate: f32,
}

impl Delay {
    pub fn parse(a: &mut Args) -> R<Delay> {
        Ok(Delay {
            time: a.expr("time", "0.25")?,
            feedback: a.expr("feedback", "0")?,
            back: a.blocks("loop")?,
            line: Line::new(a.rate, a.num("max", 4.0, 0.01, 30.0)?),
            rate: a.rate as f32,
        })
    }
}

impl Block for Delay {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let tap = self
            .line
            .read(fold(self.time.eval(v) * self.rate, self.line.max()));
        let fb = self.feedback.eval(v).clamp(-MAX_FEEDBACK, MAX_FEEDBACK);
        self.line.push(add(x, scale(self.back.tick(tap, v), fb)));
        tap
    }

    fn reset(&mut self) {
        self.line.clear();
        self.back.reset();
    }

    fn engage(&mut self) {
        self.back.engage();
    }
}

/// One channel of a unity-gain Schroeder allpass.
struct Diffuser {
    buf: Vec<f32>,
    i: usize,
    g: f32,
}

impl Diffuser {
    fn new(len: usize, g: f32) -> Diffuser {
        Diffuser {
            buf: vec![0.0; len.max(1)],
            i: 0,
            g,
        }
    }

    fn tick(&mut self, x: f32) -> f32 {
        let d = self.buf[self.i];
        let v = x - self.g * d;
        self.buf[self.i] = v;
        self.i = (self.i + 1) % self.buf.len();
        self.g * v + d
    }
}

/// `allpass`: a Schroeder allpass of `time` seconds (unit gain at every frequency, so it is safe inside a feedback loop;
/// Freeverb's own allpass peaks at 5/3). `spread` makes the right channel's longer by that share, for width.
pub struct Allpass([Diffuser; 2]);

impl Allpass {
    pub fn parse(a: &mut Args) -> R<Allpass> {
        let time = a.num("time", 0.005, 0.0001, 1.0)?;
        let g = a.num("gain", 0.5, -0.99, 0.99)?;
        let spread = a.num("spread", 0.0, 0.0, 1.0)?;
        let len = |ch: f32| (time * a.rate as f32 * (1.0 + spread * ch)) as usize;
        Ok(Allpass([
            Diffuser::new(len(0.0), g),
            Diffuser::new(len(1.0), g),
        ]))
    }
}

impl Block for Allpass {
    fn tick(&mut self, x: Frame, _: &Vars) -> Frame {
        [self.0[0].tick(x[0]), self.0[1].tick(x[1])]
    }

    fn reset(&mut self) {
        for d in &mut self.0 {
            d.buf.fill(0.0);
        }
    }
}

/// `phaser`: the mean of the signal and `stages` first-order allpasses at `hz` (sweep `hz` with an LFO), with `feedback`
/// around the allpasses: notches that move with `hz`.
pub struct Phaser {
    hz: Expr,
    feedback: f32,
    rate: f32,
    /// `[stage][channel]`: previous input and output.
    state: Vec<[[f32; 2]; 2]>,
    last: Frame,
}

impl Phaser {
    pub fn parse(a: &mut Args) -> R<Phaser> {
        Ok(Phaser {
            hz: a.expr("hz", "1000")?,
            feedback: a.num("feedback", 0.0, -0.95, 0.95)?,
            state: vec![[[0.0; 2]; 2]; a.num("stages", 4.0, 1.0, 12.0)? as usize],
            rate: a.rate as f32,
            last: [0.0; 2],
        })
    }
}

impl Block for Phaser {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let t = (PI * self.hz.eval(v).clamp(1.0, 0.45 * self.rate) / self.rate).tan();
        let a = (t - 1.0) / (t + 1.0);
        let mut y = [
            x[0] + self.last[0] * self.feedback,
            x[1] + self.last[1] * self.feedback,
        ];
        for st in &mut self.state {
            for ch in 0..2 {
                let out = a * y[ch] + st[ch][0] - a * st[ch][1];
                st[ch] = [y[ch], out];
                y[ch] = out;
            }
        }
        self.last = y;
        [(x[0] + y[0]) * 0.5, (x[1] + y[1]) * 0.5]
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = [[0.0; 2]; 2]);
        self.last = [0.0; 2];
    }
}

/// `freeverb`: Freeverb's tail only (no dry signal, at Freeverb's own wet gain). `size`, 0 to 1, sets the comb feedback
/// (0.7 + 0.28 × size): the longer, the longer the tail.
pub struct Freeverb {
    size: Expr,
    reverb: Reverb,
}

impl Freeverb {
    pub fn parse(a: &mut Args) -> R<Freeverb> {
        Ok(Freeverb {
            size: a.expr("size", "0.5")?,
            reverb: Reverb::new(a.rate),
        })
    }
}

impl Block for Freeverb {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let feedback = Reverb::feedback(self.size.eval(v).clamp(0.0, 1.0));
        self.reverb.wet(x, feedback, 1.0).map(|s| s * WET)
    }

    fn reset(&mut self) {
        self.reverb.clear();
    }
}

/// `pitch`: shifts by `semitones` with two crossfaded delay taps over a `window` (seconds; 0.03 to 0.06 suits voices,
/// 0.5 or more gives a tape-slowing sound). Within `dead` semitones of 0 the signal passes unshifted: the taps would
/// otherwise stand still and comb it.
pub struct Pitch {
    semitones: Expr,
    dead: f32,
    shifter: Shifter,
}

impl Pitch {
    pub fn parse(a: &mut Args) -> R<Pitch> {
        Ok(Pitch {
            semitones: a.expr("semitones", "0")?,
            dead: a.num("dead", 0.01, 0.0, 12.0)?,
            shifter: Shifter::new(a.rate, a.num("window", 0.05, 0.005, 2.0)?),
        })
    }
}

impl Block for Pitch {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let s = self.semitones.eval(v);
        let shifted = self.shifter.tick(x, (s / 12.0).exp2());
        if s.abs() < self.dead { x } else { shifted }
    }

    fn reset(&mut self) {
        self.shifter.clear();
    }
}
