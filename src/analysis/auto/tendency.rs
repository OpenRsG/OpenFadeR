//! The user's personal CUE tendency (CUE Analysis "Auto"), learnt from the cues they already set. It needs
//! at least 30 tracks with cues; the statistics here are ours
//! (**inferred**): where the first cue usually sits (bars from 1.1), the usual distance between hot cues, and how many
//! hot / memory cues a track usually has. [`super::cues`] applies the result.

use super::bar_times;
use crate::analysis::{Beat, Cue};

/// Tracks with a grid and at least one cue that Auto needs before it trusts the tendency.
pub const MIN_TRACKS: usize = 30;

/// A gap between hot cues must be at least this share of all gaps to count as "the usual".
const USUAL: f64 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tendency {
    /// Bars from 1.1 to the usual first cue (0 = the first bar).
    pub offset_bars: usize,
    /// The usual distance in bars between consecutive hot cues, when there is one.
    pub spacing_bars: Option<usize>,
    /// Usual number of hot / memory cues (of the tracks that have any of that kind).
    pub hot: u8,
    pub memory: u8,
    /// Tracks it was learnt from.
    pub tracks: usize,
}

/// The most common value (the smallest of equally common ones) and its share of `v`.
fn mode(v: &[usize]) -> Option<(usize, f64)> {
    let mut counts: Vec<(usize, usize)> = Vec::new();
    for &x in v {
        match counts.iter_mut().find(|c| c.0 == x) {
            Some(c) => c.1 += 1,
            None => counts.push((x, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    counts.first().map(|&(x, n)| (x, n as f64 / v.len() as f64))
}

/// The middle value (the lower one for an even count); 0 for none.
fn median(mut v: Vec<usize>) -> usize {
    v.sort_unstable();
    v.get(v.len().saturating_sub(1) / 2).copied().unwrap_or(0)
}

/// Index of the bar nearest to `ms` (cues before the first bar count as the first).
fn bar_of(bars: &[u32], ms: u32) -> usize {
    let i = bars.partition_point(|&b| b <= ms).saturating_sub(1);
    match bars.get(i + 1) {
        Some(&next) if ms - bars[i].min(ms) > (next - bars[i]) / 2 => i + 1,
        _ => i,
    }
}

/// The tendency of the `(grid, cues)` of a collection's tracks; `None` while fewer than [`MIN_TRACKS`] of them have
/// both a grid and cues. Loops and tracks without a grid are skipped.
pub fn derive<'a>(tracks: impl IntoIterator<Item = (&'a [Beat], &'a [Cue])>) -> Option<Tendency> {
    let (mut firsts, mut gaps, mut hots, mut mems, mut n) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0);
    for (beats, cues) in tracks {
        let bars = bar_times(beats);
        let mut at: Vec<(usize, bool)> = cues
            .iter()
            .filter(|c| c.loop_end_ms.is_none())
            .map(|c| (bar_of(&bars, c.time_ms), c.hot_cue > 0))
            .collect();
        if bars.is_empty() || at.is_empty() {
            continue;
        }
        n += 1;
        at.sort();
        firsts.push(at[0].0);
        let hot: Vec<usize> = at.iter().filter(|c| c.1).map(|c| c.0).collect();
        gaps.extend(hot.windows(2).map(|w| w[1] - w[0]).filter(|&g| g > 0));
        if !hot.is_empty() {
            hots.push(hot.len());
        }
        let memory = at.len() - hot.len();
        if memory > 0 {
            mems.push(memory);
        }
    }
    (n >= MIN_TRACKS).then(|| Tendency {
        offset_bars: mode(&firsts).map_or(0, |m| m.0),
        spacing_bars: mode(&gaps).filter(|m| m.1 >= USUAL).map(|m| m.0),
        hot: median(hots) as u8,
        memory: median(mems) as u8,
        tracks: n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Vec<Beat> {
        (0..256)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect()
    }
    fn cue(hot_cue: u16, bar: u32) -> Cue {
        Cue {
            hot_cue,
            time_ms: bar * 2000 + 30,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }
    /// A track whose user put hot cues A.. at `bars` and a memory cue at 1.1.
    fn track(bars: &[u32]) -> Vec<Cue> {
        std::iter::once(cue(0, 0))
            .chain(bars.iter().enumerate().map(|(i, &b)| cue(i as u16 + 1, b)))
            .collect()
    }

    #[test]
    fn learns_offset_spacing_and_counts() {
        let g = grid();
        let habit = track(&[4, 20, 36, 52, 68]);
        let odd = track(&[0, 8]);
        let tracks: Vec<Vec<Cue>> = (0..40)
            .map(|i| {
                if i % 10 == 0 {
                    odd.clone()
                } else {
                    habit.clone()
                }
            })
            .collect();
        let t = derive(tracks.iter().map(|c| (g.as_slice(), c.as_slice()))).unwrap();
        // the earliest cue is the memory cue at 1.1 in every track, hot cues start at bar 4
        assert_eq!(t.offset_bars, 0);
        assert_eq!(
            (t.spacing_bars, t.hot, t.memory, t.tracks),
            (Some(16), 5, 1, 40)
        );

        let no_memory: Vec<Vec<Cue>> = (0..40)
            .map(|_| {
                track(&[4, 20, 36])
                    .into_iter()
                    .filter(|c| c.hot_cue > 0)
                    .collect()
            })
            .collect();
        let t = derive(no_memory.iter().map(|c| (g.as_slice(), c.as_slice()))).unwrap();
        assert_eq!(
            (t.offset_bars, t.spacing_bars, t.hot, t.memory),
            (4, Some(16), 3, 0)
        );
    }

    #[test]
    fn needs_thirty_tracks_with_a_grid_and_cues() {
        let g = grid();
        let c = track(&[4, 20]);
        let none: Vec<Cue> = Vec::new();
        let enough = |with: usize, without: usize| {
            let t = (0..with)
                .map(|_| (g.as_slice(), c.as_slice()))
                .chain((0..without).map(|_| (g.as_slice(), none.as_slice())));
            derive(t)
        };
        assert!(
            enough(29, 100).is_none(),
            "tracks without cues do not count"
        );
        assert!(enough(30, 0).is_some());
        assert!(
            derive((0..50).map(|_| (&[][..], c.as_slice()))).is_none(),
            "nor do tracks without a grid"
        );
    }

    #[test]
    fn an_irregular_habit_has_no_usual_spacing() {
        let g = grid();
        let tracks: Vec<Vec<Cue>> = (0..30u32)
            .map(|i| track(&[4, 4 + 3 + i % 7, 40 + i % 5, 60 + i % 11]))
            .collect();
        let t = derive(tracks.iter().map(|c| (g.as_slice(), c.as_slice()))).unwrap();
        assert_eq!(t.spacing_bars, None);
    }

    #[test]
    fn cues_snap_to_the_nearest_bar() {
        let bars = [0, 2000, 4000];
        assert_eq!(
            [0, 900, 1100, 2000, 3500, 9000].map(|ms| bar_of(&bars, ms)),
            [0, 0, 1, 1, 2, 2]
        );
        assert_eq!(
            mode(&[3, 1, 1, 3]),
            Some((1, 0.5)),
            "ties go to the smaller value"
        );
        assert_eq!(median(vec![5, 1, 3, 9]), 3);
        assert_eq!(median(Vec::new()), 0);
    }
}
