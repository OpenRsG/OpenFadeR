//! The plain blocks: `gain`, `route`, `pan`, `width`, `mix`, `saturate`, `bitcrush`.

use super::{Args, Block, Expr, Frame, R, Vars, scale};

/// `gain`: the signal times a formula, optionally smoothed (a trance gate's declick).
pub struct Gain {
    gain: Expr,
    /// Smoothing coefficient per frame; 1 = none.
    a: f32,
    z: f32,
}

impl Gain {
    pub fn parse(a: &mut Args) -> R<Gain> {
        let gain = a.expr("gain", "1")?;
        let smooth = a.num("smooth", 0.0, 0.0, 10.0)?;
        let rate = a.rate as f32;
        Ok(Gain {
            gain,
            a: if smooth > 0.0 {
                1.0 - (-1.0 / (smooth * rate)).exp()
            } else {
                1.0
            },
            z: 0.0,
        })
    }
}

impl Block for Gain {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let t = self.gain.eval(v);
        self.z = if self.a < 1.0 {
            self.z + (t - self.z) * self.a
        } else {
            t
        };
        scale(x, self.z)
    }

    fn reset(&mut self) {
        self.z = 0.0;
    }
}

/// `route`: a 2 × 2 matrix, `[[ll, lr], [rl, rr]]`: left out = ll × left + lr × right, right out = rl × left + rr × right.
pub struct Route([[Expr; 2]; 2]);

impl Route {
    pub fn parse(a: &mut Args) -> R<Route> {
        let raw = a
            .raw("matrix")
            .unwrap_or_else(|| serde_json::json!([[1, 0], [0, 1]]));
        let rows = raw.as_array().filter(|r| {
            r.len() == 2 && r.iter().all(|r| r.as_array().is_some_and(|r| r.len() == 2))
        });
        let Some(rows) = rows else {
            return a.fail("\"matrix\" must be [[ll, lr], [rl, rr]]");
        };
        let cell = |r: usize, c: usize| a.expr_of(&rows[r][c], "matrix");
        Ok(Route([
            [cell(0, 0)?, cell(0, 1)?],
            [cell(1, 0)?, cell(1, 1)?],
        ]))
    }
}

impl Block for Route {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let m = &self.0;
        [
            m[0][0].eval(v) * x[0] + m[0][1].eval(v) * x[1],
            m[1][0].eval(v) * x[0] + m[1][1].eval(v) * x[1],
        ]
    }

    fn reset(&mut self) {}
}

/// `pan`: balance from -1 (left only) to 1 (right only); the louder side keeps its level.
pub struct Pan(Expr);

impl Pan {
    pub fn parse(a: &mut Args) -> R<Pan> {
        Ok(Pan(a.expr("pan", "0")?))
    }
}

impl Block for Pan {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let p = self.0.eval(v).clamp(-1.0, 1.0);
        [x[0] * (1.0 - p.max(0.0)), x[1] * (1.0 + p.min(0.0))]
    }

    fn reset(&mut self) {}
}

/// `width`: the stereo width, 0 = mono, 1 = unchanged, 2 = twice the side signal.
pub struct Width(Expr);

impl Width {
    pub fn parse(a: &mut Args) -> R<Width> {
        Ok(Width(a.expr("width", "1")?))
    }
}

impl Block for Width {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let (m, s) = ((x[0] + x[1]) * 0.5, (x[0] - x[1]) * 0.5 * self.0.eval(v));
        [m + s, m - s]
    }

    fn reset(&mut self) {}
}

/// `mix`: the effect's own input (the dry signal) against the signal here: `blend` moves from dry towards it by
/// `amount`, `add` adds `amount` times it to the dry signal.
pub struct Mix {
    amount: Expr,
    add: bool,
}

impl Mix {
    pub fn parse(a: &mut Args) -> R<Mix> {
        Ok(Mix {
            amount: a.expr("amount", "level * g")?,
            add: a.word("mode", "blend", &["blend", "add"])? == "add",
        })
    }
}

impl Block for Mix {
    fn tick(&mut self, w: Frame, v: &Vars) -> Frame {
        let (d, m) = (v.dry, self.amount.eval(v));
        if self.add {
            [d[0] + w[0] * m, d[1] + w[1] * m]
        } else {
            [d[0] + (w[0] - d[0]) * m, d[1] + (w[1] - d[1]) * m]
        }
    }

    fn reset(&mut self) {}
}

/// `saturate`: `tanh(x × drive)` (soft) or the signal times drive clipped to ±1 (hard).
pub struct Saturate {
    drive: Expr,
    hard: bool,
}

impl Saturate {
    pub fn parse(a: &mut Args) -> R<Saturate> {
        Ok(Saturate {
            drive: a.expr("drive", "1")?,
            hard: a.word("shape", "tanh", &["tanh", "clip"])? == "clip",
        })
    }
}

impl Block for Saturate {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let d = self.drive.eval(v);
        x.map(|s| {
            if self.hard {
                (s * d).clamp(-1.0, 1.0)
            } else {
                (s * d).tanh()
            }
        })
    }

    fn reset(&mut self) {}
}

/// `bitcrush`: quantises to `bits` bits and holds each sample for `rate / hz` frames (0 = no downsampling).
pub struct Bitcrush {
    bits: Expr,
    hz: Expr,
    rate: f32,
    phase: f32,
    held: Frame,
}

impl Bitcrush {
    pub fn parse(a: &mut Args) -> R<Bitcrush> {
        Ok(Bitcrush {
            bits: a.expr("bits", "8")?,
            hz: a.expr("hz", "0")?,
            rate: a.rate as f32,
            phase: 0.0,
            held: [0.0; 2],
        })
    }
}

impl Block for Bitcrush {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let step = (1.0 - self.bits.eval(v).clamp(1.0, 24.0)).exp2();
        let crush = |s: f32| (s / step).round() * step;
        let hz = self.hz.eval(v);
        if hz <= 0.0 || hz >= self.rate {
            return x.map(crush);
        }
        self.phase += hz / self.rate;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            self.held = x.map(crush);
        }
        self.held
    }

    fn reset(&mut self) {
        self.phase = 0.0;
        self.held = [0.0; 2];
    }
}
