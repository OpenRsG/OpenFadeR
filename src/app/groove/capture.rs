//! DRUM CAPTURE on a deck: the range of beats ([`Deck::capture_range`]) of the DRUMS stem (or, PREFS > Extensions > DRUM CAPTURE, of the
//! original track) as a drum loop, and the WAV file it is also written to ("Location of Files"). The file name carries the tempo and beat count (`<title>-drums-8b-123.50bpm-<time>.wav`), so the loop loads again
//! with its tempo from anywhere ([`super::source::bpm_from_name`]).

use crate::app::{deck::Deck, prefs};
use openfader::{
    audio::{
        self, Pcm,
        groove::{Drum, capture},
    },
    paths,
};
use std::path::PathBuf;

/// The ENV override of the capture folder.
const DIR_ENV: &str = "OPENFADER_DRUM_DIR";

/// Longest title part of a capture file name.
const STEM_MAX: usize = 40;

/// `beats` beats of deck `deck`'s track from its capture range: the DRUMS stem (`original` = false) or the track itself.
pub fn take(deck: &Deck, beats: u32, original: bool) -> Result<Drum, String> {
    let (from, to) = deck.capture_range(beats)?;
    deck.with_player(|p| {
        let rate = p.capture(0.0, 0.0).rate;
        if rate == 0 {
            return Err(tr!("no audio on this deck").to_string());
        }
        let pcm: Pcm = if original {
            p.capture(from, (to - from) / 1000.0)
        } else {
            let stems = p.stems().ok_or(tr!(
                "DRUM CAPTURE needs the STEMS of this track (PREFS > Extensions)"
            ))?;
            let frame = |ms: f64| (ms * rate as f64 / 1000.0).round() as usize;
            capture::drums(&stems, rate, frame(from), frame(to) - frame(from))?
        };
        capture::drum(pcm, beats)
    })
    .ok_or(tr!("no audio on this deck"))?
}

/// Writes the loop as a WAV file named after `title`; returns its path.
pub fn save(d: &Drum, title: &str) -> Result<PathBuf, String> {
    let dir = prefs::with(|p| paths::capture_dir(&p.extensions.drum_dir, DIR_ENV, "Drums"))
        .ok_or(tr!("set a capture location, {} or HOME", DIR_ENV))?;
    let prefix = format!(
        "{}-drums-{}b-{:.2}bpm",
        paths::file_stem(title, STEM_MAX),
        d.beats,
        d.bpm
    );
    let path = paths::at(paths::timestamped(&dir, &prefix, "wav"), &dir)?;
    paths::at(audio::write_wav(&path, &d.pcm, d.pcm.rate), &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_capture_loads_again_with_its_tempo() {
        let tmp = std::env::temp_dir().join(format!("rk-drum-cap-{}", std::process::id()));
        // 4 beats at 120 BPM = 2 s at 1 kHz
        let d = capture::drum(
            Pcm {
                rate: 1000,
                samples: vec![0.1; 4000],
            },
            4,
        )
        .unwrap();
        let prefix = format!(
            "{}-drums-{}b-{:.2}bpm",
            paths::file_stem("T", STEM_MAX),
            d.beats,
            d.bpm
        );
        let path = paths::timestamped(&tmp, &prefix, "wav").unwrap();
        audio::write_wav(&path, &d.pcm, d.pcm.rate).unwrap();
        assert_eq!(super::super::source::bpm_from_name(&path), Some(120.0));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
