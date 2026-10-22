//! The cue point a LOAD puts the playhead on, in this order (see `Deck::install` and
//! `Deck::poll_decode`): 1. LOAD SETTING on and the track has a memory / hot cue → the one nearest the start ([`nearest`]);
//! 2. AUTO CUE on → the first sound at or above the Auto Cue Level ([`first_sound`], needs the decoded audio, so it is
//! applied when the decode lands); 3. the track's first memory cue; 4. the start.

use openfader::{analysis::Cue, audio::Frames};

/// Length of the level window (ms): a click shorter than that does not count as sound.
const WINDOW_MS: u64 = 10;

/// Start (ms) of the first window of `audio` (`rate` Hz) whose RMS level (both channels) reaches `db` dBFS; `None` when none does.
pub fn first_sound<S: Frames + ?Sized>(audio: &S, rate: u32, db: f32) -> Option<u32> {
    if rate == 0 {
        return None;
    }
    let win = (rate as u64 * WINDOW_MS / 1000).max(1) as usize;
    let n = audio.frame_count();
    let limit = 10f32.powf(db / 10.0); // mean square
    let i = (0..n.div_ceil(win)).position(|i| {
        let w = i * win..((i + 1) * win).min(n);
        let frames = w.len();
        audio.frames_in(w).flatten().map(|s| s * s).sum::<f32>() / (2 * frames) as f32 >= limit
    })?;
    Some((i as u64 * win as u64 * 1000 / rate as u64) as u32)
}

/// The cue (memory, hot or loop) nearest the start of the track.
pub fn nearest(cues: &[Cue]) -> Option<u32> {
    cues.iter().map(|c| c.time_ms).min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::audio::Pcm;

    /// Stereo audio at 1 kHz: `quiet` ms of silence, then amplitude `amp` (a constant, so RMS = amp).
    fn pcm(quiet: usize, amp: f32, len: usize) -> Pcm {
        let mut samples = vec![0.0; quiet * 2];
        samples.extend(vec![amp; (len - quiet) * 2]);
        Pcm {
            rate: 1000,
            samples,
        }
    }

    #[test]
    fn the_first_sound_is_where_the_level_is_reached() {
        let p = pcm(500, 0.1, 2000); // -20 dBFS from 500 ms
        assert_eq!(first_sound(&p.samples, p.rate, -36.0), Some(500));
        assert_eq!(first_sound(&p.samples, p.rate, -20.5), Some(500));
        assert_eq!(
            first_sound(&p.samples, p.rate, -19.0),
            None,
            "louder than the track gets"
        );
    }

    #[test]
    fn a_quieter_level_finds_quieter_sound_earlier() {
        let mut p = pcm(1000, 0.1, 3000);
        p.samples[600 * 2..700 * 2].fill(0.002); // -54 dBFS murmur from 600 ms
        assert_eq!(first_sound(&p.samples, p.rate, -60.0), Some(600));
        assert_eq!(first_sound(&p.samples, p.rate, -42.0), Some(1000));
    }

    #[test]
    fn silence_and_no_audio_have_no_first_sound() {
        let sound = |p: Pcm, db| first_sound(&p.samples, p.rate, db);
        assert_eq!(sound(pcm(100, 0.0, 100), -78.0), None);
        assert_eq!(sound(Pcm::default(), -60.0), None);
    }

    #[test]
    fn nearest_is_the_earliest_cue_of_any_kind() {
        let cue = |hot_cue, time_ms, loop_end_ms| Cue {
            hot_cue,
            time_ms,
            loop_end_ms,
            color: None,
            comment: None,
        };
        assert_eq!(
            nearest(&[
                cue(0, 9000, None),
                cue(2, 4000, None),
                cue(0, 7000, Some(8000))
            ]),
            Some(4000)
        );
        assert_eq!(nearest(&[]), None);
    }
}
