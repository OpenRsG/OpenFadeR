//! Where the deck is on its beat grid, frame by frame, and how the drum loops follow it.
//!
//! The grid is the player's beat list (beat times in track ms, ascending); the beat number at a source frame is linear between
//! two beats and extrapolated with the first / last interval outside the grid, the same rule as the deck's own
//! `beat_at`. A [`Tracker`] turns the deck's source position into the beats a loop has to advance: ordinary playback moves the
//! position by a few frames per output frame and the loop advances by exactly the beats the grid advanced. A jump (a seek, a loop
//! wrap, a hot cue) moves it by many frames: a jump of a whole number of beats (quantize on) costs the loop nothing, the loop
//! keeps counting and only the sub-beat part of the jump is followed, so the loop stays on the grid; a jump that lands off the
//! grid by more than [`TOL`] (quantize off, a click on the waveform) cannot be followed: BEAT SYNC to the deck is lost.

/// Source frames per step above which the move is a jump, not playback (a 2x deck moves ~2 frames per output frame).
pub const JUMP: f64 = 16.0;
/// Beats a jump may miss the grid by and still keep the loops in sync (about 23 ms at 128 BPM).
pub const TOL: f64 = 0.05;

/// Walks the beat list: a frame's position is almost always in the segment of the previous one.
#[derive(Default)]
struct Cursor(usize);

impl Cursor {
    /// `(beat number, beats per source frame)` at source frame `pos`; `None` for a grid of fewer than two beats or an empty segment.
    fn at(&mut self, beats: &[u32], rate: f64, pos: f64) -> Option<(f64, f64)> {
        let last = beats.len().checked_sub(2)?;
        let t = pos * 1000.0 / rate;
        let mut i = self.0.min(last);
        if beats[i] as f64 > t || beats[i + 1] as f64 <= t {
            i = beats
                .partition_point(|&b| b as f64 <= t)
                .saturating_sub(1)
                .min(last);
        }
        self.0 = i;
        let d = beats[i + 1] as f64 - beats[i] as f64;
        (d > 0.0).then(|| (i as f64 + (t - beats[i] as f64) / d, 1000.0 / (rate * d)))
    }
}

/// What one output frame of the deck did to the grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    /// Beat number at the frame, and beats per source frame there.
    pub beat: f64,
    pub dens: f64,
    /// Beats the loops advance by (0 for the first frame after a [`Tracker::reset`]).
    pub delta: f64,
    /// A whole beat boundary was passed by playing (not by a jump) since the previous frame.
    pub crossed: bool,
    /// A jump that missed the grid by more than [`TOL`] beats: the loops cannot stay in sync.
    pub lost: bool,
}

#[derive(Default)]
pub struct Tracker {
    cur: Cursor,
    last: Option<(f64, f64)>,
}

impl Tracker {
    /// The next frame is the first of a new run (the deck paused, sync was lost).
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// The deck's frame at source position `pos`; `None` without a usable grid.
    pub fn step(&mut self, beats: &[u32], rate: f64, pos: f64) -> Option<Tick> {
        let (beat, dens) = self.cur.at(beats, rate, pos)?;
        let (delta, crossed, lost) = match self.last.replace((pos, beat)) {
            None => (0.0, false, false),
            Some((p, b)) if (pos - p).abs() <= JUMP => (beat - b, beat.floor() != b.floor(), false),
            Some((_, b)) => {
                let off = beat - b - (beat - b).round();
                (off, false, off.abs() > TOL)
            }
        };
        Some(Tick {
            beat,
            dens,
            delta,
            crossed,
            lost,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 120 BPM from 100 ms: a beat every 500 ms.
    fn grid() -> Vec<u32> {
        (0..40).map(|i| 100 + 500 * i).collect()
    }

    #[test]
    fn beat_numbers_follow_the_grid_and_extrapolate() {
        let (g, mut c) = (grid(), Cursor::default());
        let at = |c: &mut Cursor, ms: f64| c.at(&g, 1000.0, ms).unwrap();
        assert_eq!(at(&mut c, 100.0), (0.0, 0.002));
        assert_eq!(at(&mut c, 350.0).0, 0.5);
        assert!(
            (at(&mut c, 20_000.0).0 - 39.8).abs() < 1e-9,
            "past the last beat the last interval goes on"
        );
        assert_eq!(at(&mut c, -150.0).0, -0.5); // before the first: so does the first
        assert_eq!(at(&mut c, 600.0).0, 1.0); // and it finds its way back
        assert!(
            Cursor::default().at(&[500], 1000.0, 0.0).is_none()
                && Cursor::default().at(&[7, 7], 1000.0, 0.0).is_none()
        );
    }

    #[test]
    fn playing_advances_by_the_grid_and_flags_beat_boundaries() {
        let (g, mut t) = (grid(), Tracker::default());
        let first = t.step(&g, 1000.0, 90.0).unwrap();
        assert_eq!((first.delta, first.crossed), (0.0, false));
        let next = t.step(&g, 1000.0, 91.0).unwrap();
        assert!((next.delta - 0.002).abs() < 1e-12 && !next.crossed);
        t.step(&g, 1000.0, 99.0);
        let on = t.step(&g, 1000.0, 100.0).unwrap();
        assert!(
            on.crossed && on.beat.abs() < 1e-12,
            "beat 0 starts at 100 ms: {on:?}"
        );
        assert!(!t.step(&g, 1000.0, 101.0).unwrap().crossed);
    }

    #[test]
    fn a_whole_beat_jump_keeps_sync_and_a_sub_beat_miss_is_followed() {
        let (g, mut t) = (grid(), Tracker::default());
        t.step(&g, 1000.0, 1000.0);
        let j = t.step(&g, 1000.0, 3000.0).unwrap(); // 4 beats on
        assert_eq!((j.crossed, j.lost), (false, false));
        assert!(
            j.delta.abs() < 1e-9,
            "the loops do not move for a whole-beat jump: {}",
            j.delta
        );
        let back = t.step(&g, 1000.0, 1010.0).unwrap(); // 3.98 beats back: the whole 4 are free, 0.02 is followed
        assert!(!back.lost && (back.delta - 0.02).abs() < 1e-9, "{back:?}");
    }

    #[test]
    fn a_jump_off_the_grid_loses_sync() {
        let (g, mut t) = (grid(), Tracker::default());
        t.step(&g, 1000.0, 1000.0);
        assert!(
            t.step(&g, 1000.0, 1000.0 + 5.0 * 500.0 + 250.0)
                .unwrap()
                .lost,
            "half a beat off"
        );
        t.reset();
        assert!(
            !t.step(&g, 1000.0, 5000.0).unwrap().lost,
            "a reset starts a new run"
        );
        assert!(t.step(&[], 1000.0, 1.0).is_none());
    }
}
