//! Crossfader curves: crossfader position `x` (0 = deck A only, 1 = deck B only) → the two channel
//! gains, for the [`CrossfaderCurve`] setting. The shapes are our own choice:
//! - `Smooth`: linear, `[1 - x, x]` (gains sum to 1; −6 dB in the middle).
//! - `Sharp`: cut curve, a channel is at full gain everywhere except the last [`SHARP_EDGE`] of travel
//!   towards the other side, so both play across the middle.
//! - `Constant`: constant power, `[cos, sin]` of a quarter turn (gains² sum to 1); the default.

use crate::app::prefs::controller::{Fader, FaderCurve};
use crate::app::prefs::{Choice, CrossfaderCurve};
use openfader::audio::equal_power;

/// Fraction of the travel (from each end) over which `Sharp` moves a channel 0↔1.
const SHARP_EDGE: f32 = 0.1;

/// `[A, B]` gains at position `x` (clamped to 0..1).
pub fn sides(curve: CrossfaderCurve, x: f32) -> [f32; 2] {
    let x = x.clamp(0.0, 1.0);
    match curve {
        CrossfaderCurve::Smooth => [1.0 - x, x],
        CrossfaderCurve::Sharp => [((1.0 - x) / SHARP_EDGE).min(1.0), (x / SHARP_EDGE).min(1.0)],
        CrossfaderCurve::Constant => equal_power(x),
    }
}

/// Channel-fader gain at fader position `f` (0..1) for the CHANNEL FADER CURVE setting:
/// `Linear` g = f; `Steep` g = 1 - (1 - f)^3 (opens quickly, flat near the top); `Gradual` g = f^3 (stays low, opens late).
pub fn fader(curve: FaderCurve, f: f32) -> f32 {
    let f = f.clamp(0.0, 1.0);
    match curve {
        FaderCurve::Linear => f,
        FaderCurve::Steep => 1.0 - (1.0 - f).powi(3),
        FaderCurve::Gradual => f.powi(3),
    }
}

/// The curve factor of the channel gain for the CHANNEL FADER CURVE `sel`: the built-in curve, or 1 while a plugin curve is
/// selected (the plugin applies the fader gain in the channel's voice, `Player::fader`).
pub fn factor(sel: Fader, f: f32) -> f32 {
    sel.curve().map_or(1.0, |c| fader(c, f))
}

/// Position of `c` in [`Choice::ALL`] (`MixerState.curve`).
pub fn index(c: CrossfaderCurve) -> i32 {
    CrossfaderCurve::ALL
        .iter()
        .position(|&a| a == c)
        .unwrap_or(0) as i32
}

pub fn from_index(i: i32) -> CrossfaderCurve {
    usize::try_from(i)
        .ok()
        .and_then(|i| CrossfaderCurve::ALL.get(i))
        .copied()
        .unwrap_or_default()
}

pub fn next(c: CrossfaderCurve) -> CrossfaderCurve {
    from_index((index(c) + 1) % CrossfaderCurve::ALL.len() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_has_full_gain_on_its_own_side_and_silence_on_the_other() {
        for &c in CrossfaderCurve::ALL {
            assert_eq!(
                sides(c, 0.0).map(|g| (g * 1e6).round()),
                [1e6, 0.0],
                "{c:?}"
            );
            assert_eq!(
                sides(c, 1.0).map(|g| (g * 1e6).round()),
                [0.0, 1e6],
                "{c:?}"
            );
            assert_eq!(sides(c, -3.0), sides(c, 0.0)); // out-of-range positions clamp
            assert_eq!(sides(c, 3.0), sides(c, 1.0));
        }
    }

    #[test]
    fn shapes_at_the_middle_and_near_the_ends() {
        assert_eq!(sides(CrossfaderCurve::Smooth, 0.5), [0.5, 0.5]);
        assert_eq!(sides(CrossfaderCurve::Sharp, 0.5), [1.0, 1.0]); // both open across the middle
        let [a, b] = sides(CrossfaderCurve::Sharp, 0.05);
        assert!((a - 1.0).abs() < 1e-6 && (b - 0.5).abs() < 1e-6); // halfway through the cut edge
        let [a, b] = sides(CrossfaderCurve::Constant, 0.5);
        assert!((a - b).abs() < 1e-6 && (a - 0.7071).abs() < 1e-3);
        assert_eq!(sides(CrossfaderCurve::Constant, 0.3), equal_power(0.3));
    }

    #[test]
    fn gains_only_move_towards_the_selected_side() {
        for &c in CrossfaderCurve::ALL {
            let mut prev = sides(c, 0.0);
            for i in 1..=100 {
                let g = sides(c, i as f32 / 100.0);
                assert!(
                    g[0] <= prev[0] + 1e-6 && g[1] >= prev[1] - 1e-6,
                    "{c:?} at {i}"
                );
                prev = g;
            }
        }
        // constant power keeps gains² at 1, smooth keeps gains at 1
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            let [a, b] = sides(CrossfaderCurve::Constant, x);
            assert!((a * a + b * b - 1.0).abs() < 1e-5);
            let [a, b] = sides(CrossfaderCurve::Smooth, x);
            assert!((a + b - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn index_round_trips_and_cycles() {
        for &c in CrossfaderCurve::ALL {
            assert_eq!(from_index(index(c)), c);
        }
        assert_eq!(
            next(next(next(CrossfaderCurve::Smooth))),
            CrossfaderCurve::Smooth
        );
        assert_eq!(from_index(-1), CrossfaderCurve::Constant); // unknown → default
    }

    #[test]
    fn fader_curves_share_the_ends_and_differ_in_between() {
        for c in [FaderCurve::Linear, FaderCurve::Steep, FaderCurve::Gradual] {
            assert_eq!((fader(c, 0.0), fader(c, 1.0)), (0.0, 1.0), "{c:?}");
            assert_eq!((fader(c, -1.0), fader(c, 2.0)), (0.0, 1.0)); // out-of-range positions clamp
        }
        assert_eq!(fader(FaderCurve::Linear, 0.3), 0.3);
        assert!((fader(FaderCurve::Steep, 0.5) - 0.875).abs() < 1e-6);
        assert!((fader(FaderCurve::Gradual, 0.5) - 0.125).abs() < 1e-6);
        let mut prev = [0.0; 3];
        for i in 1..=100 {
            let g = [FaderCurve::Linear, FaderCurve::Steep, FaderCurve::Gradual]
                .map(|c| fader(c, i as f32 / 100.0));
            assert!(g.iter().zip(prev).all(|(a, b)| *a >= b)); // monotonic
            assert!(g[2] <= g[0] && g[0] <= g[1]); // gradual under linear under steep
            prev = g;
        }
    }

    #[test]
    fn a_plugin_curve_drops_the_curve_factor_from_the_channel_gain() {
        assert_eq!(factor(Fader::LINEAR, 0.3), 0.3);
        assert_eq!(factor(Fader(FaderCurve::ALL.len()), 0.3), 1.0);
    }
}
