//! Bulk-analysis path on generated click tracks (no personal audio): the first pass decodes + analyses and fills the
//! cache, the second pass hits it. One test in this binary because it points `XDG_CACHE_HOME` at a temp dir.
mod common;

use openfader::analysis::cache::{self, Source};
use std::time::Instant;

#[test]
fn second_pass_hits_the_cache_and_is_faster() {
    let root = common::fresh_dir("bulk");
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("XDG_CACHE_HOME", root.join("cache")) };
    let tracks: Vec<_> = [120.0, 128.0, 140.0]
        .map(|bpm| (bpm, root.join(format!("click{bpm}.wav"))))
        .into();
    for (bpm, p) in &tracks {
        common::click_wav(p, *bpm, 30.0);
    }

    let pass = || {
        let t = Instant::now();
        let r: Vec<_> = tracks
            .iter()
            .map(|(_, p)| cache::get_or_analyze(p).unwrap())
            .collect();
        (r, t.elapsed())
    };
    let (first, cold) = pass();
    for ((bpm, _), (a, src)) in tracks.iter().zip(&first) {
        assert!(matches!(src, Source::Computed { ignored: None }));
        let got = a.beats.first().expect("click track gets a grid").bpm_x100 as f64 / 100.0;
        assert!((got - bpm).abs() <= 0.1, "{bpm}: detected {got}");
        assert!(!a.three_band_detail.is_empty() && !a.three_band_preview.is_empty());
    }
    let (second, warm) = pass();
    assert!(second.iter().all(|(_, s)| matches!(s, Source::Cache)));
    for ((a, _), (b, _)) in first.iter().zip(&second) {
        assert_eq!(
            (&a.beats, &a.key, &a.three_band_detail, &a.phrases),
            (&b.beats, &b.key, &b.three_band_detail, &b.phrases)
        );
    }
    eprintln!("bulk analysis of 3 x 30 s click tracks: cold {cold:?}, warm {warm:?}");
    assert!(
        warm * 5 < cold,
        "cache pass should be much faster: cold {cold:?}, warm {warm:?}"
    );

    // A corrupt entry is ignored, recomputed with a reason, and healed.
    let entry = std::fs::read_dir(cache::dir().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|e| e.extension().is_some_and(|x| x == "bin"))
        .unwrap();
    std::fs::write(&entry, b"RKAC").unwrap();
    let notes: Vec<_> = tracks
        .iter()
        .filter_map(|(_, p)| match cache::get_or_analyze(p).unwrap().1 {
            Source::Computed { ignored: Some(n) } => Some(n),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        tracks
            .iter()
            .all(|(_, p)| matches!(cache::get_or_analyze(p).unwrap().1, Source::Cache))
    );
    std::fs::remove_dir_all(&root).unwrap();
}
