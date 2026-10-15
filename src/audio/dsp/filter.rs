//! One-knob DJ filter. x<0: low-pass swept exponentially 20 kHz -> 200 Hz at x=-1; x>0: high-pass
//! swept 20 Hz -> 5 kHz at x=+1; |x| < `DEAD` bypasses. 12 dB/oct, Q = 0.9 (no resonance sweep, no
//! extra character).

use super::{Coef, IDENTITY, Shape, design};

/// Knob dead zone: `|x|` below it is off (COLOR FX uses the same one, see [`super::super::cfx`]).
pub const DEAD: f32 = 0.03;
const Q: f64 = 0.9;

pub(super) fn coef(fs: f64, x: f32) -> Coef {
    if x.abs() < DEAD {
        return IDENTITY;
    }
    let (shape, from, to) = if x < 0.0 {
        (Shape::Low, 20000.0f64, 200.0f64)
    } else {
        (Shape::High, 20.0, 5000.0)
    };
    let f = (from * (to / from).powf(x.abs() as f64)).min(fs * 0.45);
    design(shape, fs, f, Q)
}

#[cfg(test)]
mod tests {
    use super::super::{Fx, measure_db};

    const FS: f64 = 48000.0;

    fn response(x: f32, f: f64) -> f64 {
        let mut fx = Fx::new();
        fx.set(FS, [0.0; 3], x, 1);
        measure_db(&mut fx, FS, f)
    }

    #[test]
    fn low_pass_at_minus_one_cuts_5k_by_more_than_20_db() {
        let g = response(-1.0, 5000.0);
        assert!(g < -20.0, "{g:.1} dB");
        assert!(response(-1.0, 50.0).abs() < 1.0); // below the 200 Hz corner: passes
    }

    #[test]
    fn high_pass_at_plus_one_cuts_100_hz_and_passes_10k() {
        assert!(response(1.0, 100.0) < -20.0);
        assert!(response(1.0, 10000.0).abs() < 1.0);
    }

    #[test]
    fn centre_is_identity() {
        let mut fx = Fx::new();
        fx.set(FS, [0.0; 3], 0.0, 512);
        assert!(
            (0..1000).all(|i| fx.process(i as f32, 1.0 - i as f32) == (i as f32, 1.0 - i as f32))
        );
        let mut fx = Fx::new();
        fx.set(FS, [0.0; 3], 0.02, 512); // inside the dead zone
        assert_eq!(fx.process(0.7, -0.3), (0.7, -0.3));
    }

    #[test]
    fn sweep_is_monotonic() {
        let g: Vec<f64> = [-0.2, -0.5, -0.8, -1.0]
            .iter()
            .map(|&x| response(x, 5000.0))
            .collect();
        assert!(g.windows(2).all(|w| w[1] < w[0]), "{g:?}");
    }
}
