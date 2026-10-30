//! All-or-nothing writes on synthetic sticks in temp dirs (never a real stick): a pull-out at every step of the commit, a cancel
//! at every phase, a dry run that touches nothing, the file size limit and names FAT cannot hold.

mod common;

use common::{
    assert_translated, cancel_at, copy_tree, read_counts, reports, setup, track, tree, with_host,
    Dirs,
};
use compat_engine_dj::{check, database};
use openfader_plugin::library::{
    commit::{self, Recovered},
    ExportOptions, ExportReport, LibraryPlugin, Severity, Snapshot,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// An export under the fake host; every text it answers the user with must be translated.
fn export_to(root: &Path, snap: &Snapshot, o: &ExportOptions) -> Result<ExportReport, String> {
    let out = with_host(|| compat_engine_dj::Plugin::export(root, snap, o, &Value::Null));
    match &out {
        Ok(r) => r
            .warnings
            .iter()
            .chain(&r.notes)
            .chain(r.plan.iter().flat_map(|p| &p.blocked))
            .for_each(|t| assert_translated(t)),
        Err(e) if e != "cancelled" && !e.contains("injected") => assert_translated(e),
        Err(_) => {}
    }
    out
}

fn dry() -> ExportOptions {
    ExportOptions {
        dry_run: true,
        ..Default::default()
    }
}

/// The snapshot of `setup` plus a fourth track D and a list holding it, and one more cue on A.
fn second(d: &Dirs) -> Snapshot {
    let mut s = setup(d);
    let mut a = s.tracks[0].1.take().unwrap();
    a.cues.push(common::cue(4, 2000, None, None, None));
    s.tracks[0].1 = Some(a);
    s.tracks
        .push((track(d, "d.wav", "Delta", "Other", 3), None));
    s.playlists.push(common::list("Extra", 0, false));
    s.members.push(vec![3, 0]);
    s
}

/// The stick holds one whole generation: `tracks` tracks and the library is not damaged.
fn whole(root: &Path, tracks: usize) {
    assert_eq!(read_counts(root).0, tracks);
    assert!(commit::pending(root).is_none());
    let r = with_host(|| compat_engine_dj::Plugin::device_check(root, &BTreeMap::new(), &Value::Null))
        .unwrap();
    assert!(
        r.problems.iter().all(|p| p.severity < Severity::Warning),
        "{:?}",
        r.problems
    );
}

#[test]
fn a_pull_out_at_every_step_of_an_export_leaves_one_whole_generation() {
    let d = Dirs::new("pull");
    let first = setup(&d);
    let next = second(&d);
    export_to(&d.stick(), &first, &ExportOptions::default()).unwrap();
    whole(&d.stick(), 2);
    let old_counts = read_counts(&d.stick());
    // An existing stick, and a stick that has nothing yet.
    for (name, from) in [("existing", Some(d.stick())), ("empty", None)] {
        let mut cut = 0;
        for step in 0.. {
            let dev = d.stick().with_file_name(format!("dev-{name}-{step}"));
            if let Some(from) = &from {
                copy_tree(from, &dev);
            }
            let out = with_host(|| {
                commit::fail_at(Some(step));
                let out =
                    compat_engine_dj::Plugin::export(&dev, &next, &ExportOptions::default(), &Value::Null);
                commit::fail_at(None);
                out
            });
            match out {
                Ok(report) => {
                    assert_eq!(report.verified, Some(true));
                    whole(&dev, 3);
                    break;
                }
                Err(e) => {
                    assert!(e.contains("injected"), "{e}");
                    assert!(commit::pending(&dev).is_some(), "step {step}");
                    if step == 0 && from.is_some() {
                        // The switch has not started: the old generation is still the one in use.
                        assert_eq!(read_counts(&dev), old_counts);
                    }
                    let done = commit::recover(&dev).unwrap();
                    assert!(
                        matches!(done, Some(Recovered::Finished { .. })),
                        "step {step}"
                    );
                    whole(&dev, 3);
                    cut += 1;
                }
            }
        }
        assert_eq!(
            cut, 2,
            "after the journal and after the one rename ({name})"
        );
    }
}

#[test]
fn a_cancel_at_any_phase_leaves_the_device_byte_identical() {
    let d = Dirs::new("cancel");
    export_to(&d.stick(), &setup(&d), &ExportOptions::default()).unwrap();
    let next = second(&d);
    // (phase, n-th report, on a stick that holds a library already): the extra copy of the existing stick is one track only.
    let cases = [
        ("Copying audio", 1, true),
        ("Copying audio", 1, false),
        ("Copying audio", 2, false),
        ("Writing the library", 1, true),
        ("Writing the library", 2, true),
        ("Writing the library", 2, false),
        ("Checking the library", 1, true),
        ("Checking the library", 2, true),
        ("Checking the library", 2, false),
    ];
    for (i, (phase, n, existing)) in cases.into_iter().enumerate() {
        let dev = d.stick().with_file_name(format!("dev-{i}"));
        if existing {
            copy_tree(&d.stick(), &dev);
        }
        let before = tree(&dev);
        let err = with_host(|| {
            cancel_at(phase, n);
            compat_engine_dj::Plugin::export(&dev, &next, &ExportOptions::default(), &Value::Null)
        })
        .unwrap_err();
        assert_eq!(err, "cancelled", "{phase} #{n}");
        assert_eq!(
            before,
            tree(&dev),
            "{phase} #{n} on an existing stick: {existing}"
        );
        assert_eq!(dev.exists(), existing, "{phase} #{n}");
        // The stick is still good for the next export.
        export_to(&dev, &next, &ExportOptions::default()).unwrap();
        whole(&dev, 3);
    }
}

#[test]
fn an_error_before_the_commit_leaves_the_device_byte_identical() {
    // A library that cannot be extended: not an Engine database at all.
    let d = Dirs::new("error");
    let dev = d.stick();
    let db = database(&dev);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(
        &db,
        b"this is not a database, only text of some length ........",
    )
    .unwrap();
    let before = tree(&dev);
    let e = export_to(&dev, &setup(&d), &ExportOptions::default()).unwrap_err();
    assert!(e.contains("is not an Engine library"), "{e}");
    assert_eq!(before, tree(&dev));
}

#[test]
fn a_dry_run_touches_nothing_and_its_plan_is_what_the_export_does() {
    let d = Dirs::new("dry");
    export_to(&d.stick(), &setup(&d), &ExportOptions::default()).unwrap();
    let next = second(&d);
    // The old file's journal files, empty: the real export removes them, the dry run does not.
    let db = database(&d.stick());
    for s in ["-wal", "-shm"] {
        std::fs::write(compat_engine_dj::schema::sidecar(&db, s), b"").unwrap();
    }
    let before = tree(&d.stick());
    let report = export_to(&d.stick(), &next, &dry()).unwrap();
    assert_eq!(before, tree(&d.stick()), "the tree is byte-identical");
    let plan = report.plan.clone().expect("a plan");
    assert_eq!(plan.update.len(), 2, "A and B are in the library");
    assert_eq!(plan.add.len(), 1, "D is new");
    assert_eq!(plan.add[0].title, "Delta");
    assert_eq!(plan.playlists, 4);
    assert_eq!(plan.analyse, 1, "D comes without a waveform and grid");
    assert!(plan.blocked.is_empty() && plan.files >= 2 && plan.write_bytes > 0);
    assert!(plan.add.iter().all(|i| i.bytes > 0 && i.key.contains('|')));
    assert!(plan.update.iter().all(|i| i.bytes == 0));
    assert_eq!(
        report.warnings.len(),
        1,
        "C has no audio file: {:?}",
        report.warnings
    );

    // A stick that does not exist yet stays not existing.
    let nothing = d.stick().with_file_name("not-yet");
    let report = export_to(&nothing, &next, &dry()).unwrap();
    assert!(!nothing.exists());
    assert_eq!(report.plan.unwrap().add.len(), 3);

    let real = export_to(&d.stick(), &next, &ExportOptions::default()).unwrap();
    assert!(real.plan.is_none());
    assert_eq!(
        (
            real.added,
            real.updated,
            real.tracks,
            real.playlists,
            real.warnings.len()
        ),
        (
            plan.add.len(),
            plan.update.len(),
            report_tracks(&plan),
            plan.playlists,
            report.warnings.len()
        )
    );
    assert!(
        real.notes
            .iter()
            .any(|n| n.starts_with(&format!("{} new tracks have no waveform", plan.analyse))),
        "{:?}",
        real.notes
    );
    let after = tree(&d.stick());
    let copied: u64 = after
        .iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !before.contains_key(*k))
        .map(|(_, v)| v.len() as u64)
        .sum();
    assert_eq!(
        copied, plan.copy_bytes,
        "the audio the plan said it would copy"
    );
    assert_eq!(
        plan.add.iter().map(|i| i.bytes).sum::<u64>(),
        plan.copy_bytes
    );
    assert!(!after
        .keys()
        .any(|k| k.ends_with("-wal") || k.ends_with("-shm")));
}

fn report_tracks(plan: &openfader_plugin::library::ExportPlan) -> usize {
    plan.add.len() + plan.update.len()
}

#[test]
fn a_real_export_names_its_phases_counts_its_bytes_and_leaves_nothing_behind() {
    let d = Dirs::new("phases");
    let report = export_to(&d.stick(), &setup(&d), &ExportOptions::default()).unwrap();
    assert_eq!(report.verified, Some(true));
    let seen = reports();
    let mut phases: Vec<&str> = seen.iter().map(|s| s.0.as_str()).collect();
    phases.dedup();
    assert_eq!(
        phases,
        [
            "Copying audio",
            "Writing the library",
            "Checking the library"
        ]
    );
    assert!(seen.iter().any(|s| s.2 > 0), "the byte total was announced");
    assert!(seen.iter().any(|s| s.1 > 0), "bytes were counted");
    for k in tree(&d.stick()).keys() {
        assert!(
            !["ofnew", ".part", ".tmp", "-journal", "-wal", "-shm"]
                .iter()
                .any(|s| k.contains(s)),
            "{k} is left behind"
        );
    }
    assert!(!d.stick().join(commit::JOURNAL).exists());
}

#[test]
fn a_track_over_the_file_limit_is_left_out_whole_never_in_part() {
    let d = Dirs::new("limit");
    let snap = setup(&d);
    let size = |n: &str| std::fs::metadata(d.src().join(n)).unwrap().len();
    // A (12 s) is over the limit, B (8 s) is not.
    let o = ExportOptions {
        max_file: Some(size("b.wav")),
        ..Default::default()
    };
    let plan = export_to(
        &d.stick(),
        &snap,
        &ExportOptions {
            dry_run: true,
            ..o.clone()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert_eq!(plan.add.len(), 1);
    assert_eq!(plan.blocked.len(), 1, "{:?}", plan.blocked);
    assert!(plan.blocked[0].contains("a.wav"), "{:?}", plan.blocked);

    let real = export_to(&d.stick(), &snap, &o).unwrap();
    assert_eq!(real.added, 1);
    assert_eq!(
        real.warnings.len(),
        2,
        "A over the limit, C without audio: {:?}",
        real.warnings
    );
    let files: Vec<(String, usize)> = tree(&d.stick())
        .into_iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !k.ends_with('/'))
        .map(|(k, v)| (k, v.len()))
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].1 as u64, size("b.wav"), "B is whole");
    assert_eq!(read_counts(&d.stick()).0, 1);
    whole(&d.stick(), 1);
}

#[test]
fn a_dry_run_says_in_cause_consequence_action_what_stops_the_export() {
    let d = Dirs::new("blocked");
    let nowhere = d.stick().with_file_name("nowhere");
    let snap = setup(&d);
    let tight = ExportOptions {
        free: Some(1000),
        ..Default::default()
    };
    let plan = export_to(
        &nowhere,
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
    let e = export_to(&nowhere, &snap, &tight).unwrap_err();
    assert_eq!(e, plan.blocked[0]);
    assert!(!nowhere.exists());

    // A name no FAT holds: the track is left out and said so.
    let con = d.src().join("CON.wav");
    std::fs::copy(d.src().join("a.wav"), &con).unwrap();
    let mut snap = setup(&d);
    snap.tracks[0].0.file = Some(con);
    let report = export_to(&nowhere, &snap, &dry()).unwrap();
    let plan = report.plan.unwrap();
    assert_eq!(plan.add.len(), 1);
    assert!(
        plan.blocked[0].contains("CON.wav") && plan.blocked[0].contains("rename the file"),
        "{:?}",
        plan.blocked
    );
    assert!(!nowhere.exists());

    // The old two-file layout and an unknown schema option are told the same way, in the plan and in the real export.
    let old = d.stick().with_file_name("old");
    std::fs::create_dir_all(old.join("Engine Library")).unwrap();
    std::fs::write(old.join("Engine Library").join("m.db"), b"x").unwrap();
    let plan = export_to(&old, &snap, &dry()).unwrap().plan.unwrap();
    assert_eq!(plan.blocked.len(), 1);
    assert!(plan.add.is_empty());
    assert_eq!(
        export_to(&old, &snap, &ExportOptions::default()).unwrap_err(),
        plan.blocked[0]
    );
    let mut o = ExportOptions::default();
    o.config
        .insert(compat_engine_dj::SCHEMA_OPTION.into(), "1.0.0".into());
    let e = export_to(&nowhere, &snap, &o).unwrap_err();
    assert!(e.contains("1.0.0") && !nowhere.exists(), "{e}");
}

#[test]
fn the_check_report_has_the_library_the_players_file_names() {
    let players: openfader_plugin::library::Players = serde_json::from_str(
        &std::fs::read_to_string(format!("{}/players.json", env!("CARGO_MANIFEST_DIR"))).unwrap(),
    )
    .unwrap();
    assert_eq!(players.version, 1);
    assert!(players.presets.is_empty() && !players.players.is_empty());
    let d = Dirs::new("players");
    let libraries = check::check(&d.stick()).unwrap().libraries;
    for p in &players.players {
        assert_eq!(p.status, "to_measure", "{}", p.model);
        assert!(p.file_systems.is_empty() && p.partition_tables.is_empty());
        assert!(p.note.contains("not measured"));
        assert_translated(&p.note);
        for r in &p.reads {
            assert!(libraries.iter().any(|l| &l.name == r), "{r}");
        }
    }
    assert!(compat_engine_dj::Plugin::device_probe(&d.stick(), &Value::Null)
        .unwrap()
        .features
        .contains(&openfader_plugin::library::Feature::Plan));
}
