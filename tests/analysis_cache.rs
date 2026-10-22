//! Bulk-analysis path on generated click tracks (no personal audio): the first pass decodes + analyses and fills the
//! analysis store, the second pass hits it; an entry follows its track (id) when the file moves, a changed file makes it
//! stale, and the old per-path entries of the cache folder move over once. One test in this binary because it points
//! `XDG_CACHE_HOME` and `XDG_DATA_HOME` at a temp dir.
mod common;

use openfader::{
    analysis::cache::{self, Source},
    library::Track,
};
use std::time::Instant;

#[test]
fn second_pass_hits_the_store_and_is_faster() {
    let root = common::fresh_dir("bulk");
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe {
        std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
    }
    let tracks: Vec<_> = [120.0, 128.0, 140.0]
        .iter()
        .zip(1..)
        .map(|(&bpm, id)| {
            let file = root.join(format!("click{bpm}.wav"));
            common::click_wav(&file, bpm, 30.0);
            (
                bpm,
                Track {
                    id: Some(id),
                    file: Some(file),
                    ..Track::default()
                },
            )
        })
        .collect();

    let pass = || {
        let t = Instant::now();
        let r: Vec<_> = tracks
            .iter()
            .map(|(_, t)| cache::get_or_analyze(t).unwrap())
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
        "store pass should be much faster: cold {cold:?}, warm {warm:?}"
    );
    let data = cache::dir().unwrap();
    assert!(data.starts_with(root.join("data")), "analysis is user data");
    assert!(
        !root
            .join("cache")
            .join("openfader")
            .join("analysis")
            .exists(),
        "nothing is written to the cache folder"
    );

    // The same track after its file moved to another folder and name: still analysed.
    let (_, t) = &tracks[0];
    let moved = root.join("elsewhere");
    std::fs::create_dir_all(&moved).unwrap();
    let new_file = moved.join("renamed.wav");
    std::fs::rename(t.file.as_ref().unwrap(), &new_file).unwrap();
    let t_moved = Track {
        file: Some(new_file.clone()),
        ..t.clone()
    };
    assert!(matches!(
        cache::get_or_analyze(&t_moved).unwrap().1,
        Source::Cache
    ));
    // A changed file is not the audio the entry was made for.
    let mut edited = std::fs::read(&new_file).unwrap();
    edited.extend_from_slice(&[0; 64]);
    std::fs::write(&new_file, edited).unwrap();
    assert!(matches!(
        cache::get_or_analyze(&t_moved).unwrap().1,
        Source::Computed { ignored: None }
    ));

    // A corrupt entry is ignored, recomputed with a reason, and healed.
    let entry = std::fs::read_dir(&data)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|e| e.extension().is_some_and(|x| x == "bin"))
        .unwrap();
    std::fs::write(&entry, b"RKAC").unwrap();
    let notes: Vec<_> = tracks[1..]
        .iter()
        .chain([&(0.0, t_moved.clone())])
        .filter_map(|(_, t)| match cache::get_or_analyze(t).unwrap().1 {
            Source::Computed { ignored: Some(n) } => Some(n),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        tracks[1..]
            .iter()
            .chain([&(0.0, t_moved.clone())])
            .all(|(_, t)| matches!(cache::get_or_analyze(t).unwrap().1, Source::Cache))
    );

    // Old entries (per path, size and mtime, no size field) of Collection tracks move over once; others stay.
    let old_dir = cache::legacy_dir().unwrap();
    std::fs::create_dir_all(&old_dir).unwrap();
    let mut old = std::fs::read(&entry).unwrap();
    old[4..8].copy_from_slice(&3u32.to_le_bytes());
    old.drain(8..16);
    let fresh_file = root.join("fresh.wav");
    common::click_wav(&fresh_file, 124.0, 5.0);
    let fresh = Track {
        id: Some(9),
        file: Some(fresh_file.clone()),
        ..Track::default()
    };
    std::fs::write(
        old_dir.join(format!("{}.bin", cache::key(&fresh_file).unwrap())),
        &old,
    )
    .unwrap();
    std::fs::write(old_dir.join("0000000000000000.bin"), &old).unwrap(); // a track the Collection has not got
    let m = cache::migrate(&[fresh.clone()]);
    assert_eq!((m.moved, m.left), (1, 1));
    assert!(
        old_dir.join("0000000000000000.bin").exists(),
        "what nobody owns is left alone"
    );
    // its beats come from an older detector: moved, but recomputed on first use, then served from the cache
    assert!(matches!(
        cache::get_or_analyze(&fresh).unwrap().1,
        Source::Computed { .. }
    ));
    assert!(matches!(
        cache::get_or_analyze(&fresh).unwrap().1,
        Source::Cache
    ));
    std::fs::remove_file(old_dir.join("0000000000000000.bin")).unwrap();
    assert_eq!(
        cache::migrate(&[fresh]),
        cache::Migrated { moved: 0, left: 0 }
    );
    assert!(!old_dir.exists(), "an emptied old folder is removed");
    // Windows: the store this process opened in `root` stays open until the process ends.
    let _ = std::fs::remove_dir_all(&root);
}
