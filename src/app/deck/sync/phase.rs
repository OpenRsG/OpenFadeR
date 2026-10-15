//! Tempo and beat-phase maths of SYNC: which rate a follower plays at, how far off the master's beats it is,
//! what to do about it. Pure, no deck state.

use crate::app::deck::{beat_at, ms_at};
use openfader::analysis::Beat;

/// Tempo multiples of the master a follower may play at (double / half time, 87 vs 174 BPM).
const MULTIPLES: [f64; 3] = [1.0, 2.0, 0.5];
/// Phase error (ms) that starts a nudge, and the smaller one it stops at (hysteresis).
const ENGAGE_MS: f64 = 10.0;
const RELEASE_MS: f64 = 3.0;
/// Above this the error is a jump, not drift: seek to the aligned spot.
const JUMP_MS: f64 = 80.0;
/// Largest nudge (fraction of the rate) and the seconds it takes to cancel an error.
const MAX_NUDGE: f64 = 0.04;
const CLOSE_S: f64 = 1.5;

/// Rate for a deck with grid tempo `own_grid` to play at a multiple `k` of `master_bpm`, `k` picked
/// so the rate stays closest to 1 within 0.5 to 2× (SYNC never follows below half speed). `half_double` off (Preferences > Controller >
/// BEAT/BPM SYNC) allows only `k` = 1. `None` without a tempo or in range.
pub fn target(own_grid: f64, master_bpm: f64, half_double: bool) -> Option<(f64, f64)> {
    if own_grid <= 0.0 || master_bpm <= 0.0 {
        return None;
    }
    let multiples = if half_double {
        &MULTIPLES[..]
    } else {
        &MULTIPLES[..1]
    };
    multiples
        .iter()
        .map(|&k| (master_bpm * k / own_grid, k))
        .filter(|(r, _)| (0.5..=2.0).contains(r))
        .min_by(|a, b| a.0.ln().abs().total_cmp(&b.0.ln().abs()))
}

/// `x` folded into -0.5..0.5 (the shortest way round a beat).
fn wrap(x: f64) -> f64 {
    (x + 0.5).rem_euclid(1.0) - 0.5
}

/// Beat-phase error of the follower at `own_pos` against the master at `master_pos` (ms, track time)
/// in the follower's beats (positive = ahead), and the position (ms) where it would be aligned.
pub fn align(
    own: &[Beat],
    own_pos: f64,
    master: &[Beat],
    master_pos: f64,
    k: f64,
) -> Option<(f64, f64)> {
    let have = beat_at(own, own_pos)?;
    let err = wrap(have - beat_at(master, master_pos)? * k);
    Some((err, ms_at(own, have - err)?))
}

/// An error in beats as audible milliseconds at the follower's effective tempo.
pub fn error_ms(err_beats: f64, eff_bpm: f64) -> f64 {
    err_beats * 60_000.0 / eff_bpm
}

/// What the phase controller does about an error.
#[derive(Debug, PartialEq)]
pub enum Fix {
    /// Rate offset to add (0 = none); negative when ahead.
    Nudge(f32),
    Seek,
}

/// `nudging`: the last tick already applied a nudge (it keeps going down to [`RELEASE_MS`]).
pub fn correct(err_ms: f64, nudging: bool) -> Fix {
    let a = err_ms.abs();
    if a > JUMP_MS {
        Fix::Seek
    } else if a > ENGAGE_MS || (nudging && a > RELEASE_MS) {
        Fix::Nudge(nudge_for(err_ms))
    } else {
        Fix::Nudge(0.0)
    }
}

/// Proportional pull: cancels the error in about [`CLOSE_S`], at most [`MAX_NUDGE`].
fn nudge_for(err_ms: f64) -> f32 {
    (-err_ms / 1000.0 / CLOSE_S).clamp(-MAX_NUDGE, MAX_NUDGE) as f32
}

/// Where a paused follower at `own_pos` starts so it sits on its nearest beat plus the master's current
/// beat phase (called as the master crosses a beat: the phase is then a few ms).
pub fn start_ms(
    own: &[Beat],
    own_pos: f64,
    master: &[Beat],
    master_pos: f64,
    k: f64,
) -> Option<u32> {
    let b = beat_at(own, own_pos)?.round();
    Some(
        ms_at(own, b + wrap(beat_at(master, master_pos)? * k))?
            .max(0.0)
            .round() as u32,
    )
}

/// The master's phase in follower beats, 0..1; it wraps when the follower's beat comes round.
pub fn wait_phase(master: &[Beat], master_pos: f64, k: f64) -> Option<f64> {
    Some((beat_at(master, master_pos)? * k).rem_euclid(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` beats from `start` ms at `bpm` (must divide 60000 evenly).
    fn grid(start: u32, bpm: u16, n: u32) -> Vec<Beat> {
        let d = 60_000 / bpm as u32;
        (0..n)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: bpm * 100,
                time_ms: start + i * d,
            })
            .collect()
    }

    #[test]
    fn tempo_matches_the_masters_effective_bpm() {
        let (rate, k) = target(120.0, 150.0, true).unwrap();
        assert!((rate - 1.25).abs() < 1e-9 && k == 1.0);
        // the master at +4%: 150 × 1.04 = 156 BPM → 120 × rate
        assert!((120.0 * target(120.0, 156.0, true).unwrap().0 - 156.0).abs() < 1e-9);
    }

    #[test]
    fn half_and_double_time_are_matched_not_stretched() {
        // 87 follows 174 at half the master tempo (rate 1, not 2)
        let (rate, k) = target(87.0, 174.0, true).unwrap();
        assert!((rate - 1.0).abs() < 1e-9 && k == 0.5);
        // 174 follows 87 at double
        let (rate, k) = target(174.0, 87.0, true).unwrap();
        assert!((rate - 1.0).abs() < 1e-9 && k == 2.0);
        // 70 vs 128: 2 × 70 = 140 is nearer to 128 than 70 itself
        let (rate, k) = target(128.0, 70.0, true).unwrap();
        assert!(k == 2.0 && (rate - 140.0 / 128.0).abs() < 1e-9);
    }

    #[test]
    fn without_half_double_only_the_plain_tempo_is_followed() {
        let (rate, k) = target(87.0, 174.0, false).unwrap();
        assert!(
            (rate - 2.0).abs() < 1e-9 && k == 1.0,
            "174 over 87 is 2×, not half time"
        );
        assert!(target(10.0, 150.0, false).is_none());
        assert!(
            target(100.0, 250.0, false).is_none(),
            "2.5× is out of range and no multiple helps"
        );
    }

    #[test]
    fn no_tempo_or_out_of_range_is_none() {
        assert!(target(0.0, 120.0, true).is_none() && target(120.0, 0.0, true).is_none());
        assert!(
            target(10.0, 150.0, true).is_none(),
            "no multiple lands in 0.5 to 2×"
        );
    }

    #[test]
    fn phase_error_is_the_shortest_way_round_a_beat() {
        // own 120 BPM (500 ms beats) from 100 ms; master 150 BPM (400 ms) from 0; same tempo domain k = 1
        let (own, master) = (grid(100, 120, 64), grid(0, 150, 64));
        // own 2000 ms = beat 3.8, master 1000 ms = beat 2.5 → ahead by 0.3 beats; aligned at beat 3.5 = 1850 ms
        let (err, ms) = align(&own, 2000.0, &master, 1000.0, 1.0).unwrap();
        assert!((err - 0.3).abs() < 1e-9 && (ms - 1850.0).abs() < 1e-6);
        // own beat 4.0 (2100 ms) against master beat 3.0 (1200 ms): aligned
        assert!(align(&own, 2100.0, &master, 1200.0, 1.0).unwrap().0.abs() < 1e-9);
        // own phase .1 against master phase .9: ahead by 0.2 across the beat, not behind by 0.8
        let (err, _) = align(&own, 100.0 + 5.1 * 500.0, &master, 0.9 * 400.0, 1.0).unwrap();
        assert!((err - 0.2).abs() < 1e-9);
    }

    #[test]
    fn phase_follows_the_half_and_double_time_multiple() {
        let (own, master) = (grid(0, 120, 64), grid(0, 120, 64));
        // k = 2: the follower runs two beats per master beat, so master beat 1.25 wants own phase .5
        let (err, _) = align(&own, 0.5 * 500.0, &master, 1.25 * 500.0, 2.0).unwrap();
        assert!(err.abs() < 1e-9);
        // k = ½: master beat 3 wants own phase .5
        let (err, _) = align(&own, 0.5 * 500.0, &master, 3.0 * 500.0, 0.5).unwrap();
        assert!(err.abs() < 1e-9);
    }

    #[test]
    fn error_in_ms_uses_the_effective_tempo() {
        assert!((error_ms(0.1, 120.0) - 50.0).abs() < 1e-9);
        assert!((error_ms(-0.5, 60.0) + 500.0).abs() < 1e-9);
    }

    #[test]
    fn small_drift_nudges_with_hysteresis_and_big_errors_seek() {
        assert_eq!(correct(5.0, false), Fix::Nudge(0.0), "inside the dead band");
        assert_eq!(correct(10.0, false), Fix::Nudge(0.0));
        assert!(
            matches!(correct(20.0, false), Fix::Nudge(d) if d < 0.0),
            "ahead: slow down"
        );
        assert!(
            matches!(correct(-20.0, false), Fix::Nudge(d) if d > 0.0),
            "behind: speed up"
        );
        assert_eq!(
            correct(5.0, true),
            Fix::Nudge(nudge_for(5.0)),
            "an active nudge keeps going until 3 ms"
        );
        assert_eq!(correct(2.0, true), Fix::Nudge(0.0));
        assert_eq!(correct(81.0, false), Fix::Seek);
        assert_eq!(correct(-300.0, true), Fix::Seek);
    }

    #[test]
    fn nudge_is_tiny_and_capped() {
        let Fix::Nudge(d) = correct(50.0, false) else {
            panic!()
        };
        assert!(d < 0.0 && d.abs() < 0.04);
        assert!((nudge_for(-30.0) - 0.02).abs() < 1e-6);
        assert!(nudge_for(10_000.0) >= -0.04 && nudge_for(-10_000.0) <= 0.04);
    }

    #[test]
    fn quantized_start_sits_on_a_beat_plus_the_masters_phase() {
        let (own, master) = (grid(100, 120, 64), grid(0, 150, 64));
        // paused at 2090 ms (beat 3.98 → beat 4 = 2100 ms); master just past a beat (1200 ms + 20 ms = phase .05)
        assert_eq!(start_ms(&own, 2090.0, &master, 1220.0, 1.0), Some(2125));
        // master phase .9 → 0.1 beat before the own beat
        assert_eq!(start_ms(&own, 2090.0, &master, 1160.0, 1.0), Some(2050));
        // never before the track start
        assert_eq!(start_ms(&own, 0.0, &master, 1000.0, 1.0), Some(0));
        assert_eq!(start_ms(&[], 0.0, &master, 0.0, 1.0), None);
    }

    #[test]
    fn the_wait_phase_wraps_when_the_masters_beat_comes_round() {
        let master = grid(0, 120, 64);
        let (a, b) = (
            wait_phase(&master, 480.0, 1.0).unwrap(),
            wait_phase(&master, 520.0, 1.0).unwrap(),
        );
        assert!(a > 0.9 && b < 0.1 && b < a);
        // double time: it wraps twice as often
        assert!((wait_phase(&master, 250.0, 2.0).unwrap()).abs() < 1e-9);
    }
}
