//! The filter blocks: `filter` and `shepard`.

use super::{Args, Block, Expr, Frame, R, Vars};
use openfader_plugin::dsp::{Lfo, OnePole, Svf, pole};
use std::f32::consts::{FRAC_1_SQRT_2, PI};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Low,
    High,
    Band,
    Notch,
}

/// `filter`: `lp`, `hp`, `bp` or `notch` at `hz` with quality `q` (0.707 = Butterworth), both formulas, so a sweep is
/// `"hz": "expo(18000, 150, sweep)"`. `order` 2 (default) is a resonant state-variable filter (Zavalishin, zero-delay
/// feedback; the one the host's LPF / HPF sweep uses), `order` 1 a 6 dB per octave one-pole `lp` or `hp` (no `q`).
pub struct Filter {
    kind: Kind,
    first_order: bool,
    hz: Expr,
    q: Expr,
    rate: f32,
    svf: Svf,
    pole: OnePole,
}

impl Filter {
    pub fn parse(a: &mut Args) -> R<Filter> {
        let kind = match a.word("type", "lp", &["lp", "hp", "bp", "notch"])? {
            "lp" => Kind::Low,
            "hp" => Kind::High,
            "bp" => Kind::Band,
            _ => Kind::Notch,
        };
        let first_order = a.num("order", 2.0, 1.0, 2.0)? as u8 == 1;
        if first_order && !matches!(kind, Kind::Low | Kind::High) {
            return a.fail("\"order\": 1 is for lp and hp only");
        }
        Ok(Filter {
            kind,
            first_order,
            hz: a.expr("hz", "1000")?,
            q: a.expr("q", &FRAC_1_SQRT_2.to_string())?,
            rate: a.rate as f32,
            svf: Svf::default(),
            pole: OnePole::default(),
        })
    }
}

impl Block for Filter {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let hz = self.hz.eval(v);
        if self.first_order {
            let a = pole(hz.clamp(1.0, 0.45 * self.rate), self.rate);
            return if self.kind == Kind::Low {
                self.pole.low(x, a)
            } else {
                self.pole.high(x, a)
            };
        }
        let b = self.svf.tick(x, hz, self.q.eval(v).max(0.1), self.rate);
        match self.kind {
            Kind::Low => b.low,
            Kind::High => b.high,
            Kind::Band => b.band,
            Kind::Notch => [b.low[0] + b.high[0], b.low[1] + b.high[1]],
        }
    }

    fn reset(&mut self) {
        self.svf.clear();
        self.pole.clear();
    }
}

/// `shepard`: `bands` band-pass filters one octave apart from `low` Hz ride up the spectrum one octave per `period`
/// seconds, each fading in at the bottom and out at the top (sin² weight over the span), so the sweep never ends: the
/// barber-pole effect applied to a filter. The output is the weighted bands (times `gain`) plus `dry` of the input.
/// Restarts on ON.
pub struct Shepard {
    period: Expr,
    low: f32,
    q: f32,
    dry: f32,
    gain: f32,
    rate: f32,
    lfo: Lfo,
    bank: Vec<Svf>,
    /// Last phase, to see the wrap.
    last: f32,
}

impl Shepard {
    pub fn parse(a: &mut Args) -> R<Shepard> {
        Ok(Shepard {
            period: a.expr("period", "period")?,
            low: a.num("low", 40.0, 1.0, 5000.0)?,
            q: a.num("q", 3.0, 0.1, 50.0)?,
            dry: a.num("dry", 0.33, 0.0, 4.0)?,
            gain: a.num("gain", 1.5, 0.0, 16.0)?,
            bank: (0..a.num("bands", 7.0, 2.0, 12.0)? as usize)
                .map(|_| Svf::default())
                .collect(),
            rate: a.rate as f32,
            lfo: Lfo::default(),
            last: 0.0,
        })
    }
}

impl Block for Shepard {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let ph = self.lfo.tick(self.period.eval(v) * self.rate);
        if ph < self.last {
            // Filter k is now where filter k + 1 was: pass the state on so the wrap does not click.
            self.bank.rotate_right(1);
        }
        self.last = ph;
        let n = self.bank.len() as f32;
        let mut w = [x[0] * self.dry, x[1] * self.dry];
        for (k, svf) in self.bank.iter_mut().enumerate() {
            let u = (k as f32 + ph) / n;
            let weight = (u * PI).sin().powi(2) * self.gain;
            let b = svf
                .tick(x, self.low * (k as f32 + ph).exp2(), self.q, self.rate)
                .band;
            w[0] += b[0] * weight;
            w[1] += b[1] * weight;
        }
        w
    }

    fn reset(&mut self) {
        self.bank.iter_mut().for_each(Svf::clear);
        self.lfo.reset();
        self.last = 0.0;
    }

    fn engage(&mut self) {
        self.reset();
    }
}
