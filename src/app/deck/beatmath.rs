//! Beat-grid maths shared by the deck's children, re-exported from the base.

use openfader::analysis::Beat;

/// Grid tempo in BPM (first beat), `None` for an empty or zero-tempo grid.
pub(in crate::app) fn grid_bpm(beats: &[Beat]) -> Option<f64> {
    beats
        .first()
        .filter(|b| b.bpm_x100 > 0)
        .map(|b| b.bpm_x100 as f64 / 100.0)
}

/// Tempo in BPM of the grid section the playhead `ms` is in: the tempo of the beat it has passed (a beat carries the tempo of
/// the interval after it), the first section's before the first beat. `None` for an empty or zero-tempo grid.
pub(in crate::app) fn tempo_at(beats: &[Beat], ms: f64) -> Option<f64> {
    let i = beats
        .partition_point(|b| b.time_ms as f64 <= ms)
        .saturating_sub(1);
    beats
        .get(i)
        .filter(|b| b.bpm_x100 > 0)
        .map(|b| b.bpm_x100 as f64 / 100.0)
}

/// Two neighbouring beats are in different tempo sections when their `bpm_x100` differ by more than this (0.5 BPM): edits and
/// rounding wobble the tempo of single beats by far less.
const SECTION_STEP: u16 = 50;

/// The tempo sections of a grid: the runs of beats whose tempo stays within [`SECTION_STEP`] of the beat before, as beat index
/// ranges in order.
pub(super) fn sections(beats: &[Beat]) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        beats.get(at)?;
        let len = 1 + beats[at..]
            .windows(2)
            .take_while(|w| w[0].bpm_x100.abs_diff(w[1].bpm_x100) <= SECTION_STEP)
            .count();
        at += len;
        Some(at - len..at)
    })
}

/// Start (ms) and length of segment `seg..seg+1`; needs 2 beats and a positive interval.
fn segment(beats: &[Beat], seg: usize) -> Option<(f64, f64)> {
    let (a, b) = (
        beats.get(seg)?.time_ms as f64,
        beats.get(seg + 1)?.time_ms as f64,
    );
    (b > a).then_some((a, b - a))
}

/// Fractional beat number at `ms`: beat `i` sits at `beats[i].time_ms`, positions between beats
/// interpolate linearly, positions outside the grid extrapolate with the first / last interval.
pub(in crate::app) fn beat_at(beats: &[Beat], ms: f64) -> Option<f64> {
    let seg = beats
        .partition_point(|b| b.time_ms as f64 <= ms)
        .saturating_sub(1)
        .min(beats.len().checked_sub(2)?);
    let (t, d) = segment(beats, seg)?;
    Some(seg as f64 + (ms - t) / d)
}

/// Inverse of [`beat_at`].
pub(in crate::app) fn ms_at(beats: &[Beat], beat: f64) -> Option<f64> {
    let seg = (beat.floor().max(0.0) as usize).min(beats.len().checked_sub(2)?);
    let (t, d) = segment(beats, seg)?;
    Some(t + (beat - seg as f64) * d)
}

/// `ms` moved to the nearest multiple of `frac` beats (never before 0); unchanged (rounded) when the grid is unusable.
pub(super) fn snap(beats: &[Beat], ms: f64, frac: f64) -> u32 {
    let on_beat = beat_at(beats, ms).and_then(|b| ms_at(beats, (b / frac).round() * frac));
    on_beat.unwrap_or(ms).round().max(0.0) as u32
}

/// Beats spanned by `(s, e)` ms on the grid; `None` without a usable grid.
pub(super) fn beats_between(beats: &[Beat], (s, e): (u32, u32)) -> Option<f64> {
    Some(beat_at(beats, e as f64)? - beat_at(beats, s as f64)?)
}

/// Is a loop of `len` beats the `n`-beat loop (tolerant of ms rounding)?
pub(super) fn is_loop_len(len: f64, n: f64) -> bool {
    (len - n).abs() < 0.02 * n.max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::deck::tests::grid;

    #[test]
    fn snap_goes_to_the_nearest_beat() {
        let g = grid(1000, 4); // beats at 1000, 1500, 2000, 2500
        assert_eq!(
            [
                snap(&g, 1000.0, 1.0),
                snap(&g, 1240.0, 1.0),
                snap(&g, 1260.0, 1.0),
                snap(&g, 1750.0, 1.0)
            ],
            [1000, 1000, 1500, 2000]
        );
        // extrapolates with the last / first interval; never before 0
        assert_eq!(
            [
                snap(&g, 3100.0, 1.0),
                snap(&g, 3300.0, 1.0),
                snap(&g, 400.0, 1.0),
                snap(&g, 0.0, 1.0)
            ],
            [3000, 3500, 500, 0]
        );
    }

    #[test]
    fn snap_without_a_usable_grid_leaves_the_position() {
        assert_eq!(snap(&[], 1234.4, 1.0), 1234);
        assert_eq!(snap(&grid(0, 1), 1234.6, 1.0), 1235);
    }

    #[test]
    fn snap_goes_to_the_nearest_fraction_of_a_beat() {
        let g = grid(1000, 4);
        assert_eq!(
            [
                snap(&g, 1100.0, 0.25),
                snap(&g, 1130.0, 0.25),
                snap(&g, 1300.0, 0.5),
                snap(&g, 1400.0, 0.5)
            ],
            [1125, 1125, 1250, 1500]
        );
        assert_eq!(snap(&g, 1070.0, 1.0), 1000);
    }

    #[test]
    fn loop_length_tolerates_ms_rounding() {
        assert!(is_loop_len(4.01, 4.0) && is_loop_len(0.126, 0.125));
        assert!(!is_loop_len(3.0, 4.0) && !is_loop_len(0.25, 0.125));
    }

    #[test]
    fn tempo_follows_the_section_under_the_playhead() {
        let mut g = grid(0, 8); // 120 BPM
        g.extend((0..4).map(|i| Beat {
            bar_pos: 1 + i as u16,
            bpm_x100: 12800,
            time_ms: 4000 + i * 469,
        }));
        assert_eq!(tempo_at(&g, -10.0), Some(120.0), "before the first beat");
        assert_eq!(tempo_at(&g, 3999.0), Some(120.0));
        assert_eq!(tempo_at(&g, 4000.0), Some(128.0));
        assert_eq!(tempo_at(&g, 9e6), Some(128.0));
        assert_eq!(grid_bpm(&g), Some(120.0), "the browser BPM stays the first tempo");
        assert_eq!(sections(&g).collect::<Vec<_>>(), [0..8, 8..12]);
        assert_eq!(tempo_at(&[], 0.0), None);
    }
}
