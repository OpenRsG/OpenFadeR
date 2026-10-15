//! Expressions of effect chains: a number or a small formula over the per-sample context (`knob`, `level`, `g`, `beats`,
//! `rate`, `period`, `period_ms`, `bpm` and the effect's LFOs), written as a JSON number or string. [`Expr::parse`] turns
//! one into postfix code once, when the plugin loads; [`Expr::eval`] runs it on the audio thread with a fixed stack, no
//! allocation. A formula without variables is folded to a constant at parse time.
//!
//! Syntax: numbers (`0.5`, `1e-3`, with the suffixes `ms` = ÷1000 and `k` = ×1000), variables, `pi` and `tau`,
//! `+ - * / %`, `^` (power, right associative), comparisons (`< > <= >= == !=`, giving 1 or 0), `&&`, `||`, `!`,
//! parentheses and the functions listed in [`function`]. A result that is not finite evaluates to 0.

use super::{Scope, Vars};
use std::f32::consts::{PI, TAU};

/// Deepest evaluation stack; a formula needing more is rejected when it is parsed.
const DEPTH: usize = 16;

#[derive(Clone, Copy, Debug)]
enum Op {
    Const(f32),
    Var(u8),
    Neg,
    Not,
    F1(fn(f32) -> f32),
    F2(fn(f32, f32) -> f32),
    F3(fn(f32, f32, f32) -> f32),
}

/// A parsed expression.
#[derive(Clone, Debug)]
pub struct Expr(Vec<Op>);

/// A function of the expression language: `sin cos tanh abs sqrt floor round exp2 log2 semi` (one argument; `semi(x)` is
/// the frequency ratio of `x` semitones), `min max pow` (two) and `clamp(x, lo, hi)`, `lerp(a, b, t)`,
/// `expo(a, b, t)` (= a × (b / a)^t: the sweep from a to b on a pitch scale) and `if(c, a, b)` (three).
fn function(name: &str) -> Option<Op> {
    Some(match name {
        "sin" => Op::F1(f32::sin),
        "cos" => Op::F1(f32::cos),
        "tanh" => Op::F1(f32::tanh),
        "abs" => Op::F1(f32::abs),
        "sqrt" => Op::F1(f32::sqrt),
        "floor" => Op::F1(f32::floor),
        "round" => Op::F1(f32::round),
        "exp2" => Op::F1(f32::exp2),
        "log2" => Op::F1(f32::log2),
        "semi" => Op::F1(|x| (x / 12.0).exp2()),
        "min" => Op::F2(f32::min),
        "max" => Op::F2(f32::max),
        "pow" => Op::F2(f32::powf),
        "clamp" => Op::F3(|x, lo, hi| x.max(lo).min(hi)),
        "lerp" => Op::F3(|a, b, t| a + (b - a) * t),
        "expo" => Op::F3(|a, b, t| a * (b / a).powf(t)),
        "if" => Op::F3(|c, a, b| if c != 0.0 { a } else { b }),
        _ => return None,
    })
}

fn truth(b: bool) -> f32 {
    f32::from(u8::from(b))
}

impl Expr {
    pub fn constant(v: f32) -> Expr {
        Expr(vec![Op::Const(v)])
    }

    /// Parses `src`; variable names come from `scope`.
    pub fn parse(src: &str, scope: &Scope) -> Result<Expr, String> {
        let fail = |m: String| format!("expression {src:?}: {m}");
        let mut p = Parser {
            s: src.as_bytes(),
            i: 0,
            scope,
            ops: Vec::new(),
        };
        p.or().map_err(fail)?;
        p.ws();
        if p.i < p.s.len() {
            return Err(fail(format!("unexpected {:?}", &src[p.i..])));
        }
        let (mut depth, mut max) = (0i32, 0i32);
        for op in &p.ops {
            depth += match op {
                Op::Const(_) | Op::Var(_) => 1,
                Op::Neg | Op::Not | Op::F1(_) => 0,
                Op::F2(_) => -1,
                Op::F3(_) => -2,
            };
            max = max.max(depth);
        }
        if max as usize > DEPTH {
            return Err(fail("nested too deeply".into()));
        }
        let e = Expr(p.ops);
        if e.0.iter().any(|o| matches!(o, Op::Var(_))) {
            return Ok(e);
        }
        Ok(Expr::constant(e.eval(&Vars::default())))
    }

    /// The value, when the expression has no variables.
    pub fn as_const(&self) -> Option<f32> {
        match self.0[..] {
            [Op::Const(c)] => Some(c),
            _ => None,
        }
    }

    /// The value for one frame's context.
    pub fn eval(&self, v: &Vars) -> f32 {
        let mut st = [0.0f32; DEPTH];
        let mut n = 0;
        for op in &self.0 {
            match *op {
                Op::Const(c) => {
                    st[n] = c;
                    n += 1;
                }
                Op::Var(i) => {
                    st[n] = v.slot[i as usize];
                    n += 1;
                }
                Op::Neg => st[n - 1] = -st[n - 1],
                Op::Not => st[n - 1] = truth(st[n - 1] == 0.0),
                Op::F1(f) => st[n - 1] = f(st[n - 1]),
                Op::F2(f) => {
                    n -= 1;
                    st[n - 1] = f(st[n - 1], st[n]);
                }
                Op::F3(f) => {
                    n -= 2;
                    st[n - 1] = f(st[n - 1], st[n], st[n + 1]);
                }
            }
        }
        if st[0].is_finite() { st[0] } else { 0.0 }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    scope: &'a Scope,
    ops: Vec<Op>,
}

type R = Result<(), String>;

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    /// Consumes `t` if it comes next.
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        let hit = self.s[self.i..].starts_with(t.as_bytes());
        if hit {
            self.i += t.len();
        }
        hit
    }

    /// Consumes `t` if it comes next and is not the start of the longer operator `longer`.
    fn eat_alone(&mut self, t: &str, longer: &str) -> bool {
        self.ws();
        !self.s[self.i..].starts_with(longer.as_bytes()) && self.eat(t)
    }

    fn or(&mut self) -> R {
        self.and()?;
        while self.eat("||") {
            self.and()?;
            self.ops.push(Op::F2(|a, b| truth(a != 0.0 || b != 0.0)));
        }
        Ok(())
    }

    fn and(&mut self) -> R {
        self.cmp()?;
        while self.eat("&&") {
            self.cmp()?;
            self.ops.push(Op::F2(|a, b| truth(a != 0.0 && b != 0.0)));
        }
        Ok(())
    }

    fn cmp(&mut self) -> R {
        self.sum()?;
        loop {
            let f: fn(f32, f32) -> f32 = if self.eat("<=") {
                |a, b| truth(a <= b)
            } else if self.eat(">=") {
                |a, b| truth(a >= b)
            } else if self.eat("==") {
                |a, b| truth(a == b)
            } else if self.eat("!=") {
                |a, b| truth(a != b)
            } else if self.eat("<") {
                |a, b| truth(a < b)
            } else if self.eat(">") {
                |a, b| truth(a > b)
            } else {
                return Ok(());
            };
            self.sum()?;
            self.ops.push(Op::F2(f));
        }
    }

    fn sum(&mut self) -> R {
        self.prod()?;
        loop {
            let f: fn(f32, f32) -> f32 = if self.eat("+") {
                |a, b| a + b
            } else if self.eat("-") {
                |a, b| a - b
            } else {
                return Ok(());
            };
            self.prod()?;
            self.ops.push(Op::F2(f));
        }
    }

    fn prod(&mut self) -> R {
        self.unary()?;
        loop {
            let f: fn(f32, f32) -> f32 = if self.eat("*") {
                |a, b| a * b
            } else if self.eat("/") {
                |a, b| a / b
            } else if self.eat("%") {
                |a, b| a % b
            } else {
                return Ok(());
            };
            self.unary()?;
            self.ops.push(Op::F2(f));
        }
    }

    fn unary(&mut self) -> R {
        if self.eat("-") {
            self.unary()?;
            self.ops.push(Op::Neg);
        } else if self.eat_alone("!", "!=") {
            self.unary()?;
            self.ops.push(Op::Not);
        } else {
            self.power()?;
        }
        Ok(())
    }

    fn power(&mut self) -> R {
        self.atom()?;
        if self.eat("^") {
            self.unary()?;
            self.ops.push(Op::F2(f32::powf));
        }
        Ok(())
    }

    fn atom(&mut self) -> R {
        self.ws();
        let Some(&c) = self.s.get(self.i) else {
            return Err("ends where a value is expected".into());
        };
        if c == b'(' {
            self.i += 1;
            self.or()?;
            return if self.eat(")") {
                Ok(())
            } else {
                Err("missing `)`".into())
            };
        }
        if c.is_ascii_digit() || c == b'.' {
            return self.number();
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            return self.name();
        }
        Err(format!(
            "unexpected {:?}",
            &String::from_utf8_lossy(&self.s[self.i..])
        ))
    }

    fn number(&mut self) -> R {
        let at = self.i;
        let digits = |p: &mut Self| {
            while p.s.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
        };
        digits(self);
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            digits(self);
        }
        if self.s.get(self.i).is_some_and(|c| matches!(c, b'e' | b'E')) {
            let sign = usize::from(matches!(self.s.get(self.i + 1), Some(b'+' | b'-')));
            if self
                .s
                .get(self.i + 1 + sign)
                .is_some_and(u8::is_ascii_digit)
            {
                self.i += 1 + sign;
                digits(self);
            }
        }
        let text = std::str::from_utf8(&self.s[at..self.i]).expect("ASCII");
        let mut v: f64 = text.parse().map_err(|_| format!("bad number {text:?}"))?;
        let unit = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            self.i += 1;
        }
        match &self.s[unit..self.i] {
            b"" => {}
            b"ms" => v /= 1000.0,
            b"k" => v *= 1000.0,
            u => {
                return Err(format!(
                    "unknown unit {:?} after {text}",
                    String::from_utf8_lossy(u)
                ));
            }
        }
        self.ops.push(Op::Const(v as f32));
        Ok(())
    }

    fn name(&mut self) -> R {
        let at = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[at..self.i]).expect("ASCII");
        if self.eat("(") {
            let Some(op) = function(name) else {
                return Err(format!("unknown function {name:?}"));
            };
            let arity = match op {
                Op::F1(_) => 1,
                Op::F2(_) => 2,
                _ => 3,
            };
            for k in 0..arity {
                if k > 0 && !self.eat(",") {
                    return Err(format!("{name} takes {arity} values"));
                }
                self.or()?;
            }
            if !self.eat(")") {
                return Err(format!("{name} takes {arity} values"));
            }
            self.ops.push(op);
            return Ok(());
        }
        match (name, self.scope.slot(name)) {
            (_, Some(i)) => self.ops.push(Op::Var(i)),
            ("pi", _) => self.ops.push(Op::Const(PI)),
            ("tau", _) => self.ops.push(Op::Const(TAU)),
            _ => return Err(format!("unknown variable {name:?}")),
        }
        Ok(())
    }
}
