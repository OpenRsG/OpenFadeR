//! `device_check` and `device_fix` on synthetic sticks in temp dirs: what is found, that looking changes nothing, and what the
//! fixes repair. Every text the plugin answers is translated.

mod common;

use common::{assert_translated, export, read_back, snapshot, tree, with_host, Root};
use openfader_plugin::library::{
    commit::{self, Commit},
    CheckReport, ExportOptions, LibraryPlugin, Problem, Severity,
};
use serde_json::Value;
use std::path::Path;
use compat_traktor::{Plugin, FILE};

fn check(dev: &Path) -> CheckReport {
    let before = tree(dev);
    let report = Plugin::device_check(dev, &Default::default(), &Value::Null).unwrap();
    assert_eq!(
        before,
        tree(dev),
        "device_check wrote or recovered something"
    );
    for p in &report.problems {
        for t in [&p.cause, &p.consequence, &p.action] {
            assert_translated(t);
        }
        assert!(p.id.starts_with("traktor:"), "{}", p.id);
        assert_eq!(p.library, "Traktor collection");
    }
    report
}

fn problem<'a>(report: &'a CheckReport, id: &str) -> &'a Problem {
    report
        .problems
        .iter()
        .find(|p| p.id == format!("traktor:{id}"))
        .unwrap_or_else(|| panic!("no {id} in {:?}", report.problems))
}

fn fix(dev: &Path, id: &str) -> String {
    let msg = with_host(|| Plugin::device_fix(dev, id, &Default::default(), &Value::Null))
        .unwrap()
        .message;
    assert_translated(&msg);
    msg
}

/// A stick with the four tracks written.
fn stick(r: &Root) {
    export(
        &r.dev(),
        &snapshot(r, &[0, 1, 2, 3], 2),
        &ExportOptions::default(),
    )
    .unwrap();
}

#[test]
fn a_whole_stick_has_no_problems_and_its_counts() {
    let r = Root::new("clean");
    stick(&r);
    let report = check(&r.dev());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let lib = &report.libraries[0];
    assert_eq!(lib.name, "Traktor collection");
    assert_eq!(
        (lib.present, lib.readable, lib.tracks, lib.playlists),
        (true, true, 4, 3)
    );
    // Hot cues of T0 (2) and of the three others (1 each); the grid marker is no cue.
    assert_eq!(lib.cues, 5);
    assert_eq!(read_back(&r.dev()), (4, 3, 2));
    assert!(lib.modified.unwrap() > 1_600_000_000);
}

#[test]
fn a_stick_without_a_collection_is_not_a_problem() {
    let r = Root::new("none");
    for dev in [r.dev(), r.0.join("src")] {
        let report = check(&dev);
        assert!(report.problems.is_empty());
        let lib = &report.libraries[0];
        assert_eq!((lib.present, lib.readable, lib.tracks), (false, false, 0));
    }
    assert!(!r.dev().exists(), "looking does not make the folder");
}

#[test]
fn audio_the_collection_lists_but_the_stick_lacks_is_an_error_naming_the_files() {
    let r = Root::new("missing");
    stick(&r);
    std::fs::remove_file(r.dev().join("Contents/Art/Alb/c.wav")).unwrap();
    std::fs::remove_file(r.dev().join("Contents/Art/Alb/d.wav")).unwrap();
    let report = check(&r.dev());
    let p = problem(&report, "missing_audio");
    assert_eq!(p.severity, Severity::Error);
    assert_eq!(
        p.files,
        ["Contents/Art/Alb/c.wav", "Contents/Art/Alb/d.wav"]
    );
    assert!(p.fix.is_none(), "the host copies them again");
    assert_eq!(report.problems.len(), 1);
    // Exporting the tracks again puts them back.
    stick(&r);
    assert!(check(&r.dev()).problems.is_empty());
}

#[test]
fn entries_of_other_volumes_are_not_checked() {
    // A volume no test root can be on ("C:" is the temporary folder's own drive on Windows).
    let r = Root::new("foreign");
    stick(&r);
    let nml = std::fs::read_to_string(r.dev().join(FILE)).unwrap();
    let foreign = nml.replacen(
        "<COLLECTION ENTRIES=\"4\">",
        "<COLLECTION ENTRIES=\"5\">\n<ENTRY TITLE=\"X\"><LOCATION DIR=\"/:Users/:dj/:Music/:\" FILE=\"x.mp3\" VOLUME=\"Macintosh HD\"/></ENTRY>",
        1,
    );
    assert_ne!(nml, foreign);
    std::fs::write(r.dev().join(FILE), foreign).unwrap();
    let report = check(&r.dev());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.libraries[0].tracks, 5);
}

#[test]
fn a_collection_that_cannot_be_read_is_unreadable_and_has_no_fix() {
    let r = Root::new("unreadable");
    stick(&r);
    std::fs::write(r.dev().join(FILE), "<NML><COLLECTION>").unwrap();
    let report = check(&r.dev());
    let p = problem(&report, "unreadable");
    assert_eq!(p.severity, Severity::Unreadable);
    assert!(p.fix.is_none());
    let lib = &report.libraries[0];
    assert_eq!((lib.present, lib.readable, lib.tracks), (true, false, 0));
}

#[test]
fn a_leftover_staged_file_is_found_and_cleaned_by_the_fix() {
    let r = Root::new("leftover");
    stick(&r);
    let staged = r.dev().join("Traktor/collection.nml.ofnew");
    std::fs::write(&staged, "half a collection").unwrap();
    std::fs::write(r.dev().join("Traktor/collection.nml.part"), "older").unwrap();
    let report = check(&r.dev());
    let p = problem(&report, "leftovers");
    assert_eq!(p.severity, Severity::Info);
    assert_eq!(p.fix.as_deref(), Some("traktor:clean_leftovers"));
    let kept = std::fs::read(r.dev().join(FILE)).unwrap();
    let msg = fix(&r.dev(), "traktor:clean_leftovers");
    assert!(msg.contains("Removed"), "{msg}");
    assert!(!staged.exists() && !r.dev().join("Traktor/collection.nml.part").exists());
    assert_eq!(kept, std::fs::read(r.dev().join(FILE)).unwrap());
    assert!(check(&r.dev()).problems.is_empty());
    // Nothing left to do.
    assert!(fix(&r.dev(), "traktor:clean_leftovers").contains("nothing to clean"));
}

#[test]
fn an_interrupted_switch_is_a_warning_and_the_fix_finishes_it() {
    let r = Root::new("switching");
    export(
        &r.dev(),
        &snapshot(&r, &[0, 1], 1),
        &ExportOptions::default(),
    )
    .unwrap();
    commit::fail_at(Some(0));
    let cut = common::export_raw(
        &r.dev(),
        &snapshot(&r, &[0, 1, 2, 3], 2),
        &ExportOptions::default(),
    );
    commit::fail_at(None);
    assert!(cut.is_err());
    let report = check(&r.dev());
    let p = problem(&report, "interrupted");
    assert_eq!(p.severity, Severity::Warning);
    assert_eq!(p.fix.as_deref(), Some("traktor:clean_leftovers"));
    assert!(
        report.problems.iter().all(|p| p.id != "traktor:leftovers"),
        "the staged file belongs to the change"
    );
    let msg = fix(&r.dev(), "traktor:clean_leftovers");
    assert!(msg.contains("Finished"), "{msg}");
    assert_eq!(read_back(&r.dev()), (4, 3, 2));
    assert!(check(&r.dev()).problems.is_empty());
}

#[test]
fn a_change_cut_while_staging_is_info_and_the_fix_undoes_it() {
    let r = Root::new("staging");
    stick(&r);
    let before = std::fs::read(r.dev().join(FILE)).unwrap();
    let mut c = Commit::new(&r.dev(), "traktor").unwrap();
    c.stage(&r.dev().join(FILE), b"not whole yet").unwrap();
    std::mem::forget(c); // a pulled stick: no drop cleans up
    let report = check(&r.dev());
    let p = problem(&report, "interrupted");
    assert_eq!(p.severity, Severity::Info);
    let msg = fix(&r.dev(), "traktor:clean_leftovers");
    assert!(msg.contains("Undid"), "{msg}");
    assert_eq!(before, std::fs::read(r.dev().join(FILE)).unwrap());
    assert!(check(&r.dev()).problems.is_empty());
    assert!(commit::pending(&r.dev()).is_none());
}

#[test]
fn an_unknown_fix_is_refused_in_a_translated_sentence() {
    let r = Root::new("unknown");
    stick(&r);
    let before = tree(&r.dev());
    let err = Plugin::device_fix(&r.dev(), "traktor:nope", &Default::default(), &Value::Null)
        .unwrap_err();
    assert_translated(&err);
    assert_eq!(before, tree(&r.dev()));
}

#[test]
fn checking_a_thousand_track_stick_is_fast() {
    let r = Root::new("fast");
    std::fs::create_dir_all(r.dev().join("Traktor")).unwrap();
    let mut nml = String::from("<NML VERSION=\"19\"><COLLECTION ENTRIES=\"1000\">");
    for i in 0..1000 {
        nml += &format!(
            "<ENTRY TITLE=\"T{i}\"><LOCATION DIR=\"/:Contents/:A/:B/:\" FILE=\"{i}.wav\" VOLUME=\"device\"/><TEMPO BPM=\"128\"/><CUE_V2 NAME=\"c\" TYPE=\"0\" START=\"1000\" LEN=\"0\" HOTCUE=\"0\"/></ENTRY>"
        );
    }
    nml += "</COLLECTION><PLAYLISTS/></NML>";
    std::fs::write(r.dev().join(FILE), nml).unwrap();
    let t = std::time::Instant::now();
    let report = check(&r.dev());
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    assert_eq!(report.libraries[0].tracks, 1000);
    assert_eq!(report.libraries[0].cues, 1000);
    assert_eq!(problem(&report, "missing_audio").files.len(), 1000);
}
