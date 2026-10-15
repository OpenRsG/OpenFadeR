//! Effect chains (base): effects declared in `plugin.json`, no compiled code. An effect is a list of blocks the signal
//! runs through in order, plus optional LFOs whose values the blocks' expressions can read (grammar and variables:
//! [`expr`]; every block and its parameters: `docs/plugins.md`).
//!
//! [`compile`] reads a declaration once, when the plugin loads: it parses every block and expression, so a mistake is a
//! load error and never a surprise on the audio thread. [`Def::stage`] builds one running [`Stage`] per FX unit from the
//! checked declaration (the blocks allocate their delay lines and buffers there, as a native library's `create` does).
//! The audio callback only runs [`Block::tick`]: fixed-size expression stacks, no allocation, no parsing, no locks.
//!
//! Children: [`expr`] (the expression language), `run` (a whole declared effect: [`compile`], [`Def`], the running
//! stage with its LFOs), `basic` (gain, routing, mix, saturation, bit crusher), `delay` (delay line, allpass, phaser,
//! Freeverb, pitch shifter), `filter` (state-variable / one-pole filter, Shepard bank), `container` (feedback loop,
//! parallel branches, crossover), `capture` (loop, slice, reverse, tape), `sugar` (`echo` and `reverb`, written as the
//! blocks they stand for).

mod basic;
mod capture;
mod container;
mod delay;
pub mod expr;
mod filter;
mod run;
mod sugar;
#[cfg(test)]
mod tests;

pub use run::{Def, compile};

use super::ChainDecl;
use crate::audio::fx::Frame;
use expr::Expr;
use serde_json::{Map, Value};

/// Slots of [`Vars`] the host fills: the names [`Scope`] knows before any LFO.
const BUILTINS: [&str; 8] = [
    "knob",
    "level",
    "g",
    "beats",
    "rate",
    "period",
    "period_ms",
    "bpm",
];
const KNOB: usize = 0;
const LEVEL: usize = 1;
const G: usize = 2;
const BEATS: usize = 3;
const RATE: usize = 4;
const PERIOD: usize = 5;
const PERIOD_MS: usize = 6;
const BPM: usize = 7;
/// The most feedback a loop may have; 1 would ring forever and anything above runs away.
const MAX_FEEDBACK: f32 = 0.99;
/// Most LFOs one effect declares.
pub const MAX_LFOS: usize = 4;
const SLOTS: usize = BUILTINS.len() + MAX_LFOS;

/// What the expressions of one frame read: the context's values by slot, and the signal the effect received (for `mix`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Vars {
    slot: [f32; SLOTS],
    dry: Frame,
}

/// The variable names of one effect: the built-ins, then its LFOs.
pub struct Scope {
    lfos: Vec<String>,
}

impl Scope {
    fn slot(&self, name: &str) -> Option<u8> {
        let at = BUILTINS.iter().position(|b| *b == name).or_else(|| {
            self.lfos
                .iter()
                .position(|l| l == name)
                .map(|i| BUILTINS.len() + i)
        })?;
        Some(at as u8)
    }
}

/// One step of a chain: the state of one parsed block. Built by [`parse_into`], ticked on the audio thread.
trait Block: Send {
    /// The block's output for input `x` (the effect's own input is `v.dry`).
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame;
    /// Forget everything (the effect was selected, or woke from idle).
    fn reset(&mut self);
    /// The effect was switched ON: restart phases and captures. Delay lines and filters keep their tails.
    fn engage(&mut self) {}
}

/// Blocks in series.
#[derive(Default)]
struct Chain(Vec<Box<dyn Block>>);

impl Chain {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        self.0.iter_mut().fold(x, |x, b| b.tick(x, v))
    }

    fn reset(&mut self) {
        self.0.iter_mut().for_each(|b| b.reset());
    }

    fn engage(&mut self) {
        self.0.iter_mut().for_each(|b| b.engage());
    }
}

fn add(a: Frame, b: Frame) -> Frame {
    [a[0] + b[0], a[1] + b[1]]
}

fn scale(a: Frame, k: f32) -> Frame {
    [a[0] * k, a[1] * k]
}

/// The parameters of one block's JSON object, taken out one by one; [`Args::end`] rejects what is left over.
struct Args<'a> {
    /// Where the block is, for messages (`chain[2]`, `chain[0].loop[1]`).
    at: String,
    name: String,
    map: Map<String, Value>,
    rate: u32,
    scope: &'a Scope,
}

type R<T> = Result<T, String>;

impl Args<'_> {
    fn fail<T>(&self, msg: impl std::fmt::Display) -> R<T> {
        Err(format!("{} ({}): {msg}", self.at, self.name))
    }

    fn raw(&mut self, key: &str) -> Option<Value> {
        self.map.remove(key)
    }

    /// A number or a formula; `default` (formula text) when absent.
    fn expr(&mut self, key: &str, default: &str) -> R<Expr> {
        let v = self.raw(key).unwrap_or_else(|| default.into());
        self.expr_of(&v, key)
    }

    fn expr_of(&self, v: &Value, key: &str) -> R<Expr> {
        match v {
            Value::Number(n) => Ok(Expr::constant(n.as_f64().unwrap_or(0.0) as f32)),
            Value::String(s) => {
                Expr::parse(s, self.scope).or_else(|e| self.fail(format!("\"{key}\": {e}")))
            }
            _ => self.fail(format!("\"{key}\" must be a number or a formula string")),
        }
    }

    /// A plain number in `lo..=hi`: a JSON number, or a string with no variables (`"5ms"`).
    fn num(&mut self, key: &str, default: f32, lo: f32, hi: f32) -> R<f32> {
        let Some(v) = self.raw(key) else {
            return Ok(default);
        };
        let n = match &v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => Expr::parse(s, self.scope)
                .ok()
                .and_then(|e| e.as_const())
                .map(f64::from),
            _ => None,
        };
        match n {
            Some(n) if (f64::from(lo)..=f64::from(hi)).contains(&n) => Ok(n as f32),
            _ => self.fail(format!("\"{key}\" must be a number from {lo} to {hi}")),
        }
    }

    fn flag(&mut self, key: &str, default: bool) -> R<bool> {
        match self.raw(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(b),
            Some(_) => self.fail(format!("\"{key}\" must be true or false")),
        }
    }

    /// One of `allowed`; `default` when absent.
    fn word(
        &mut self,
        key: &str,
        default: &'static str,
        allowed: &[&'static str],
    ) -> R<&'static str> {
        match self.raw(key) {
            None => Ok(default),
            Some(Value::String(s)) if allowed.contains(&s.as_str()) => {
                Ok(allowed.iter().find(|a| **a == s).expect("listed"))
            }
            Some(_) => self.fail(format!("\"{key}\" must be one of {}", allowed.join(", "))),
        }
    }

    /// The blocks of the array `key` (empty when absent).
    fn blocks(&mut self, key: &str) -> R<Chain> {
        match self.raw(key) {
            None => Ok(Chain::default()),
            Some(v) => self.chain_of(&v, key, &format!("{}.{key}", self.at)),
        }
    }

    fn chain_of(&self, v: &Value, key: &str, at: &str) -> R<Chain> {
        let Value::Array(list) = v else {
            return self.fail(format!("\"{key}\" must be a list of blocks"));
        };
        let mut c = Chain::default();
        for (i, b) in list.iter().enumerate() {
            parse_into(b, self.scope, self.rate, &format!("{at}[{i}]"), &mut c.0)?;
        }
        Ok(c)
    }

    /// Fails when a parameter nobody asked for is left.
    fn end(self) -> R<()> {
        match self.map.keys().next() {
            Some(k) => self.fail(format!("unknown parameter \"{k}\"")),
            None => Ok(()),
        }
    }
}

/// Every block name `docs/plugins.md` lists, for the error of an unknown one.
const BLOCKS: &str = "gain, route, pan, width, mix, saturate, bitcrush, delay, allpass, phaser, freeverb, pitch, filter, \
shepard, feedback, parallel, crossover, loop, slice, reverse, tape, echo, reverb";

/// Parses the block `v` and appends it (a sugar block like `echo` appends several) to `out`.
fn parse_into(
    v: &Value,
    scope: &Scope,
    rate: u32,
    at: &str,
    out: &mut Vec<Box<dyn Block>>,
) -> R<()> {
    let Value::Object(obj) = v else {
        return Err(format!(
            "{at}: a block is an object like {{\"block\": \"gain\", \"gain\": 0.5}}"
        ));
    };
    let mut map = obj.clone();
    let Some(Value::String(name)) = map.remove("block") else {
        return Err(format!(
            "{at}: every block needs a \"block\" name ({BLOCKS})"
        ));
    };
    let mut a = Args {
        at: at.into(),
        name: name.clone(),
        map,
        rate,
        scope,
    };
    macro_rules! one {
        ($t:ty) => {{
            let b = <$t>::parse(&mut a)?;
            a.end()?;
            out.push(Box::new(b));
        }};
    }
    match name.as_str() {
        "gain" => one!(basic::Gain),
        "route" => one!(basic::Route),
        "pan" => one!(basic::Pan),
        "width" => one!(basic::Width),
        "mix" => one!(basic::Mix),
        "saturate" => one!(basic::Saturate),
        "bitcrush" => one!(basic::Bitcrush),
        "delay" => one!(delay::Delay),
        "allpass" => one!(delay::Allpass),
        "phaser" => one!(delay::Phaser),
        "freeverb" => one!(delay::Freeverb),
        "pitch" => one!(delay::Pitch),
        "filter" => one!(filter::Filter),
        "shepard" => one!(filter::Shepard),
        "feedback" => one!(container::Feedback),
        "parallel" => one!(container::Parallel),
        "crossover" => one!(container::Crossover),
        "loop" => one!(capture::Loop),
        "slice" => one!(capture::Slice),
        "reverse" => one!(capture::Reverse),
        "tape" => one!(capture::Tape),
        "echo" | "reverb" => {
            let list = if name == "echo" {
                sugar::echo(&mut a)?
            } else {
                sugar::reverb(&mut a)?
            };
            a.end()?;
            for (i, b) in list.iter().enumerate() {
                parse_into(b, scope, rate, &format!("{at}.{name}[{i}]"), out)?;
            }
        }
        _ => return Err(format!("{at}: unknown block {name:?} ({BLOCKS})")),
    }
    Ok(())
}
