//! The blocks that record audio: `loop`, `slice`, `reverse`, `tape`. They restart when the effect is switched ON (`engage`)
//! and hand the input back unchanged while they record (follow them with `mix` to fade the effect in by LEVEL).

use super::{Args, Block, Expr, Frame, R, Vars};
use openfader_plugin::dsp::{Line, fold};

/// Frames a slice or chunk fades in and out over (2 ms at 48 kHz) so its seams do not click.
const FADE: usize = 96;

/// Gain of frame `i` of a `len`-frame piece: a linear fade over [`FADE`] frames at both ends.
fn edge(i: usize, len: usize) -> f32 {
    if len <= 2 * FADE {
        return 1.0;
    }
    (i.min(len - 1 - i) as f32 / FADE as f32).min(1.0)
}

fn silent(rate: u32, seconds: f32) -> Vec<Frame> {
    vec![[0.0; 2]; (rate as f32 * seconds) as usize]
}

/// `loop`: the next `time` seconds of input are recorded when the effect goes ON (and still heard), then played over
/// and over. Each pass can play faster than the last by `climb` semitones (a tape-speed stair: the pass gets shorter and
/// higher), starting over after `steps` passes. Records at most `max` seconds (longer times are halved until they fit).
pub struct Loop {
    time: Expr,
    climb: f32,
    steps: u32,
    rate: f32,
    buf: Vec<Frame>,
    /// Slice length in frames, 0 = not decided yet (set by the first tick after ON).
    len: usize,
    /// Frames recorded so far; the loop starts when it reaches `len`.
    rec: usize,
    pos: f64,
    pass: u32,
}

impl Loop {
    pub fn parse(a: &mut Args) -> R<Loop> {
        Ok(Loop {
            time: a.expr("time", "period")?,
            climb: a.num("climb", 0.0, -12.0, 12.0)?,
            steps: a.num("steps", 12.0, 1.0, 1000.0)? as u32,
            rate: a.rate as f32,
            buf: silent(a.rate, a.num("max", 4.0, 0.01, 30.0)?),
            len: 0,
            rec: 0,
            pos: 0.0,
            pass: 0,
        })
    }
}

impl Block for Loop {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        if self.len == 0 {
            let p = fold(self.time.eval(v) * self.rate, self.buf.len() as f32);
            self.len = (p.round() as usize).clamp(2, self.buf.len());
        }
        if self.rec < self.len {
            self.buf[self.rec] = x;
            self.rec += 1;
            return x;
        }
        let (i, f) = (self.pos as usize, self.pos.fract() as f32);
        let (a, b) = (self.buf[i], self.buf[(i + 1) % self.len]);
        let w = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
        self.pos += f64::from((self.pass as f32 * self.climb / 12.0).exp2());
        if self.pos >= self.len as f64 {
            self.pos -= self.len as f64;
            self.pass = (self.pass + 1) % self.steps;
        }
        w
    }

    fn reset(&mut self) {
        (self.len, self.rec, self.pos, self.pass) = (0, 0, 0.0, 0);
    }

    fn engage(&mut self) {
        self.reset();
    }
}

/// `slice`: the next `time` seconds are captured (and heard), then cut into `count` slices (a formula, read at each
/// slice start) that are re-triggered one after the other, every `stride`-th of the capture in turn (0, 3, 6, ... modulo
/// the count with the default 3), so the beat is juggled. Keeps at most `max` seconds.
pub struct Slice {
    time: Expr,
    count: Expr,
    stride: usize,
    rate: f32,
    buf: Vec<Frame>,
    /// Captured frames so far, and the capture's length (0 = not decided yet).
    rec: usize,
    len: usize,
    /// The slice playing: its first frame, its length and the frame within it.
    start: usize,
    sl: usize,
    at: usize,
    /// Slices started so far.
    n: usize,
}

impl Slice {
    pub fn parse(a: &mut Args) -> R<Slice> {
        Ok(Slice {
            time: a.expr("time", "period")?,
            count: a.expr("count", "8")?,
            stride: a.num("stride", 3.0, 1.0, 64.0)? as usize,
            rate: a.rate as f32,
            buf: silent(a.rate, a.num("max", 2.0, 0.01, 30.0)?),
            rec: 0,
            len: 0,
            start: 0,
            sl: 0,
            at: 0,
            n: 0,
        })
    }
}

impl Block for Slice {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        if self.len == 0 {
            let p = fold(self.time.eval(v) * self.rate, self.buf.len() as f32);
            self.len = (p.round() as usize).clamp(4, self.buf.len());
        }
        if self.rec < self.len {
            self.buf[self.rec] = x;
            self.rec += 1;
            return x;
        }
        if self.at == 0 {
            let count = (self.count.eval(v).round() as usize).clamp(1, self.len);
            self.sl = (self.len / count).max(1);
            self.start = (self.stride * self.n) % count * self.sl;
            self.n += 1;
        }
        let g = edge(self.at, self.sl);
        let w = self.buf[self.start + self.at].map(|s| s * g);
        self.at = (self.at + 1) % self.sl;
        w
    }

    fn reset(&mut self) {
        (self.rec, self.len, self.start, self.sl, self.at, self.n) = (0, 0, 0, 0, 0, 0);
    }

    fn engage(&mut self) {
        self.reset();
    }
}

/// `reverse`: the previous `time` seconds played backwards: while a chunk is recorded into one buffer the last one plays
/// back from the other, end to start, swapping every chunk (the classic chunk reverser). The first chunk is heard
/// forwards while it records. Keeps at most `max` seconds per chunk.
pub struct Reverse {
    time: Expr,
    rate: f32,
    bufs: [Vec<Frame>; 2],
    /// Chunk length in frames (0 = not decided yet), the frame within it and the buffer being recorded.
    len: usize,
    pos: usize,
    rec: usize,
    /// A recorded chunk is waiting to play.
    have: bool,
}

impl Reverse {
    pub fn parse(a: &mut Args) -> R<Reverse> {
        let max = a.num("max", 2.0, 0.01, 30.0)?;
        Ok(Reverse {
            time: a.expr("time", "period")?,
            rate: a.rate as f32,
            bufs: [silent(a.rate, max), silent(a.rate, max)],
            len: 0,
            pos: 0,
            rec: 0,
            have: false,
        })
    }
}

impl Block for Reverse {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        if self.len == 0 {
            let p = fold(self.time.eval(v) * self.rate, self.bufs[0].len() as f32);
            self.len = (p.round() as usize).clamp(2, self.bufs[0].len());
        }
        let w = if self.have {
            let g = edge(self.pos, self.len);
            self.bufs[1 - self.rec][self.len - 1 - self.pos].map(|s| s * g)
        } else {
            x
        };
        self.bufs[self.rec][self.pos] = x;
        self.pos += 1;
        if self.pos == self.len {
            (self.pos, self.rec, self.have) = (0, 1 - self.rec, true);
        }
        w
    }

    fn reset(&mut self) {
        (self.len, self.pos, self.rec, self.have) = (0, 0, 0, false);
    }

    fn engage(&mut self) {
        self.reset();
    }
}

/// `tape`: varispeed playback of the last `max` seconds: the read head moves at `speed` times normal speed (a formula;
/// 1 = live, 0.25 = a quarter speed and two octaves down), so below 1 the audio falls further behind the live signal,
/// up to `max` seconds, and above 1 it catches up. Every pull starts from live.
pub struct Tape {
    speed: Expr,
    limit: f32,
    line: Line,
    /// How far behind the live signal the read head is, in frames.
    lag: f32,
}

impl Tape {
    pub fn parse(a: &mut Args) -> R<Tape> {
        let max = a.num("max", 2.0, 0.01, 30.0)?;
        Ok(Tape {
            speed: a.expr("speed", "1")?,
            limit: max * a.rate as f32,
            line: Line::new(a.rate, max + 0.1),
            lag: 1.0,
        })
    }
}

impl Block for Tape {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        self.line.push(x);
        self.lag = (self.lag + 1.0 - self.speed.eval(v)).clamp(1.0, self.limit);
        self.line.read(self.lag)
    }

    fn reset(&mut self) {
        self.line.clear();
        self.lag = 1.0;
    }

    fn engage(&mut self) {
        self.reset();
    }
}
