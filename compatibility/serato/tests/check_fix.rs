//! The stick checker (`device_check`) and its fix (`device_fix`) on synthetic sticks in temp dirs: every fault that is injected is
//! found with the right severity, fix and numbers, the fix repairs what it offers, and the stick checks clean afterwards.

mod common;

use common::*;
use openfader_plugin::library::{
    commit::{self, Commit, Recovered},
    ActionAnswer, CheckReport, ExportOptions, LibraryPlugin, Problem, Severity, Snapshot,
};
use compat_serato::{check::CUE_SAMPLE, crate_file, database, tag, Plugin};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SHARED: &str = "Contents/Artist/Album/";

/// Three tracks a, b, c (the standard analysis: two hot cues and a loop each) in the crates P1 = [a, b] and P2 = [c].
fn snap(d: &Path) -> Snapshot {
    let tracks = ["a", "b", "c"]
        .iter()
        .map(|n| {
            let f = d.join("lib").join(format!("{n}.mp3"));
            if !f.exists() {
                fs::write(&f, mp3(4096)).unwrap();
            }
            (track(&f, n), Some(analysis(1500)))
        })
        .collect();
    Snapshot {
        tracks,
        playlists: vec![playlist("P1", 0, false), playlist("P2", 0, false)],
        members: vec![vec![0, 1], vec![2]],
        ..Snapshot::default()
    }
}

/// A stick with the three tracks exported.
fn stick(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let d = tmp(name);
    let usb = d.join("usb");
    export_host(&usb, &snap(&d), &ExportOptions::default()).unwrap();
    (d, usb)
}

/// The report of the checker; every text in it must be translated.
fn check(usb: &Path) -> CheckReport {
    let report = Plugin::device_check(usb, &BTreeMap::new(), &Value::Null).unwrap();
    for p in &report.problems {
        [&p.cause, &p.consequence, &p.action]
            .into_iter()
            .for_each(|t| assert_translated(t));
    }
    report
}

fn fix(usb: &Path, id: &str) -> Result<ActionAnswer, String> {
    let out = with_host(|| Plugin::device_fix(usb, id, &BTreeMap::new(), &Value::Null));
    match &out {
        Ok(a) => assert_translated(&a.message),
        Err(e) => assert_translated(e),
    }
    out
}

fn problem<'a>(r: &'a CheckReport, what: &str) -> &'a Problem {
    r.problems
        .iter()
        .find(|p| p.id == format!("serato:{what}"))
        .unwrap_or_else(|| panic!("no {what} in {:?}", r.problems))
}

fn clean(usb: &Path) {
    let r = check(usb);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}

#[test]
fn a_whole_stick_checks_clean_and_says_what_it_holds() {
    let (_, usb) = stick("whole");
    let report = check(&usb);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let l = &report.libraries[0];
    assert_eq!(l.name, "Serato library");
    assert!(
        l.present && l.readable && l.modified.is_some_and(|t| t > 1_700_000_000),
        "{l:?}"
    );
    // Two hot cues and a loop in each of three tracks.
    assert_eq!((l.tracks, l.playlists, l.cues), (3, 2, 9));
    assert_eq!(report.libraries.len(), 1);
}

#[test]
fn a_stick_without_a_library_is_not_a_problem_and_the_check_writes_nothing() {
    let d = tmp("none");
    let nothing = d.join("nothing-here");
    let r = check(&nothing);
    assert!(!r.libraries[0].present && !r.libraries[0].readable && r.problems.is_empty());
    assert!(!nothing.exists());
    let empty = d.join("usb");
    let before = tree(&empty);
    assert!(check(&empty).problems.is_empty());
    assert_eq!(before, tree(&empty));
}

#[test]
fn a_truncated_library_is_unreadable_and_has_no_fix() {
    let (d, usb) = stick("truncated");
    let p = usb.join(database::FILE);
    let bytes = fs::read(&p).unwrap();
    fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
    let before = tree(&usb);
    let report = check(&usb);
    assert_eq!(before, tree(&usb), "the check wrote nothing");
    let l = &report.libraries[0];
    assert!(l.present && !l.readable && l.tracks == 0);
    let pr = problem(&report, "unreadable_library");
    assert_eq!(
        (pr.severity, pr.fix.as_deref()),
        (Severity::Unreadable, None)
    );
    assert_eq!(pr.library, "Serato library");
    assert!(
        pr.cause.contains("cannot be read") && pr.action.contains("no backup copy"),
        "{pr:?}"
    );
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);

    // Not a database at all: does not start with `vrsn`.
    fs::write(&p, b"just some text, not a Serato database").unwrap();
    assert_eq!(
        problem(&check(&usb), "unreadable_library").severity,
        Severity::Unreadable
    );
    fs::write(&p, b"").unwrap();
    assert_eq!(
        problem(&check(&usb), "unreadable_library").severity,
        Severity::Unreadable
    );
    let _ = d;
}

#[test]
fn audio_that_is_listed_but_gone_is_an_error_the_host_has_to_copy_again() {
    let (_, usb) = stick("audio");
    fs::remove_file(usb.join(SHARED).join("b.mp3")).unwrap();
    let report = check(&usb);
    let pr = problem(&report, "audio_missing");
    assert_eq!(pr.severity, Severity::Error);
    assert_eq!(pr.fix, None);
    assert_eq!(pr.files, [format!("{SHARED}b.mp3")]);
    assert!(
        pr.cause.starts_with("1 track(s) of the Serato library"),
        "{}",
        pr.cause
    );
    assert!(pr.cause.contains(&format!("{SHARED}b.mp3")));
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    assert_eq!(report.libraries[0].tracks, 3);
}

#[test]
fn leftover_staged_files_without_a_journal_are_info_and_are_cleaned() {
    let (_, usb) = stick("left");
    fs::write(usb.join("_Serato_/database V2.ofnew"), b"half").unwrap();
    fs::write(usb.join("_Serato_/Subcrates/P1.crate.ofnew"), b"half").unwrap();
    let report = check(&usb);
    let pr = problem(&report, "leftover_files");
    assert_eq!(
        (pr.severity, pr.fix.as_deref()),
        (Severity::Info, Some("serato:clean_leftovers"))
    );
    assert!(pr.cause.starts_with("2 unfinished"), "{}", pr.cause);
    assert_eq!(report.problems.len(), 1);
    let before = tree(&usb);
    let done = fix(&usb, "serato:clean_leftovers").unwrap();
    assert_eq!(done.message, "Removed 2 unfinished temporary file(s).");
    clean(&usb);
    let after = tree(&usb);
    let gone: Vec<_> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    assert_eq!(
        gone,
        [
            "_Serato_/Subcrates/P1.crate.ofnew",
            "_Serato_/database V2.ofnew"
        ]
    );
    // The libraries and the audio were not touched.
    assert!(after.iter().all(|(k, v)| before.get(k) == Some(v)));
    // Nothing to clean: said so.
    assert!(fix(&usb, "serato:clean_leftovers")
        .unwrap()
        .message
        .contains("no unfinished files"));
}

#[test]
fn a_pending_change_is_the_hosts_to_finish_not_a_leftover() {
    let (d, usb) = stick("pending");
    // A pull-out after the switching journal: the host finishes it.
    let mut second = snap(&d);
    second.tracks.truncate(2);
    second.members = vec![vec![0, 1], vec![]];
    let err = with_host(|| {
        commit::fail_at(Some(0));
        let out = Plugin::export(&usb, &second, &ExportOptions::default(), &Value::Null);
        commit::fail_at(None);
        out
    })
    .unwrap_err();
    assert!(err.contains("injected"));
    assert!(commit::pending(&usb).is_some());
    let report = check(&usb);
    let pr = problem(&report, "pending_change");
    assert_eq!(pr.severity, Severity::Warning);
    assert!(
        report
            .problems
            .iter()
            .all(|p| p.id != "serato:leftover_files"),
        "{:?}",
        report.problems
    );
    // The fix refuses to delete what belongs to the change.
    let staged = tree(&usb);
    let e = fix(&usb, "serato:clean_leftovers").unwrap_err();
    assert!(e.starts_with("A change to this stick is waiting"), "{e}");
    assert_eq!(staged, tree(&usb));
    assert!(matches!(
        commit::recover(&usb).unwrap(),
        Some(Recovered::Finished { .. })
    ));
    clean(&usb);
}

#[test]
fn a_change_that_was_still_staging_is_info_and_is_rolled_back() {
    let (_, usb) = stick("staging");
    let mut c = Commit::new(&usb, "serato").unwrap();
    c.stage(&usb.join(database::FILE), b"new bytes").unwrap();
    std::mem::forget(c); // the stick is pulled here
    let report = check(&usb);
    let pr = problem(&report, "pending_change");
    assert_eq!(pr.severity, Severity::Info);
    assert!(report
        .problems
        .iter()
        .all(|p| p.id != "serato:leftover_files"));
    assert!(matches!(
        commit::recover(&usb).unwrap(),
        Some(Recovered::RolledBack { .. })
    ));
    clean(&usb);
    // Another plugin's change is not ours to report.
    let mut c = Commit::new(&usb, "engine-dj").unwrap();
    c.stage(&usb.join("x.db"), b"x").unwrap();
    std::mem::forget(c);
    assert!(check(&usb)
        .problems
        .iter()
        .all(|p| p.id != "serato:pending_change"));
}

#[test]
fn crates_that_are_broken_or_name_unknown_tracks_are_warnings_without_a_fix() {
    let (_, usb) = stick("crates");
    let dir = usb.join(crate_file::DIR);
    fs::write(
        dir.join("Extra.crate"),
        crate_file::build(&["Nowhere/x.mp3", "Nowhere/y.mp3"]),
    )
    .unwrap();
    fs::write(dir.join("Broken.crate"), b"vrsn\0\0\0\xff cut").unwrap();
    let report = check(&usb);
    let unknown = problem(&report, "crate_unknown_track");
    assert_eq!(
        (unknown.severity, unknown.fix.as_deref()),
        (Severity::Warning, None)
    );
    assert!(
        unknown.cause.starts_with("2 entries") && unknown.cause.contains("Extra"),
        "{}",
        unknown.cause
    );
    let broken = problem(&report, "crate_unreadable");
    assert_eq!(
        (broken.severity, broken.fix.as_deref()),
        (Severity::Warning, None)
    );
    assert!(
        broken.cause.starts_with("1 crate file(s)") && broken.cause.contains("Broken"),
        "{}",
        broken.cause
    );
    // Playlists: P1, P2 and Extra (Broken cannot be read).
    assert_eq!(report.libraries[0].playlists, 3);
    assert!(report.libraries[0].readable);
}

#[test]
fn a_folder_crate_is_not_a_playlist() {
    let d = tmp("folders");
    let usb = d.join("usb");
    let mut s = snap(&d);
    s.playlists = vec![playlist("Sets", 0, true), playlist("Warm", 1, false)];
    s.members = vec![vec![], vec![0, 1]];
    export_host(&usb, &s, &ExportOptions::default()).unwrap();
    assert_eq!(
        check(&usb).libraries[0].playlists,
        1,
        "Sets.crate is a folder, Sets%%Warm.crate a playlist"
    );
}

#[test]
fn an_unknown_fix_is_refused_in_words_the_host_can_translate() {
    let (_, usb) = stick("unknown-fix");
    let before = tree(&usb);
    let e = fix(&usb, "serato:nonsense").unwrap_err();
    assert_eq!(e, "unknown fix serato:nonsense");
    assert_eq!(before, tree(&usb));
}

/// A stick written straight from the models (a real export of 1000 audio files would take minutes): 1000 MP3 files that carry
/// the standard cues, one database, one crate.
fn big_stick(usb: &Path, n: usize) {
    let tags = tag::Tags::new(Some(&analysis(1500)), None).0;
    let mut recs = Vec::new();
    let dir = usb.join("Contents/Artist/Album");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        let f = dir.join(format!("t{i}.mp3"));
        fs::write(&f, mp3(4096)).unwrap();
        let mut edits = tag::plan(&f, &tags, true).unwrap();
        tag::apply(&f, &mut edits).unwrap();
        let rel = format!("{SHARED}t{i}.mp3");
        recs.push((
            rel.clone(),
            database::track(&rel, &track(&f, &format!("T{i}")), 10096, 0),
        ));
    }
    let (db, _) = database::merge(None, &recs, &[]).unwrap();
    fs::create_dir_all(usb.join(crate_file::DIR)).unwrap();
    fs::write(usb.join(database::FILE), db).unwrap();
    let paths: Vec<&str> = recs.iter().map(|(p, _)| p.as_str()).collect();
    fs::write(
        usb.join(crate_file::DIR).join("All.crate"),
        crate_file::build(&paths),
    )
    .unwrap();
}

#[test]
fn checking_a_thousand_track_stick_is_fast() {
    let d = tmp("big");
    let usb = d.join("usb");
    big_stick(&usb, 1000);
    let t = std::time::Instant::now();
    let report = check(&usb);
    let took = t.elapsed();
    eprintln!("device_check of 1000 tracks: {took:?}");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let l = &report.libraries[0];
    // The cue count covers the first CUE_SAMPLE tracks (three cues and loops each).
    assert_eq!((l.tracks, l.playlists, l.cues), (1000, 1, 3 * CUE_SAMPLE));
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
}
