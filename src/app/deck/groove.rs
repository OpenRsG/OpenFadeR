//! BEAT LAB hooks of a deck: what `app/groove` reads from it (the DRUM CAPTURE range, the tempo the DRUM RELEASE FX follows,
//! whether a DRUM SWAP loop has taken the drums). The deck does not know the panel; it only answers.

use super::{Deck, beat_at, ms_at, snap};
use openfader::analysis::Beat;

/// `n` beats from `start_ms` on the grid as `(start_ms, end_ms)`; fails without a grid or when the range leaves the track.
pub(super) fn capture_span(
    beats: &[Beat],
    start_ms: f64,
    n: u32,
    duration_ms: u32,
) -> Result<(f64, f64), String> {
    let end = beat_at(beats, start_ms)
        .and_then(|b| ms_at(beats, b + n as f64))
        .ok_or(tr!("the track has no beat grid"))?;
    if end > duration_ms as f64 + 0.5 {
        return Err(tr!("{} beats run past the end of the track", n));
    }
    Ok((start_ms, end))
}

impl Deck {
    /// The DRUM CAPTURE range of `n` beats (ms): from the loop-in point during a loop, else from the playhead, which Q moves to the nearest
    /// beat.
    pub(in crate::app) fn capture_range(&self, n: u32) -> Result<(f64, f64), String> {
        if !self.ready() {
            return Err(tr!("load a track first").into());
        }
        let l = self.loaded.borrow();
        let (pos, _) = self.clock(&l);
        let start = match l.loop_ {
            Some((s, _)) => s as f64,
            None if self.quantize.get() => snap(&l.analysis.beats, pos, 1.0) as f64,
            None => pos,
        };
        capture_span(&l.analysis.beats, start, n, l.duration_ms)
    }

    /// The deck's tempo as heard, BPM (0 = unknown): what the DRUM RELEASE FX follows.
    pub(in crate::app) fn effective_bpm(&self) -> f64 {
        self.loaded.borrow().base_bpm * self.rate.get()
    }

    /// A DRUM SWAP loop is playing in SINGLE mode: the DRUMS stem is off.
    pub(super) fn drums_swapped(&self) -> bool {
        self.with_player(|p| p.groove().swapping()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::grid;
    use super::*;

    #[test]
    fn a_capture_range_is_whole_beats_from_the_start_and_stays_in_the_track() {
        let g = grid(1000, 40); // 120 BPM: beats at 1000, 1500, …
        assert_eq!(capture_span(&g, 3000.0, 4, 60_000), Ok((3000.0, 5000.0)));
        assert_eq!(
            capture_span(&g, 3250.0, 8, 60_000),
            Ok((3250.0, 7250.0)),
            "an unquantized start keeps its offset"
        );
        assert!(capture_span(&g, 3000.0, 32, 10_000).is_err_and(|e| e.contains("past the end")));
        assert!(capture_span(&[], 0.0, 4, 10_000).is_err_and(|e| e.contains("no beat grid")));
    }
}
