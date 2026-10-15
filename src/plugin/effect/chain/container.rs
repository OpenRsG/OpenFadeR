//! The blocks that hold other blocks: `feedback`, `parallel`, `crossover`.

use super::{Args, Block, Chain, Expr, Frame, MAX_FEEDBACK, R, Vars, add, scale};
use openfader_plugin::dsp::Svf;
use std::f32::consts::FRAC_1_SQRT_2;

/// `feedback`: `chain` with a loop around it, for loops that are not a plain delay (`delay` has its own `feedback`).
/// The output is what `chain` makes of (input + the loop signal); the loop signal is the output run through `loop` and
/// times `gain` (limited to ±0.99), one frame late. A reverb whose tail is pitch-shifted and fed back is a shimmer.
pub struct Feedback {
    gain: Expr,
    chain: Chain,
    back: Chain,
    last: Frame,
}

impl Feedback {
    pub fn parse(a: &mut Args) -> R<Feedback> {
        let gain = a.expr("gain", "0.5")?;
        if a.map.get("chain").is_none() {
            return a.fail("needs a \"chain\" of blocks");
        }
        Ok(Feedback {
            gain,
            chain: a.blocks("chain")?,
            back: a.blocks("loop")?,
            last: [0.0; 2],
        })
    }
}

impl Block for Feedback {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let y = self.chain.tick(add(x, self.last), v);
        let g = self.gain.eval(v).clamp(-MAX_FEEDBACK, MAX_FEEDBACK);
        self.last = scale(self.back.tick(y, v), g);
        y
    }

    fn reset(&mut self) {
        self.chain.reset();
        self.back.reset();
        self.last = [0.0; 2];
    }

    fn engage(&mut self) {
        self.chain.engage();
        self.back.engage();
    }
}

/// `parallel`: every list of `branches` gets the same input; the output is the sum of what they make of it (an empty
/// list passes the input).
pub struct Parallel(Vec<Chain>);

impl Parallel {
    pub fn parse(a: &mut Args) -> R<Parallel> {
        let Some(serde_json::Value::Array(list)) = a.raw("branches") else {
            return a.fail("needs \"branches\": a list of lists of blocks");
        };
        let at = format!("{}.branches", a.at);
        let branches = list
            .iter()
            .enumerate()
            .map(|(i, b)| a.chain_of(b, "branches", &format!("{at}[{i}]")))
            .collect::<R<Vec<_>>>()?;
        Ok(Parallel(branches))
    }
}

impl Block for Parallel {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        self.0
            .iter_mut()
            .fold([0.0; 2], |sum, c| add(sum, c.tick(x, v)))
    }

    fn reset(&mut self) {
        self.0.iter_mut().for_each(Chain::reset);
    }

    fn engage(&mut self) {
        self.0.iter_mut().for_each(Chain::engage);
    }
}

/// `crossover`: splits the signal at `freq` Hz into lows (a 12 dB low-pass) and highs (the input minus the lows, so
/// the two sum back to the input exactly), runs them through `low` and `high` and adds the results.
pub struct Crossover {
    freq: Expr,
    q: f32,
    rate: f32,
    svf: Svf,
    low: Chain,
    high: Chain,
}

impl Crossover {
    pub fn parse(a: &mut Args) -> R<Crossover> {
        Ok(Crossover {
            freq: a.expr("freq", "150")?,
            q: a.num("q", FRAC_1_SQRT_2, 0.1, 50.0)?,
            rate: a.rate as f32,
            svf: Svf::default(),
            low: a.blocks("low")?,
            high: a.blocks("high")?,
        })
    }
}

impl Block for Crossover {
    fn tick(&mut self, x: Frame, v: &Vars) -> Frame {
        let low = self.svf.tick(x, self.freq.eval(v), self.q, self.rate).low;
        let high = [x[0] - low[0], x[1] - low[1]];
        add(self.low.tick(low, v), self.high.tick(high, v))
    }

    fn reset(&mut self) {
        self.svf.clear();
        self.low.reset();
        self.high.reset();
    }

    fn engage(&mut self) {
        self.low.engage();
        self.high.engage();
    }
}
