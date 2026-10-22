//! How an audio file becomes a [`Drum`]: a library track, a DRUM CAPTURE file or a sample of a sampler pack (the library plugin
//! that opens the pack has unpacked it to a plain file). The tempo is the one given (the library's, the pack's, a capture's), else
//! the one in a capture file's name (`…-120.00bpm-…`), else the one the beat detector finds in the audio.

use openfader::{
    analysis,
    audio::{self, Pcm, groove::Drum},
};
use std::path::Path;

/// `…-120.00bpm-…` in a file name: the tempo DRUM CAPTURE wrote there.
pub fn bpm_from_name(path: &Path) -> Option<f32> {
    path.file_stem()?
        .to_str()?
        .split('-')
        .find_map(|p| p.strip_suffix("bpm")?.parse::<f32>().ok())
        .filter(|b| *b > 0.0)
}

/// The tempo of `pcm` by the beat detector (the grid's first tempo); 0 when it finds none.
fn detect(pcm: &Pcm) -> f32 {
    analysis::analyze(&pcm.samples, pcm.rate)
        .beats
        .first()
        .map_or(0.0, |b| b.bpm_x100 as f32 / 100.0)
}

/// The loop of the audio file `file` at `bpm` BPM (0 = find out). Heavy: call off the UI thread.
pub fn load(file: &Path, bpm: f32) -> Result<Drum, String> {
    let pcm = audio::decode(file)?;
    let bpm = [bpm, bpm_from_name(file).unwrap_or(0.0)]
        .into_iter()
        .find(|b| *b > 0.0)
        .unwrap_or_else(|| detect(&pcm));
    if bpm <= 0.0 {
        return Err(tr!("no tempo found for this drum loop").into());
    }
    Drum::new(pcm, bpm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_file_names_its_tempo() {
        let p = |s: &str| bpm_from_name(Path::new(s));
        assert_eq!(
            p("/x/Some Song-drums-8b-123.50bpm-20260101-101010.wav"),
            Some(123.5)
        );
        assert_eq!(p("Breaks1.wav"), None);
        assert_eq!(p("a-0bpm-b.wav"), None);
    }

    #[test]
    fn a_file_loop_takes_the_given_tempo_else_the_name_else_the_detector() {
        let dir = std::env::temp_dir().join(format!("rk-groove-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // two seconds of a click every half second at 8 kHz
        let samples: Vec<f32> = (0..16_000)
            .flat_map(|i| [if i % 4000 < 40 { 0.9 } else { 0.0 }; 2])
            .collect();
        let pcm = Pcm {
            rate: 8000,
            samples,
        };
        let named = dir.join("loop-120.00bpm.wav");
        let plain = dir.join("loop.wav");
        for f in [&named, &plain] {
            audio::write_wav(f, &pcm, 8000).unwrap();
        }
        assert_eq!(
            load(&plain, 90.0).unwrap().bpm,
            90.0,
            "the given tempo wins"
        );
        let d = load(&named, 0.0).unwrap();
        assert_eq!((d.bpm, d.beats), (120.0, 4), "the name's tempo");
        assert!(load(&dir.join("nope.wav"), 0.0).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
