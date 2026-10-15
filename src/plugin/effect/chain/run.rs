//! A whole declared effect: [`compile`] checks a [`ChainDecl`] once, [`Def::stage`] builds a running instance of it.

use super::{
    BEATS, BPM, BUILTINS, Chain, ChainDecl, Expr, G, KNOB, LEVEL, MAX_LFOS, PERIOD, PERIOD_MS, R,
    RATE, Scope, Vars, parse_into,
};
use crate::audio::fx::{Ctx, Frame, Stage, finite};
use openfader_plugin::dsp::{Lfo, sweep};
use serde_json::Value;
use std::f32::consts::TAU;

/// How an LFO's phase (0 to 1 per period) becomes its value.
fn shape(name: &str) -> Option<fn(f32) -> f32> {
    Some(match name {
        "sine" => |p| (p * TAU).sin(),
        "sweep" => sweep,
        "saw" => |p| p,
        "tri" => |p| 1.0 - (2.0 * p - 1.0).abs(),
        "square" => |p| f32::from(u8::from(p < 0.5)),
        _ => return None,
    })
}

/// A checked declaration of one effect.
#[derive(Clone)]
pub struct Def {
    decl: ChainDecl,
}

impl std::fmt::Debug for Def {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Def({:?})", self.decl.name)
    }
}

/// Parses and checks `decl` (every block, parameter and formula); the error says where the mistake is.
pub fn compile(decl: &ChainDecl) -> Result<Def, String> {
    let def = Def { decl: decl.clone() };
    def.build(super::super::PROBE_RATE)?;
    Ok(def)
}

/// One LFO of a running chain.
struct LfoRun {
    period: Expr,
    /// `period` is a rate in Hz.
    inverse: bool,
    shape: fn(f32) -> f32,
    phase: Lfo,
}

/// A running chain: what [`Def::stage`] returns.
struct Run {
    lfos: Vec<LfoRun>,
    chain: Chain,
    vars: Vars,
}

impl Def {
    fn build(&self, rate: u32) -> R<Run> {
        let d = &self.decl;
        let names: Vec<String> = d.lfo.iter().map(|l| l.name.clone()).collect();
        if names.len() > MAX_LFOS {
            return Err(format!("at most {MAX_LFOS} lfos"));
        }
        for (i, n) in names.iter().enumerate() {
            let ident = n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && n.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
            if !ident || BUILTINS.contains(&n.as_str()) || names[..i].contains(n) {
                return Err(format!(
                    "lfo name {n:?} must be a new plain word (letters, digits, _)"
                ));
            }
        }
        // An LFO's own period may use the built-in variables, not other LFOs.
        let builtin = Scope { lfos: Vec::new() };
        let mut lfos = Vec::new();
        for (i, l) in d.lfo.iter().enumerate() {
            let at = format!("lfo[{i}] ({})", l.name);
            let Some(sh) = shape(&l.shape) else {
                return Err(format!(
                    "{at}: shape must be one of sine, sweep, saw, tri, square"
                ));
            };
            let one = |key: &str, v: &Value| -> R<Expr> {
                match v {
                    Value::Number(n) => Ok(Expr::constant(n.as_f64().unwrap_or(0.0) as f32)),
                    Value::String(s) => {
                        Expr::parse(s, &builtin).map_err(|e| format!("{at}: \"{key}\": {e}"))
                    }
                    _ => Err(format!(
                        "{at}: \"{key}\" must be a number or a formula string"
                    )),
                }
            };
            // The period in seconds, or the rate in Hz (`inverse`).
            lfos.push(match (&l.period, &l.hz) {
                (Some(p), None) => (one("period", p)?, false, sh),
                (None, Some(h)) => (one("hz", h)?, true, sh),
                _ => return Err(format!("{at}: give either \"period\" (seconds) or \"hz\"")),
            });
        }
        let scope = Scope { lfos: names };
        let mut blocks = Vec::new();
        for (i, b) in d.chain.iter().enumerate() {
            parse_into(b, &scope, rate, &format!("chain[{i}]"), &mut blocks)?;
        }
        Ok(Run {
            lfos: lfos
                .into_iter()
                .map(|(period, inverse, shape)| LfoRun {
                    period,
                    inverse,
                    shape,
                    phase: Lfo::default(),
                })
                .collect(),
            chain: Chain(blocks),
            vars: Vars::default(),
        })
    }

    /// One instance for an FX unit at `rate` Hz. The declaration was checked by [`compile`]; should building still fail,
    /// the stage passes the signal through.
    pub fn stage(&self, rate: u32) -> Box<dyn Stage> {
        match self.build(rate) {
            Ok(run) => Box::new(run),
            Err(_) => Box::new(super::super::Bypass),
        }
    }
}

impl Stage for Run {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        let v = &mut self.vars;
        v.slot[KNOB] = c.knob;
        v.slot[LEVEL] = c.level;
        v.slot[G] = c.g;
        v.slot[BEATS] = c.beats;
        v.slot[RATE] = c.rate;
        v.slot[PERIOD] = c.period / c.rate;
        v.slot[PERIOD_MS] = c.period / c.rate * 1000.0;
        v.slot[BPM] = if c.period > 0.0 {
            c.beats * 60.0 * c.rate / c.period
        } else {
            0.0
        };
        v.dry = x;
        for (i, l) in self.lfos.iter_mut().enumerate() {
            let p = l.period.eval(v);
            let samples = if l.inverse { c.rate / p } else { p * c.rate };
            v.slot[BUILTINS.len() + i] = (l.shape)(l.phase.tick(samples));
        }
        let y = self.chain.tick(x, &self.vars);
        if finite(y) {
            y
        } else {
            self.reset();
            [0.0; 2]
        }
    }

    fn reset(&mut self) {
        self.chain.reset();
        self.lfos.iter_mut().for_each(|l| l.phase.reset());
    }

    fn engage(&mut self) {
        self.chain.engage();
        self.lfos.iter_mut().for_each(|l| l.phase.reset());
    }
}
