//! All-or-nothing writes on synthetic sticks in temp dirs (never a real stick): a pull-out at every step of the commit, a cancel
//! at every phase, a dry run that touches nothing, the file size limit and names FAT reserves.

mod common;

use common::{assert_translated, cancel_at, reports, snapshot, track, tree, with_host, Dir};
use openfader_plugin::library::{
    commit, ExportOptions, ExportReport, LibraryPlugin, Severity, Snapshot,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use compat_virtualdj::{Plugin, DATABASE, LIBRARY};

/// An export under the fake host; every text it answers the user with must be translated.
fn export(root: &Path, s: &Snapshot, o: &ExportOptions) -> Result<ExportReport, String> {
    let out = raw(root, s, o);
    match &out {
        Ok(r) => r
            .warnings
            .iter()
            .chain(&r.notes)
            .chain(r.plan.iter().flat_map(|p| &p.blocked))
            .for_each(|t| assert_translated(t)),
        Err(e) => assert_translated(e),
    }
    out
}

/// The same without the translation check (an injected failure is not a user text).
fn raw(root: &Path, s: &Snapshot, o: &ExportOptions) -> Result<ExportReport, String> {
    with_host(|| Plugin::export(root, s, o, &json!({})))
}

fn dry() -> ExportOptions {
    ExportOptions {
        dry_run: true,
        ..Default::default()
    }
}

/// `snapshot` plus a third track and a list holding it.
fn bigger(src: &Dir) -> Snapshot {
    let mut s = snapshot(src);
    s.tracks.push((track(src, "c.wav", "Third", 900), None));
    s.playlists.push(common::playlist("Extra", 0, false));
    s.members.push(vec![2]);
    s
}

fn audio(t: &BTreeMap<String, Vec<u8>>) -> u64 {
    t.iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !k.ends_with('/'))
        .map(|(_, v)| v.len() as u64)
        .sum()
}

/// The stick holds one whole generation: `songs` songs and `lists` lists read back, the checker finds nothing wrong, and
/// nothing of a change is left.
fn whole(root: &Path, songs: usize, lists: usize) {
    let src = Plugin::open(root, &json!({})).unwrap();
    assert_eq!(src.tracks(None).unwrap().len(), songs);
    let got = src
        .playlists()
        .unwrap()
        .iter()
        .filter(|p| !p.is_folder)
        .count();
    assert_eq!(got, lists);
    let c = Plugin::device_check(root, &BTreeMap::new(), &json!({})).unwrap();
    assert!(
        c.libraries[0].readable && c.libraries[0].tracks == songs,
        "{c:?}"
    );
    assert!(
        c.problems.iter().all(|p| p.severity < Severity::Error),
        "{c:?}"
    );
    nothing_left(root);
}

fn nothing_left(root: &Path) {
    for k in tree(root).keys() {
        assert!(
            !k.ends_with(".ofnew") && !k.ends_with(".part") && !k.ends_with("-journal"),
            "left behind: {k}"
        );
        assert_ne!(k, ".openfader/journal.json");
    }
}

#[test]
fn a_pull_out_at_every_step_leaves_one_whole_generation() {
    let src = Dir::new("pull-src");
    let mut step = 0;
    loop {
        let stick = Dir::new("pull-stick");
        export(&stick.0, &snapshot(&src), &Default::default()).unwrap();
        whole(&stick.0, 2, 2);
        commit::fail_at(Some(step));
        let r = raw(&stick.0, &bigger(&src), &Default::default());
        commit::fail_at(None);
        if r.is_ok() {
            whole(&stick.0, 3, 3);
            assert!(
                step >= 4,
                "the commit has at least the lists and the database"
            );
            break;
        }
        // The change was switching: the host finishes it, so the stick holds the new generation whole, never a mix.
        assert!(commit::recover(&stick.0).unwrap().is_some());
        whole(&stick.0, 3, 3);
        let names: Vec<String> = tree(&stick.0).into_keys().collect();
        assert!(
            names.iter().any(|n| n.ends_with("Extra.vdjfolder")),
            "{names:?}"
        );
        step += 1;
    }
}

#[test]
fn a_cancel_leaves_the_stick_byte_identical() {
    let src = Dir::new("cancel-src");
    let cases = [
        ("Copying audio", 0),
        ("Copying audio", 1),
        ("Writing the library", 0),
        ("Checking the library", 0),
    ];
    for (phase, n) in cases {
        // A stick that holds an older export (and the host's folder), and an empty one (mid-copy: one track is placed).
        for existing in [true, false] {
            let stick = Dir::new("cancel-stick");
            if existing {
                std::fs::create_dir_all(stick.join(".openfader")).unwrap();
                export(&stick.0, &snapshot(&src), &Default::default()).unwrap();
            }
            let before = tree(&stick.0);
            let s = if existing {
                bigger(&src)
            } else {
                snapshot(&src)
            };
            let r = with_host(|| {
                cancel_at(phase, n);
                Plugin::export(&stick.0, &s, &Default::default(), &json!({}))
            });
            assert_eq!(r.unwrap_err(), "cancelled", "{phase} {n} {existing}");
            assert_eq!(tree(&stick.0), before, "{phase} {n} existing={existing}");
        }
        // A device folder that does not exist: nothing is created.
        let gone = src.join("no-such-stick");
        let r = raw(&gone, &snapshot(&src), &Default::default());
        assert!(r.is_err());
        assert!(!gone.exists());
    }
    // The host's folder is only removed when the run made it.
    let stick = Dir::new("cancel-fresh");
    with_host(|| {
        cancel_at("Checking the library", 0);
        let _ = Plugin::export(&stick.0, &snapshot(&src), &Default::default(), &json!({}));
    });
    assert!(tree(&stick.0).is_empty());
}

#[test]
fn concurrent_exports_verify_their_own_library_and_keep_other_sticks_intact() {
    let start = std::sync::Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        for i in 0..8 {
            let start = start.clone();
            scope.spawn(move || {
                let src = Dir::new(&format!("parallel-src-{i}"));
                let stick = Dir::new(&format!("parallel-stick-{i}"));
                let first = snapshot(&src);
                let next = bigger(&src);
                start.wait();
                export(&stick.0, &first, &Default::default()).unwrap();
                export(&stick.0, &next, &Default::default()).unwrap();
                whole(&stick.0, 3, 3);
            });
        }
    });
}

#[test]
fn a_dry_run_touches_nothing_and_matches_the_real_export() {
    let src = Dir::new("dry-src");
    let stick = Dir::new("dry-stick");
    export(&stick.0, &snapshot(&src), &Default::default()).unwrap();
    let before = tree(&stick.0);
    let plan = export(&stick.0, &bigger(&src), &dry()).unwrap();
    assert_eq!(tree(&stick.0), before);
    let p = plan.plan.clone().unwrap();
    assert!(p.blocked.is_empty(), "{p:?}");
    assert_eq!((p.add.len(), p.update.len(), p.analyse), (1, 2, 0));
    assert_eq!(p.add[0].title, "Third");
    assert!(p.add[0].bytes > 0 && p.update.iter().all(|u| u.bytes == 0));
    assert_eq!(p.playlists, 4);
    assert_eq!(p.files, 4);
    assert!(p.write_bytes > 0);

    let real = export(&stick.0, &bigger(&src), &Default::default()).unwrap();
    let after = tree(&stick.0);
    assert!(real.plan.is_none());
    assert_eq!(real.verified, Some(true));
    assert_eq!(p.copy_bytes, audio(&after) - audio(&before));
    assert_eq!(
        (plan.added, plan.updated, plan.tracks, plan.playlists),
        (real.added, real.updated, real.tracks, real.playlists)
    );
    assert_eq!(plan.warnings, real.warnings);
    assert_eq!(plan.notes, real.notes);
    assert_eq!(p.add.len(), real.added);
    assert_eq!(p.update.len(), real.updated);

    // A stick without anything, and one that does not exist.
    let empty = Dir::new("dry-empty");
    let plan = export(&empty.0, &snapshot(&src), &dry())
        .unwrap()
        .plan
        .unwrap();
    assert!(tree(&empty.0).is_empty());
    assert_eq!((plan.add.len(), plan.update.len()), (2, 0));
    let gone = src.join("no-such-stick");
    let plan = export(&gone, &snapshot(&src), &dry())
        .unwrap()
        .plan
        .unwrap();
    assert!(!gone.exists());
    let err = export(&gone, &snapshot(&src), &Default::default()).unwrap_err();
    assert_eq!(plan.blocked, vec![err]);
}

#[test]
fn no_room_is_the_same_text_in_the_plan_and_the_error() {
    let src = Dir::new("room-src");
    let stick = Dir::new("room-stick");
    let o = ExportOptions {
        free: Some(100),
        ..Default::default()
    };
    let plan = export(
        &stick.0,
        &snapshot(&src),
        &ExportOptions {
            dry_run: true,
            ..o.clone()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    let err = export(&stick.0, &snapshot(&src), &o).unwrap_err();
    assert_eq!(plan.blocked, vec![err]);
    assert!(tree(&stick.0).is_empty());
}

#[test]
fn a_track_over_the_file_limit_or_with_a_reserved_name_is_left_out_whole() {
    let src = Dir::new("big-src");
    let mut s = snapshot(&src); // a.wav 444 bytes, b.wav 744 bytes
    s.tracks
        .push((track(&src, "CON.wav", "Reserved", 300), None));
    let o = ExportOptions {
        max_file: Some(600),
        ..Default::default()
    };
    let stick = Dir::new("big-stick");
    let plan = export(
        &stick.0,
        &s,
        &ExportOptions {
            dry_run: true,
            ..o.clone()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert_eq!(plan.blocked.len(), 2, "{plan:?}");
    assert!(plan.blocked.iter().any(|b| b.contains("b.wav")));
    assert!(plan.blocked.iter().any(|b| b.contains("CON.wav")));
    assert_eq!(plan.add.len(), 1);
    let r = export(&stick.0, &s, &o).unwrap();
    assert_eq!(r.added, 1);
    assert_eq!(
        r.warnings.iter().filter(|w| w.contains("left out")).count(),
        2,
        "{:?}",
        r.warnings
    );
    let t = tree(&stick.0);
    assert!(t.keys().any(|k| k.ends_with("/a.wav")));
    assert!(!t
        .keys()
        .any(|k| k.ends_with("b.wav") || k.ends_with("CON.wav")));
    whole(&stick.0, 1, 2);
}

#[test]
fn phases_come_in_order_bytes_are_announced_and_nothing_is_left() {
    let src = Dir::new("phase-src");
    let stick = Dir::new("phase-stick");
    let r = export(&stick.0, &snapshot(&src), &Default::default()).unwrap();
    assert_eq!(r.verified, Some(true));
    let seen = reports();
    let mut phases: Vec<&str> = seen.iter().map(|(p, _, _)| p.as_str()).collect();
    phases.dedup();
    assert_eq!(
        phases,
        [
            "Copying audio",
            "Writing the library",
            "Checking the library"
        ]
    );
    let copied = audio(&tree(&stick.0));
    assert!(
        seen.iter()
            .all(|(_, _, total)| *total >= copied && *total > 0),
        "{seen:?}"
    );
    nothing_left(&stick.0);
    assert!(tree(&stick.0).contains_key(DATABASE));
    // The library name the host knows.
    let info = Plugin::device_probe(&stick.0, &json!({})).unwrap();
    assert_eq!(info.libraries, [LIBRARY]);
}
