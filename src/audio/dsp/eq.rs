//! 3-band isolator EQ on a Linkwitz-Riley crossover (Linkwitz-Riley = two cascaded Butterworth
//! cookbook sections, Q = 1/sqrt 2). x -> low = LP4(200 Hz); rest = HP4(200 Hz) -> mid = LP4(3 kHz),
//! high = HP4(3 kHz); out = gl*low + gm*mid + gh*high. The bands sum to an all-pass of x (flat
//! magnitude), so a gain of 0 removes a band outright, with 24 dB/oct skirts: `EQ_KILL_DB` is a real
//! kill, not -26 dB. Corner choice (200 Hz, 3 kHz; 3 kHz rather than 4 so 10 kHz is >35 dB down when
//! the high band is killed) is a layout typical of DJ mixers. The sections
//! never change coefficients (only per-sample linear gain ramps), so a knob move cannot click.
//!
//! While flat (0 dB x3) the crossover is bypassed: `x` comes out bit-exact. Leaving/entering the
//! bypass is a linear dry/wet crossfade over one buffer (the all-pass phase shift never switches
//! abruptly), and the sections' state is cleared when the bypass resumes.

use super::{Biquad, Ramp, Shape, design};
use std::f64::consts::FRAC_1_SQRT_2;

pub const EQ_KILL_DB: f32 = -26.0;
pub const EQ_MAX_DB: f32 = 6.0;
/// Crossover corners in Hz.
const LOW_MID: f64 = 200.0;
const MID_HIGH: f64 = 3000.0;

pub(super) struct Eq {
    fs: f64,
    sec: [Biquad; 8], // low: 0,1; rest: 2,3; mid: 4,5; high: 6,7
    /// Band gains low, mid, high, then the wet share: 0 = dry (bypass), 1 = full crossover.
    ramp: Ramp<4>,
}

fn gain(db: f32) -> f32 {
    if db <= EQ_KILL_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

impl Eq {
    pub(super) fn new() -> Eq {
        Eq {
            fs: 0.0,
            sec: std::array::from_fn(|_| Biquad::new()),
            ramp: Ramp::new([1.0, 1.0, 1.0, 0.0]),
        }
    }

    /// Aim at `db` per band (low, mid, high), reached after `n` processed frames.
    pub(super) fn aim(&mut self, fs: f64, db: [f32; 3], n: usize) {
        if fs != self.fs {
            self.fs = fs;
            let sec = |shape, f| design(shape, fs, f, FRAC_1_SQRT_2);
            let coefs = [
                sec(Shape::Low, LOW_MID),
                sec(Shape::High, LOW_MID),
                sec(Shape::Low, MID_HIGH),
                sec(Shape::High, MID_HIGH),
            ];
            for (i, b) in self.sec.iter_mut().enumerate() {
                *b = Biquad::fixed(coefs[i / 2]); // each LR4 = the same section twice
            }
        }
        let [gl, gm, gh] = db.map(gain);
        self.ramp
            .aim([gl, gm, gh, if db == [0.0; 3] { 0.0 } else { 1.0 }], n);
    }

    fn run(&mut self, first: usize, x: [f32; 2]) -> [f32; 2] {
        let y = self.sec[first].frame(x);
        self.sec[first + 1].frame(y)
    }

    pub(super) fn frame(&mut self, x: [f32; 2]) -> [f32; 2] {
        if self.ramp.cur[3] == 0.0 && self.ramp.left == 0 {
            return x;
        }
        self.ramp.advance();
        let [gl, gm, gh, wet] = self.ramp.cur;
        let low = self.run(0, x);
        let rest = self.run(2, x);
        let mid = self.run(4, rest);
        let high = self.run(6, rest);
        let mut y = [0.0; 2];
        for c in 0..2 {
            let band = gl * low[c] + gm * mid[c] + gh * high[c];
            y[c] = if wet >= 1.0 {
                band
            } else {
                x[c] + wet * (band - x[c])
            };
        }
        if wet == 0.0 {
            self.sec.iter_mut().for_each(Biquad::clear); // stale state must not leak into the next engage
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Fx, clamp_db, measure_db};
    use super::*;

    const FS: f64 = 48000.0;
    const PROBES: [f64; 3] = [50.0, 1000.0, 10000.0];

    fn response(eq: [f32; 3], f: f64) -> f64 {
        let mut fx = Fx::new();
        fx.set(FS, eq, 0.0, 1);
        measure_db(&mut fx, FS, f)
    }

    #[test]
    fn flat_is_bit_exact() {
        let mut fx = Fx::new();
        fx.set(FS, [0.0; 3], 0.0, 512);
        assert!((0..1000).all(|i| fx.process(i as f32 * 1e-3, -(i as f32)) == (i as f32 * 1e-3, -(i as f32))));
    }

    #[test]
    fn flat_crossover_has_flat_magnitude() {
        // engaged but all gains 1 (a 0.0 dB neighbour of a tiny nonzero value): the all-pass sum
        for f in [50.0, 200.0, 1000.0, 3000.0, 10000.0] {
            let g = response([0.001, 0.0, 0.0], f);
            assert!(g.abs() < 0.3, "{f} Hz: {g:.2} dB");
        }
    }

    #[test]
    fn kill_removes_its_band_and_leaves_the_others() {
        for band in 0..3 {
            let mut eq = [0.0; 3];
            eq[band] = EQ_KILL_DB;
            let g = response(eq, PROBES[band]);
            assert!(g < -35.0, "band {band} at {} Hz: {g:.1} dB", PROBES[band]);
            for f in PROBES
                .iter()
                .enumerate()
                .filter(|&(o, _)| o != band)
                .map(|(_, f)| f)
            {
                let o = response(eq, *f);
                assert!(o.abs() < 0.5, "kill {band} moved {f} Hz by {o:.2} dB");
            }
        }
    }

    #[test]
    fn boost_is_plus_six_at_the_band() {
        for band in 0..3 {
            let mut eq = [0.0; 3];
            eq[band] = EQ_MAX_DB;
            let g = response(eq, PROBES[band]);
            assert!(
                (g - 6.0).abs() < 0.5,
                "band {band} at {} Hz: {g:.2} dB",
                PROBES[band]
            );
        }
    }

    #[test]
    fn range_is_clamped() {
        assert_eq!(clamp_db(-100.0), EQ_KILL_DB);
        assert_eq!(clamp_db(40.0), EQ_MAX_DB);
        assert_eq!(clamp_db(f32::NAN), 0.0);
    }

    #[test]
    fn knob_moves_ramp_without_a_step_and_return_to_exact_bypass() {
        let mut fx = Fx::new();
        let x = |i: usize| 0.5 * (2.0 * std::f64::consts::PI * 100.0 * i as f64 / FS).sin() as f32;
        let (mut last, mut worst) = (0.0f32, 0.0f32);
        for i in 0..72000 {
            match i {
                12000 => fx.set(FS, [EQ_KILL_DB, 0.0, 6.0], 0.0, 512), // flat -> engaged
                36000 => fx.set(FS, [EQ_MAX_DB, 0.0, 0.0], 0.0, 512),  // kill -> boost
                54000 => fx.set(FS, [0.0; 3], 0.0, 512),               // back to flat
                _ => {}
            }
            let (y, _) = fx.process(x(i), x(i));
            worst = worst.max((y - last).abs());
            last = y;
            if i > 54000 + 600 {
                assert_eq!(y, x(i), "bypass is exact again at {i}");
            }
        }
        // 100 Hz at amplitude 0.5 moves <= 2*pi*100/48000*0.5 = 0.0065 per sample, gain up to x2 and ramp slop
        assert!(worst < 0.03, "step {worst}");
    }
}
