//! Track Suggestion's measurements (`analysis::similar`) on a real device library (read by its plugin) named by `OPENFADER_USB`;
//! skips otherwise. Every track must yield features from the library's own 3-band waveform and a year tag must read; our
//! own analysis of the decoded audio must land in the same sound world (similarity above 0.5, no scale slip). Measured
//! while writing this: the two waveforms differ by more than the tracks of this all-electronic export differ from each
//! other, so the two sources are not mixed in one ranking where avoidable (`similar::profile`).

mod common;

use openfader::analysis::{
    self,
    similar::{Features, profile},
};
use openfader::audio::decode;

#[test]
fn features_of_a_real_export() {
    let Some((lib, tracks)) = common::usb_library() else {
        return;
    };

    let mut years = 0;
    let theirs: Vec<Features> = tracks
        .iter()
        .map(|t| {
            let (read, owned) = (lib.analysis_reader().unwrap(), t.clone());
            let p = profile(
                t.file.as_deref().unwrap(),
                Some(Box::new(move || read(&owned))),
            );
            years += p.tags.year.is_some() as usize;
            p.features.expect("features from the device's waveform")
        })
        .collect();
    eprintln!("{} tracks, {years} with a year tag", tracks.len());
    assert!(years > 0, "no year tag read from any file");
    for f in &theirs {
        assert!(
            (f.bands.iter().sum::<f32>() - 1.0).abs() < 1e-4 && (0.0..=1.0).contains(&f.energy)
        );
        assert!((f.similarity(f) - 1.0).abs() < 1e-6);
    }
    for (i, t) in tracks.iter().enumerate().take(4) {
        let pcm = decode(t.file.as_ref().unwrap()).unwrap();
        let mine = Features::of(&analysis::analyze(&pcm.samples, pcm.rate), true).unwrap();
        let same = mine.similarity(&theirs[i]);
        eprintln!(
            "track {i}: own vs device {same:.3}; ours bands {:?} energy {:.3}",
            mine.bands, mine.energy
        );
        assert!(
            same > 0.5,
            "track {i}: our waveform measures it far from the device's ({same:.3})"
        );
    }
}
