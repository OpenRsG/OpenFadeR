//! Per-voice effects chain: 3-band isolator EQ ([`eq`], Linkwitz-Riley crossover) then a one-knob
//! filter ([`filter`]). Everything is built from RBJ cookbook low/high-pass sections (Audio EQ
//! Cookbook). Base: [`design`], [`Ramp`] (linear parameter ramp over a buffer, so a knob move never
//! clicks), [`Biquad`] (stereo, DF2 transposed, coefficients ramped) and [`Fx`], the chain. Interpolating two stable
//! denominators stays stable: the (a1, a2) stability region is a convex triangle.

pub mod eq;
mod filter;

pub use eq::{EQ_KILL_DB, EQ_MAX_DB};
pub use filter::DEAD;

/// `[b0, b1, b2, a1, a2]`, already divided by a0.
type Coef = [f32; 5];
const IDENTITY: Coef = [1.0, 0.0, 0.0, 0.0, 0.0];

#[derive(Clone, Copy)]
enum Shape {
    Low,
    High,
}

/// Cookbook second-order low-/high-pass at `f` Hz, quality `q` (computed in f64, narrowed once).
fn design(shape: Shape, fs: f64, f: f64, q: f64) -> Coef {
    let w = 2.0 * std::f64::consts::PI * f / fs;
    let (c, al) = (w.cos(), w.sin() / (2.0 * q));
    let a0 = 1.0 + al;
    let b = match shape {
        Shape::Low => [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
        Shape::High => [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
    };
    [b[0], b[1], b[2], -2.0 * c, 1.0 - al].map(|v| (v / a0) as f32)
}

/// Clamp a band gain to the EQ range; non-finite is 0 dB.
pub fn clamp_db(db: f32) -> f32 {
    super::clamped_in(db, EQ_KILL_DB, EQ_MAX_DB, 0.0)
}

/// `N` values moving linearly to a goal over a set number of frames, then landing on it exactly.
struct Ramp<const N: usize> {
    cur: [f32; N],
    goal: [f32; N],
    step: [f32; N],
    left: usize,
}

impl<const N: usize> Ramp<N> {
    fn new(v: [f32; N]) -> Self {
        Ramp {
            cur: v,
            goal: v,
            step: [0.0; N],
            left: 0,
        }
    }

    /// Reach `to` after `n` advances, starting from wherever the ramp is now.
    fn aim(&mut self, to: [f32; N], n: usize) {
        if to == self.goal {
            return;
        }
        let n = n.max(1);
        self.goal = to;
        self.left = n;
        self.step = std::array::from_fn(|i| (to[i] - self.cur[i]) / n as f32);
    }

    /// Moves one frame towards the goal.
    fn advance(&mut self) {
        if self.left == 0 {
            return;
        }
        self.left -= 1;
        for (c, s) in self.cur.iter_mut().zip(self.step) {
            *c += s;
        }
        if self.left == 0 {
            self.cur = self.goal;
        }
    }
}

struct Biquad {
    c: Ramp<5>,
    z: [[f32; 2]; 2], // [channel][z1, z2]
}

impl Biquad {
    fn new() -> Biquad {
        Biquad::fixed(IDENTITY)
    }

    /// Constant coefficients (crossover sections).
    fn fixed(c: Coef) -> Biquad {
        Biquad {
            c: Ramp::new(c),
            z: [[0.0; 2]; 2],
        }
    }

    fn clear(&mut self) {
        self.z = [[0.0; 2]; 2];
    }

    fn frame(&mut self, x: [f32; 2]) -> [f32; 2] {
        if self.c.left == 0 && self.c.cur == IDENTITY {
            return x;
        }
        self.c.advance();
        let [b0, b1, b2, a1, a2] = self.c.cur;
        let mut y = x;
        for (c, z) in self.z.iter_mut().enumerate() {
            y[c] = b0 * x[c] + z[0];
            z[0] = b1 * x[c] - a1 * y[c] + z[1];
            z[1] = b2 * x[c] - a2 * y[c];
        }
        y
    }
}

/// EQ then filter. Bit-exact pass-through while the EQ is flat (0 dB x3) and the filter is off.
pub struct Fx {
    eq: eq::Eq,
    filter: Biquad,
}

impl Fx {
    pub fn new() -> Fx {
        Fx {
            eq: eq::Eq::new(),
            filter: Biquad::new(),
        }
    }

    /// Aim the chain at new settings (`eq_db` per band, filter `x` in -1..=1), ramped over `n` frames.
    pub fn set(&mut self, fs: f64, eq_db: [f32; 3], x: f32, n: usize) {
        self.eq.aim(fs, eq_db, n);
        self.filter.c.aim(filter::coef(fs, x), n);
    }

    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let [l, r] = self.filter.frame(self.eq.frame([l, r]));
        (l, r)
    }
}

/// Steady-state gain in dB of `fx` (already `set`) for a sine at `f` Hz: RMS of the last half second over the input's.
#[cfg(test)]
pub(crate) fn measure_db(fx: &mut Fx, fs: f64, f: f64) -> f64 {
    let n = fs as usize;
    let (mut sin, mut sout) = (0.0f64, 0.0f64);
    for i in 0..n {
        let x = (0.5 * (2.0 * std::f64::consts::PI * f * i as f64 / fs).sin()) as f32;
        let (y, _) = fx.process(x, x);
        if i >= n / 2 {
            sin += (x as f64).powi(2);
            sout += (y as f64).powi(2);
        }
    }
    10.0 * (sout / sin).log10()
}
