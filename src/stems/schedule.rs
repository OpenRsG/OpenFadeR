//! The order a track's segments are separated in (STEMS Analysis Process).
//!
//! - QUALITY ("Prioritize sound quality"): in order from the start of each track; a jump during the analysis may take
//!   time before a Stem can be played: segment 0, 1, 2…
//! - SPEED ("Prioritize speed"): the play positions and HOT CUEs first, so that even after a jump a Stem can be played
//!   right away: the play position and what follows it first, then each HOT CUE,
//!   then the rest of the track from the play position on, wrapping round to its start.
//!
//! A [`Schedule`] hands each segment out once; the engines (GPU, CPU) take the next one whenever they are free.

use crate::audio::Grid;

/// Segments ahead of the play position that are separated before anything else (about 20 s of audio: time to catch up
/// with the player, a seek or a loop).
const LEAD: usize = 3;

pub struct Schedule {
    grid: Grid,
    claimed: Vec<bool>,
    sequential: bool,
    /// Segments in the order they are wanted before the plain sweep, set by [`Schedule::focus`].
    wanted: Vec<usize>,
    /// Play position (segment) the sweep starts at.
    from: usize,
}

impl Schedule {
    pub fn new(grid: Grid, segments: usize, sequential: bool) -> Schedule {
        Schedule {
            grid,
            claimed: vec![false; segments],
            sequential,
            wanted: Vec::new(),
            from: 0,
        }
    }

    /// The segments that make the frame `at` playable: its own, and the one before it while `at` lies in their overlap.
    fn around(&self, at: usize) -> impl Iterator<Item = usize> {
        let n = self.claimed.len();
        let s = self.grid.segment_of(at, n);
        let shared = s > 0 && at - self.grid.start(s) < self.grid.overlap;
        std::iter::once(s).chain(shared.then(|| s - 1))
    }

    /// What SPEED separates first: the play position `play` (frames) with [`LEAD`] segments after it, then each of the `cues`
    /// (frames). Ignored by a QUALITY schedule. Segments already handed out stay done.
    pub fn focus(&mut self, play: usize, cues: &[usize]) {
        if self.sequential {
            return;
        }
        let n = self.claimed.len();
        let s = self.grid.segment_of(play, n);
        self.from = s;
        let mut w: Vec<usize> = self.around(play).chain((1..=LEAD).map(|i| s + i)).collect();
        for &c in cues {
            w.extend(self.around(c).chain([self.grid.segment_of(c, n) + 1]));
        }
        w.retain(|&s| s < n);
        self.wanted = w;
    }

    /// The next segment to separate, `None` when every one has been handed out.
    pub fn next(&mut self) -> Option<usize> {
        let n = self.claimed.len();
        let sweep = (self.from..n).chain(0..self.from.min(n));
        let s = self
            .wanted
            .iter()
            .copied()
            .chain(sweep)
            .find(|&s| !self.claimed[s])?;
        self.claimed[s] = true;
        Some(s)
    }

    /// Gives segment `s` back (its engine failed): it is handed out again.
    pub fn release(&mut self, s: usize) {
        self.claimed[s] = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 segments of 100 frames overlapping by 25 (stride 75).
    const G: Grid = Grid {
        len: 100,
        overlap: 25,
    };

    fn order(mut s: Schedule) -> Vec<usize> {
        std::iter::from_fn(|| s.next()).collect()
    }

    #[test]
    fn quality_goes_from_the_start_whatever_the_play_position() {
        let mut s = Schedule::new(G, 20, true);
        s.focus(75 * 9, &[75 * 15]);
        assert_eq!(order(s), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn speed_starts_at_the_play_position_then_the_cues_then_sweeps_forward_and_wraps() {
        let mut s = Schedule::new(G, 20, false);
        s.focus(75 * 9 + 40, &[75 * 15 + 40]); // play inside segment 9's own part, one cue in 15
        let o = order(s);
        assert_eq!(o[..4], [9, 10, 11, 12], "play position, then the lead");
        assert_eq!(o[4..6], [15, 16], "the HOT CUE and its next");
        assert_eq!(
            o[6..],
            [13, 14, 17, 18, 19, 0, 1, 2, 3, 4, 5, 6, 7, 8],
            "then the sweep from the play position, wrapped"
        );
    }

    #[test]
    fn a_play_position_in_the_overlap_also_needs_the_previous_segment() {
        let mut s = Schedule::new(G, 20, false);
        s.focus(75 * 9 + 10, &[]); // frame 10 of segment 9, which segment 8 shares
        assert_eq!(order(s)[..3], [9, 8, 10]);
    }

    #[test]
    fn a_seek_reorders_what_is_left_and_nothing_is_handed_out_twice() {
        let mut s = Schedule::new(G, 20, false);
        s.focus(0, &[]);
        assert_eq!([s.next(), s.next()], [Some(0), Some(1)]);
        s.focus(75 * 12 + 40, &[]);
        assert_eq!([s.next(), s.next()], [Some(12), Some(13)]);
        let rest = order(s);
        assert_eq!(rest.len(), 16);
        assert!(rest.iter().all(|&s| s != 0 && s != 1 && s != 12 && s != 13));
    }

    #[test]
    fn the_end_of_a_short_track_is_clipped() {
        let mut s = Schedule::new(G, 2, false);
        s.focus(75 * 9, &[75 * 30]);
        assert_eq!(order(s), [1, 0]);
    }
}
