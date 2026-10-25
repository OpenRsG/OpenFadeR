//! Runs the analysis-file parser over a real Device Library named by `OPENFADER_USB`; skips otherwise.

mod common;

use openfader_plugin::library::{DETAIL_ENTRIES_PER_SEC, LibraryPlugin};
use rekordbox_common::anlz::{self, load_track};
use rekordbox_device::Device;
use serde_json::{json, to_value};
use std::path::Path;

#[test]
fn every_anlz_file_loads_and_invariants_hold() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let mut dats = Vec::new();
    common::all_dats(&root.join("PIONEER/USBANLZ"), &mut dats);
    dats.sort();
    assert!(!dats.is_empty());
    let (mut hot, mut mem, mut loops, mut colored) = (0, 0, 0, 0);
    for dat in &dats {
        // Each standalone file parses on its own, not only through load_track.
        for ext in ["DAT", "EXT", "2EX"] {
            let p = dat.with_extension(ext);
            let a =
                anlz::parse(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
                    .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            // Every `.EXT` has a 1200-column colour preview.
            if ext == "EXT" {
                assert_eq!(a.color_preview.len(), 1200, "{}", p.display());
            }
        }
        let a = load_track(dat).unwrap();
        let n = a.detail.len();
        assert!(
            n > 0 && a.color_detail.len() == n,
            "PWV3/PWV5 counts differ"
        );
        assert_eq!(a.three_band_detail.len(), n, "PWV7 != PWV3");
        assert_eq!(a.color_preview.len(), 1200);
        assert_eq!(a.three_band_preview.len(), 1200);
        assert_eq!(a.preview.len(), 400);
        assert!(a.vocal_thresholds.is_some());
        // Detail covers the beat grid: it ends after the last beat, within a few seconds.
        let detail_ms = n as u32 * 1000 / DETAIL_ENTRIES_PER_SEC;
        let last_beat = a.beats.last().expect("beatgrid").time_ms;
        assert!(
            detail_ms >= last_beat && detail_ms - last_beat < 3000,
            "detail {detail_ms} ms vs last beat {last_beat} ms"
        );
        for c in &a.cues {
            assert!(c.time_ms < detail_ms);
            assert!(c.hot_cue <= 8, "hot cue {}", c.hot_cue);
            if let Some(end) = c.loop_end_ms {
                assert!(end > c.time_ms && end < detail_ms);
                loops += 1;
            }
            colored += c.color.is_some() as usize;
            if c.hot_cue == 0 { mem += 1 } else { hot += 1 }
        }
    }
    assert!(hot + mem > 0, "no file has cues");
    eprintln!(
        "files={} hot_cues={hot} memory_cues={mem} loops={loops} colored={colored}",
        dats.len()
    );
}

#[test]
fn detail_length_matches_pdb_duration() {
    let Some(root) = common::usb_root() else {
        return;
    };
    for t in common::open(&root, None).tracks(None).unwrap() {
        let a = load_track(Path::new(t.analysis_ref.as_ref().unwrap())).unwrap();
        let (got, want) = (a.color_detail.len() as i64, t.length_s * 150);
        assert!((got - want).abs() <= 300, "PWV5 {got} vs length {want}");
    }
}

/// The source's `analysis` and the plugin's `analysis_ref` answer exactly what `load_track` reads, with or without a library.
#[test]
fn source_and_ref_answer_with_load_track() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let lib = common::open(&root, None);
    let tracks = lib.tracks(None).unwrap();
    assert!(!tracks.is_empty());
    for t in &tracks {
        let reference = t.analysis_ref.as_deref().unwrap();
        let want = to_value(load_track(Path::new(reference)).unwrap()).unwrap();
        assert_eq!(to_value(lib.analysis(t).unwrap()).unwrap(), want);
        assert_eq!(
            to_value(Device::analysis_ref(reference, &json!({})).unwrap()).unwrap(),
            want
        );
    }
    assert!(Device::analysis_ref("/nonexistent/ANLZ0000.DAT", &json!({})).is_none());
}
