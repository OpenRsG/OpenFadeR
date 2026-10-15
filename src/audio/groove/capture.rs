//! DRUM CAPTURE: a range of beats of the DRUMS stem of a deck's track, or of the original track (PREFS > Extensions > DRUM CAPTURE), as a
//! drum loop for a DRUM SWAP SLOT. The range is 4, 8, 16 or 32 beats from the playhead (from the loop-in point during a loop).

use super::super::{DRUMS, Pcm, Stems};
use super::Drum;

/// The extraction ranges in beats, in the order the range button steps through them.
pub const RANGES: [u32; 4] = [4, 8, 16, 32];

/// The range after `beats`: 4 → 8 → 16 → 32 → 4 (anything else starts again at 4).
pub fn next_range(beats: u32) -> u32 {
    RANGES
        .iter()
        .position(|&r| r == beats)
        .map_or(RANGES[0], |i| RANGES[(i + 1) % RANGES.len()])
}

/// Frames `start .. start + frames` of the DRUMS stem as stereo f32 at the track's `rate`. Fails when the range leaves the track or the
/// separation has not finished every part of it (the stems fill in while the track plays).
pub fn drums(stems: &Stems, rate: u32, start: usize, frames: usize) -> Result<Pcm, String> {
    let end = start
        .checked_add(frames)
        .filter(|&e| frames > 0 && e <= stems.frames());
    let Some(end) = end else {
        return Err("the range runs past the end of the track".into());
    };
    let step = stems.grid().stride().max(1);
    if !(start..end)
        .step_by(step)
        .chain([end - 1])
        .all(|i| stems.ready(i))
    {
        return Err("the DRUMS stem of this part is not separated yet".into());
    }
    let mut raw = vec![0i16; 2 * frames];
    stems.read(DRUMS, start, &mut raw);
    Ok(Pcm {
        rate,
        samples: raw.into_iter().map(|s| s as f32 / 32768.0).collect(),
    })
}

/// `pcm` as an `n`-beat drum loop: its tempo is the one that makes it exactly `n` beats long.
pub fn drum(pcm: Pcm, beats: u32) -> Result<Drum, String> {
    if pcm.rate == 0 || pcm.samples.len() < 2 {
        return Err("nothing was captured".into());
    }
    let secs = (pcm.samples.len() / 2) as f64 / pcm.rate as f64;
    Drum::new(pcm, (beats as f64 * 60.0 / secs) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Grid;

    #[test]
    fn the_range_button_cycles_4_8_16_32() {
        assert_eq!([4, 8, 16, 32, 4].map(next_range), [8, 16, 32, 4, 8]);
        assert_eq!(next_range(0), 4);
    }

    #[test]
    fn a_range_of_the_drums_stem_is_that_stem_not_the_mix() {
        let n = 1000;
        let stems = Stems::complete([
            vec![100; 2 * n],
            vec![200; 2 * n],
            vec![300; 2 * n],
            (0..2 * n as i16).collect(),
        ]);
        let c = drums(&stems, 44100, 10, 3).unwrap();
        assert_eq!(c.rate, 44100);
        assert_eq!(
            c.samples,
            [20, 21, 22, 23, 24, 25].map(|v| v as f32 / 32768.0)
        );
        assert!(
            drums(&stems, 44100, 998, 3).is_err() && drums(&stems, 44100, 0, 0).is_err(),
            "outside the track"
        );
    }

    #[test]
    fn a_range_the_separation_has_not_reached_is_refused() {
        let grid = Grid {
            len: 400,
            overlap: 100,
        };
        let stems = Stems::new(1000, grid);
        let part = |s: usize| {
            let len = (1000 - grid.start(s)).min(grid.len);
            [
                vec![0.1; 2 * len],
                vec![0.0; 2 * len],
                vec![0.0; 2 * len],
                vec![0.5; 2 * len],
            ]
        };
        let deposit = |s: usize| {
            let p = part(s);
            stems.deposit(s, [&p[0], &p[1], &p[2], &p[3]]);
        };
        deposit(0);
        assert!(
            drums(&stems, 8000, 0, 250).is_ok(),
            "the first segment is in"
        );
        assert!(drums(&stems, 8000, 200, 500).is_err_and(|e| e.contains("not separated")));
        (1..stems.segments()).for_each(deposit);
        assert!(
            drums(&stems, 8000, 200, 500).is_ok(),
            "once every segment is in"
        );
    }

    #[test]
    fn a_captured_range_is_a_loop_of_exactly_that_many_beats() {
        let secs = 2.0; // 4 beats at 120 BPM
        let pcm = Pcm {
            rate: 1000,
            samples: vec![0.0; (2.0 * secs * 1000.0) as usize],
        };
        let d = drum(pcm, 4).unwrap();
        assert!(
            (d.bpm - 120.0).abs() < 1e-3 && d.beats == 4,
            "{} BPM, {} beats",
            d.bpm,
            d.beats
        );
        assert!(drum(Pcm::default(), 4).is_err());
        let slow = Pcm {
            rate: 1000,
            samples: vec![0.0; 2 * 60_000],
        }; // 4 beats in a minute: 4 BPM
        assert!(drum(slow, 4).is_err());
    }
}
