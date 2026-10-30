//! All-or-nothing writes on synthetic sticks in temp dirs (never a real stick): a pull-out at every step of the commit, a cancel
//! at every phase, a dry run that touches nothing, the file size limit and the texts the user reads.

mod common;

use common::*;
use openfader_plugin::library::{
    commit::{self, Recovered},
    track_key, ExportOptions, LibraryPlugin, Severity, Snapshot,
};
use compat_serato::{database, tag, Plugin};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SHARED: &str = "Contents/Artist/Album/";

/// Tracks named `names` (a file `<name>.mp3` each; a name that starts with `t` has no room for tags in its ID3 tag), all with the
/// standard analysis at `cue_ms`, and the playlists `lists` (name, member indexes).
fn lib(d: &Path, names: &[&str], cue_ms: u32, lists: &[(&str, &[usize])]) -> Snapshot {
    let tracks = names
        .iter()
        .map(|n| {
            let f = d.join("lib").join(format!("{n}.mp3"));
            if !f.exists() {
                fs::write(&f, mp3(if n.starts_with('t') { ID3_TIGHT } else { 4096 })).unwrap();
            }
            (track(&f, n), Some(analysis(cue_ms)))
        })
        .collect();
    Snapshot {
        tracks,
        playlists: lists.iter().map(|(n, _)| playlist(n, 0, false)).collect(),
        members: lists.iter().map(|(_, m)| m.to_vec()).collect(),
        colors: vec![(0, 5)],
        ..Snapshot::default()
    }
}

fn plain() -> ExportOptions {
    ExportOptions::default()
}

fn dry() -> ExportOptions {
    ExportOptions {
        dry_run: true,
        ..plain()
    }
}

fn check(usb: &Path) -> openfader_plugin::library::CheckReport {
    Plugin::device_check(usb, &BTreeMap::new(), &Value::Null).unwrap()
}

/// The stick holds one whole generation: `tracks` tracks, the crates `crates` (name, members), the tags of `a.mp3` hold the cue
/// of `cue_ms`, no change is pending and the checker finds nothing wrong.
fn whole(usb: &Path, tracks: usize, crates: &[(&str, usize)], cue_ms: u32) {
    let src = Plugin::open(usb, &Value::Null).unwrap();
    assert_eq!(src.tracks(None).unwrap().len(), tracks);
    for (name, n) in crates {
        assert_eq!(src.tracks(Some(name)).unwrap().len(), *n, "{name}");
    }
    assert!(commit::pending(usb).is_none());
    let report = check(usb);
    assert!(
        report
            .problems
            .iter()
            .all(|p| p.severity < Severity::Warning),
        "{:?}",
        report.problems
    );
    assert_eq!(report.libraries[0].tracks, tracks);
    let m2 = tag::read(&usb.join(SHARED).join("a.mp3"))
        .unwrap()
        .markers2
        .unwrap();
    assert!(
        tag::markers::cues(&m2)
            .iter()
            .any(|c| c.hot_cue == 1 && c.time_ms == cue_ms),
        "the tags of a.mp3 belong to the same generation"
    );
}

#[test]
fn a_pull_out_at_every_step_of_an_export_leaves_one_whole_generation() {
    let d = tmp("pull");
    let usb = d.join("usb");
    let first = lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1])]);
    export_host(&usb, &first, &plain()).unwrap();
    whole(&usb, 2, &[("P1", 2)], 1500);

    // The second export adds a track and a crate and gives a.mp3 another cue: the tags of a file the stick holds are rewritten.
    let second = lib(&d, &["a", "b", "c"], 2500, &[("P1", &[0, 1]), ("P2", &[2])]);
    let mut cut = 0;
    for step in 0.. {
        let dev = d.join(format!("dev{step}"));
        copy_tree(&usb, &dev);
        let out = with_host(|| {
            commit::fail_at(Some(step));
            let out = Plugin::export(&dev, &second, &plain(), &Value::Null);
            commit::fail_at(None);
            out
        });
        match out {
            Ok(report) => {
                assert_eq!(report.verified, Some(true));
                whole(&dev, 3, &[("P1", 2), ("P2", 1)], 2500);
                break;
            }
            Err(e) => {
                assert!(e.contains("injected"), "{e}");
                assert!(commit::pending(&dev).is_some(), "step {step}");
                let before = check(&dev);
                assert!(
                    before.problems.iter().any(|p| p.id == "serato:pending_change" && p.severity == Severity::Warning),
                    "{:?}",
                    before.problems
                );
                assert!(before
                    .problems
                    .iter()
                    .all(|p| p.id != "serato:leftover_files"));
                let done = commit::recover(&dev).unwrap();
                assert!(
                    matches!(done, Some(Recovered::Finished { .. })),
                    "step {step}"
                );
                whole(&dev, 3, &[("P1", 2), ("P2", 1)], 2500);
                cut += 1;
            }
        }
    }
    // The new crate and the database: the journal is cut before, between and after their renames.
    assert!(cut >= 3, "only {cut} steps were cut");
}

#[test]
fn a_cancel_at_any_phase_leaves_the_device_byte_identical() {
    let d = tmp("cancel");
    let new = lib(
        &d,
        &["a", "b", "t0", "t1", "c", "e"],
        2500,
        &[("P1", &[0, 1, 2]), ("P2", &[3, 4, 5])],
    );
    let base = d.join("base");
    export_host(
        &base,
        &lib(&d, &["a", "b", "t0"], 1500, &[("P1", &[0, 1, 2])]),
        &plain(),
    )
    .unwrap();
    assert!(base.join("_Serato_/Audio").is_dir(), "t0 went private");
    for (name, existing) in [("existing", true), ("new", false)] {
        for (phase, item) in [
            ("Copying audio", 0),
            // after the first new track is copied (a stick that has a, b, t0) or after four are (one that has none)
            ("Copying audio", 4),
            ("Writing analysis files", 0),
            // a and b are tagged in place on the existing stick, t0's private copy refreshed
            ("Writing analysis files", 3),
            ("Writing analysis files", 5),
            ("Writing the library", 0),
            ("Checking the library", 0),
        ] {
            let dev = d.join(format!("dev-{name}"));
            let _ = fs::remove_dir_all(&dev);
            if existing {
                copy_tree(&base, &dev);
            }
            let before = tree(&dev);
            let err = with_host(|| {
                cancel_at_item(phase, item);
                Plugin::export(&dev, &new, &plain(), &Value::Null)
            })
            .unwrap_err();
            assert_eq!(err, "cancelled", "{phase} {item} on a {name} stick");
            assert_eq!(before, tree(&dev), "{phase} {item} on a {name} stick");
            if !existing {
                assert!(
                    !dev.exists(),
                    "{phase} {item}: the folder of the stick is gone"
                );
            }
        }
    }
}

#[test]
fn a_private_copy_that_grows_is_put_back_on_a_cancel_and_read_back_when_it_is_not() {
    let d = tmp("grow");
    let base = d.join("base");
    export_host(
        &base,
        &lib(&d, &["t0", "t1"], 1500, &[("P1", &[0, 1])]),
        &plain(),
    )
    .unwrap();
    // A cue name longer than the padding of the private copies: their tags must grow, so the files are rewritten.
    let mut grown = lib(&d, &["t0", "t1"], 1500, &[("P1", &[0, 1])]);
    for (_, a) in &mut grown.tracks {
        a.as_mut().unwrap().cues[0].comment = Some("n".repeat(6000));
    }
    let dev = d.join("dev");
    copy_tree(&base, &dev);
    let before = tree(&dev);
    let err = with_host(|| {
        cancel_at_item("Writing analysis files", 1);
        Plugin::export(&dev, &grown, &plain(), &Value::Null)
    })
    .unwrap_err();
    assert_eq!(err, "cancelled");
    assert_eq!(
        before,
        tree(&dev),
        "the first private copy was rewritten, then put back"
    );

    let report = export_host(&dev, &grown, &plain()).unwrap();
    assert_eq!(report.verified, Some(true));
    let size = |n: &str| {
        fs::metadata(dev.join("_Serato_/Audio/Artist/Album").join(n))
            .unwrap()
            .len()
    };
    let old = |n: &str| {
        fs::metadata(base.join("_Serato_/Audio/Artist/Album").join(n))
            .unwrap()
            .len()
    };
    assert!(
        size("t0.mp3") > old("t0.mp3") + 4000,
        "{} {}",
        size("t0.mp3"),
        old("t0.mp3")
    );
    whole_names(&dev, 2);
}

/// Both tracks read back from the stick (the private copies hold the cues).
fn whole_names(usb: &Path, tracks: usize) {
    let src = Plugin::open(usb, &Value::Null).unwrap();
    assert_eq!(src.tracks(None).unwrap().len(), tracks);
    assert!(commit::pending(usb).is_none());
    assert!(check(usb).problems.is_empty());
}

#[test]
fn two_tracks_of_one_audio_file_are_copied_once() {
    let d = tmp("twice");
    let usb = d.join("usb");
    let mut snap = lib(&d, &["a"], 1500, &[("P1", &[0, 1])]);
    let mut second = snap.tracks[0].clone();
    second.0.title = "Again".into();
    snap.tracks.push(second);
    let plan = export_host(&usb, &snap, &dry()).unwrap().plan.unwrap();
    assert_eq!((plan.add.len(), plan.copy_bytes), (1, size_of(&d, "a.mp3")));
    let real = export_host(&usb, &snap, &plain()).unwrap();
    assert_eq!((real.tracks, real.added), (1, 1));
    assert_eq!(real.verified, Some(true));
    assert_eq!(
        tree(&usb)
            .keys()
            .filter(|k| k.starts_with("Contents/") && k.ends_with(".mp3"))
            .count(),
        1
    );
}

#[test]
fn a_dry_run_touches_nothing_and_its_plan_is_what_the_export_does() {
    let d = tmp("dry");
    let usb = d.join("usb");
    export_host(
        &usb,
        &lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1])]),
        &plain(),
    )
    .unwrap();
    let new = lib(
        &d,
        &["a", "b", "c", "t0"],
        2500,
        &[("P1", &[0, 1]), ("P2", &[2, 3])],
    );
    let before = tree(&usb);
    let report = export_host(&usb, &new, &dry()).unwrap();
    assert_eq!(before, tree(&usb), "the tree is byte-identical");
    let plan = report.plan.clone().expect("a plan");
    assert_eq!(plan.update.len(), 2, "a and b are on the stick");
    assert_eq!(plan.add.len(), 2, "c and t0 are new");
    assert_eq!(plan.playlists, 2);
    assert_eq!(
        plan.analyse, 0,
        "the tags are Serato's analysis, the host makes none"
    );
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    assert!(plan.files >= 7 && plan.write_bytes > 0, "{plan:?}");
    assert!(plan.update.iter().all(|i| i.bytes == 0));
    assert!(plan.add.iter().all(|i| i.bytes > 0 && i.key.contains('|')));
    assert_eq!(plan.add[0].key, track_key(&new.tracks[2].0));
    let size = |n: &str| fs::metadata(d.join("lib").join(n)).unwrap().len();
    assert_eq!(plan.add[0].bytes, size("c.mp3"));
    assert_eq!(
        plan.add[1].bytes,
        2 * size("t0.mp3"),
        "t0 gets a shared and a private copy"
    );
    assert_eq!(
        plan.copy_bytes,
        plan.add.iter().map(|i| i.bytes).sum::<u64>()
    );

    // A stick that does not exist yet stays not existing.
    let nothing = d.join("not-yet");
    let none = export_host(&nothing, &new, &dry()).unwrap();
    assert!(!nothing.exists());
    assert_eq!(none.plan.unwrap().add.len(), 4);

    let real = export_host(&usb, &new, &plain()).unwrap();
    assert!(real.plan.is_none());
    assert_eq!(
        (
            report.tracks,
            report.added,
            report.updated,
            report.playlists
        ),
        (real.tracks, real.added, real.updated, real.playlists)
    );
    assert_eq!(
        (real.tracks, real.added, real.updated, real.playlists),
        (4, 2, 2, 2)
    );
    assert_eq!(
        (&report.warnings, &report.notes),
        (&real.warnings, &real.notes)
    );
    assert!(real.warnings.is_empty());
    assert_eq!(
        real.notes,
        [
            "1 tracks got a private copy under _Serato_/Audio: their cues, loops and grid did not fit the shared copy without changing its size",
            "4 memory cues were not written: Serato has no memory cues",
        ]
    );
    let after = tree(&usb);
    let copied: u64 = after
        .iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !before.contains_key(*k))
        .map(|(_, v)| v.0.len() as u64)
        .sum();
    // The shared copies are the audio as it was; the private copy is that plus the tags it was given.
    let private: u64 = after
        .iter()
        .filter(|(k, _)| {
            k.starts_with("_Serato_/Audio/") && !before.contains_key(*k) && !k.ends_with('/')
        })
        .map(|(_, v)| v.0.len() as u64)
        .sum();
    assert_eq!(copied, size("c.mp3") + size("t0.mp3"));
    assert!(private >= size("t0.mp3") && private < size("t0.mp3") + 8192);
    assert!(plan.copy_bytes >= copied + size("t0.mp3") && plan.copy_bytes <= copied + private);
}

#[test]
fn a_dry_run_counts_what_the_export_reaches_with_nothing_private() {
    let d = tmp("dry-counts");
    let usb = d.join("usb");
    let snap = lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1])]);
    let plan = export_host(&usb, &snap, &dry()).unwrap();
    assert!(tree(&usb).is_empty());
    let real = export_host(&usb, &snap, &plain()).unwrap();
    assert_eq!(
        (
            plan.tracks,
            plan.added,
            plan.updated,
            plan.playlists,
            &plan.warnings,
            &plan.notes
        ),
        (
            real.tracks,
            real.added,
            real.updated,
            real.playlists,
            &real.warnings,
            &real.notes
        )
    );
    let copied: u64 = tree(&usb)
        .iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !k.ends_with('/'))
        .map(|(_, v)| v.0.len() as u64)
        .sum();
    assert_eq!(
        plan.plan.unwrap().copy_bytes,
        copied,
        "the audio bytes actually added"
    );
}

#[test]
fn a_real_export_names_its_phases_counts_its_bytes_and_leaves_nothing_behind() {
    let d = tmp("phases");
    let usb = d.join("usb");
    let snap = lib(&d, &["a", "b", "t0"], 1500, &[("P1", &[0, 1, 2])]);
    let report = export_host(&usb, &snap, &plain()).unwrap();
    assert_eq!(report.verified, Some(true));
    let seen = reports();
    let mut phases: Vec<&str> = seen.iter().map(|s| s.0.as_str()).collect();
    phases.dedup();
    assert_eq!(
        phases,
        [
            "Copying audio",
            "Writing analysis files",
            "Writing the library",
            "Checking the library"
        ],
        "{phases:?}"
    );
    assert!(
        seen.iter().all(|s| s.3 > 0),
        "the byte total was announced up front"
    );
    let total = seen[0].3;
    assert!(
        seen.iter().all(|s| s.2 <= total),
        "more bytes written than announced: {seen:?}"
    );
    for k in tree(&usb).keys() {
        assert!(
            !["ofnew", ".part", ".tmp", "-journal"]
                .iter()
                .any(|s| k.contains(s)),
            "{k} is left behind"
        );
    }
    assert!(!usb.join(commit::JOURNAL).exists());
}

fn size_of(d: &Path, n: &str) -> u64 {
    fs::metadata(d.join("lib").join(n)).unwrap().len()
}

#[test]
fn a_track_over_the_file_limit_is_left_out_whole_never_in_part() {
    let d = tmp("limit");
    let usb = d.join("usb");
    let mut snap = lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1, 2])]);
    let big = d.join("lib/big.mp3");
    fs::write(&big, [id3v23(4096), audio(20000)].concat()).unwrap();
    snap.tracks.push((track(&big, "Big"), Some(analysis(1500))));
    let o = ExportOptions {
        max_file: Some(size_of(&d, "a.mp3")),
        ..plain()
    };
    let planned = export_host(
        &usb,
        &snap,
        &ExportOptions {
            dry_run: true,
            ..o.clone()
        },
    )
    .unwrap();
    let plan = planned.plan.clone().unwrap();
    assert_eq!(plan.add.len(), 2);
    assert_eq!(plan.blocked.len(), 1, "{:?}", plan.blocked);
    assert!(
        plan.blocked[0].starts_with("big.mp3 is ") && plan.blocked[0].contains("up to 0.0 MiB")
    );
    assert_eq!(planned.warnings.len(), 1);
    assert!(tree(&usb).is_empty());

    let real = export_host(&usb, &snap, &o).unwrap();
    assert_eq!(real.added, 2);
    assert_eq!(real.warnings, planned.warnings);
    let files: Vec<(String, usize)> = tree(&usb)
        .into_iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !k.ends_with('/'))
        .map(|(k, v)| (k, v.0.len()))
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    for (name, len) in files {
        let src = name.rsplit('/').next().unwrap();
        assert_eq!(len as u64, size_of(&d, src), "{name} is whole");
    }
    assert_eq!(
        database::paths(&fs::read(usb.join(database::FILE)).unwrap())
            .unwrap()
            .len(),
        2
    );
    whole(&usb, 2, &[("P1", 2)], 1500);
}

#[test]
fn a_private_copy_over_the_file_limit_keeps_the_shared_copy_and_says_so() {
    let d = tmp("limit-private");
    let usb = d.join("usb");
    let snap = lib(&d, &["t0"], 1500, &[("P1", &[0])]);
    let o = ExportOptions {
        max_file: Some(size_of(&d, "t0.mp3")),
        ..plain()
    };
    let planned = export_host(
        &usb,
        &snap,
        &ExportOptions {
            dry_run: true,
            ..o.clone()
        },
    )
    .unwrap();
    assert_eq!(planned.warnings.len(), 1, "{:?}", planned.warnings);
    assert!(planned.warnings[0].contains("private copy would be over"));
    let real = export_host(&usb, &snap, &o).unwrap();
    assert_eq!(real.warnings, planned.warnings);
    assert!(!usb.join("_Serato_/Audio").exists());
    assert_eq!(
        fs::read(usb.join(SHARED).join("t0.mp3")).unwrap(),
        fs::read(d.join("lib/t0.mp3")).unwrap(),
        "the shared copy is untouched"
    );
}

#[test]
fn a_dry_run_says_in_cause_consequence_action_what_stops_the_export() {
    let d = tmp("blocked");
    let usb = d.join("usb");
    let snap = lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1])]);
    let tight = ExportOptions {
        free: Some(1000),
        ..plain()
    };
    let plan = export_host(
        &usb,
        &snap,
        &ExportOptions {
            dry_run: true,
            ..tight.clone()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert_eq!(plan.blocked.len(), 1, "{:?}", plan.blocked);
    assert!(
        plan.blocked[0].starts_with("Not enough space on the stick: about")
            && plan.blocked[0].contains("Nothing is written; free some space"),
        "{:?}",
        plan.blocked
    );
    // The real export stops with the same text and writes nothing.
    let e = export_host(&usb, &snap, &tight).unwrap_err();
    assert_eq!(e, plan.blocked[0]);
    assert!(tree(&usb).is_empty());

    // A name no FAT holds: the track is left out and said so.
    let con = d.join("lib/CON.mp3");
    fs::copy(d.join("lib/a.mp3"), &con).unwrap();
    let mut snap = lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1])]);
    snap.tracks[0].0.file = Some(con);
    let report = export_host(&usb, &snap, &dry()).unwrap();
    let plan = report.plan.unwrap();
    assert_eq!(plan.add.len(), 1);
    assert!(
        plan.blocked[0].contains("CON.mp3") && plan.blocked[0].contains("rename the file"),
        "{:?}",
        plan.blocked
    );
    assert_eq!(report.warnings.len(), 1);
    assert!(tree(&usb).is_empty());

    // A library that cannot be read is not written over.
    fs::create_dir_all(usb.join("_Serato_")).unwrap();
    fs::write(usb.join(database::FILE), b"not a database, only text").unwrap();
    let before = tree(&usb);
    let snap = lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1])]);
    let plan = export_host(&usb, &snap, &dry()).unwrap().plan.unwrap();
    assert_eq!(plan.blocked.len(), 1, "{:?}", plan.blocked);
    assert!(
        plan.blocked[0].contains("cannot be read") && plan.blocked[0].contains("Rename the file")
    );
    let e = export_host(&usb, &snap, &plain()).unwrap_err();
    assert_eq!(e, plan.blocked[0]);
    assert_eq!(before, tree(&usb), "nothing was written, not even a folder");
}

#[test]
fn an_export_that_cannot_write_says_what_to_do_and_leaves_the_stick_as_it_was() {
    let d = tmp("cannot-write");
    let usb = d.join("usb");
    let snap = lib(&d, &["a", "b"], 1500, &[("P1", &[0, 1])]);
    // A file where the folder of the crates must go: staging fails after the audio was copied and tagged.
    fs::create_dir_all(usb.join("_Serato_")).unwrap();
    fs::write(usb.join("_Serato_/Subcrates"), b"in the way").unwrap();
    let before = tree(&usb);
    let e = export_host(&usb, &snap, &plain()).unwrap_err();
    assert!(e.starts_with("Cannot write to the stick: "), "{e}");
    assert_eq!(before, tree(&usb));
}

#[test]
fn a_track_can_hold_more_than_the_database_reads_back_texts_with_accents_and_emoji() {
    let d = tmp("names");
    let usb = d.join("usb");
    let mut snap = lib(&d, &["a"], 1500, &[("Ünï 🎧", &[0])]);
    snap.tracks[0].0.title = "Ünïcødé — 日本語 🎧".into();
    snap.tracks[0].0.artist = "Björk / Sigur Rós".into();
    snap.tracks[0].0.album = "💿 Album: Vol.1?".into();
    let report = export_host(&usb, &snap, &plain()).unwrap();
    assert_eq!(report.verified, Some(true), "{:?}", report.warnings);
    let src = Plugin::open(&usb, &Value::Null).unwrap();
    let t = &src.tracks(None).unwrap()[0];
    assert_eq!(
        (&t.title[..], &t.artist[..], &t.album[..]),
        (
            "Ünïcødé — 日本語 🎧",
            "Björk / Sigur Rós",
            "💿 Album: Vol.1?"
        )
    );
    assert!(check(&usb).problems.is_empty());
}

#[test]
fn the_texts_that_are_hard_to_provoke_are_translated_too() {
    // A file type that holds no Serato tags: a warning of a real export.
    let d = tmp("texts");
    let f = d.join("lib/a.ogg");
    fs::write(&f, audio(500)).unwrap();
    let snap = Snapshot {
        tracks: vec![(track(&f, "Song"), Some(analysis(1500)))],
        ..Snapshot::default()
    };
    let report = export_host(&d.join("usb"), &snap, &plain()).unwrap();
    assert!(
        report.warnings[0].contains("cannot hold Serato tags"),
        "{:?}",
        report.warnings
    );
    // The ones that need a failing disk or a damaged file, worded as the export words them.
    for text in [
        "The tags of /x/a.mp3 cannot be written (Io(\"e\")). The stick was left as it was; check that the file is not damaged or read-only, then export again.",
        "Song: its audio file could not be read for tagging (e), so its cues, loops and grid are not written. Check that the file is there and export again.",
        "The new Serato library did not read back as written (2 tracks instead of 3). Nothing on the stick was changed. Export again; if this repeats, check the stick for errors.",
        "The new Serato library could not be switched in completely (e). The stick finishes the change by itself the next time it is connected to OpenFader; until then the library may be incomplete. Connect it again before you play from it.",
    ] {
        assert_translated(text);
    }
}
