//! All-or-nothing writes on synthetic sticks in temp dirs (never a real stick): a pull-out at every step of the commit, a cancel
//! at every phase, an error after the audio was copied, a dry run that touches nothing, the file size limit and reserved names.

mod common;

use common::{
    assert_translated, cancel_at, copy_tree, export, export_raw, read_back, reports, snapshot,
    tree, with_host, Root,
};
use openfader_plugin::library::{
    commit::{self, Recovered},
    ExportOptions, LibraryPlugin, Severity,
};
use serde_json::Value;
use std::path::Path;
use compat_traktor::{Plugin, FILE};

/// The stick holds one whole generation: `tracks` tracks, `playlists` playlists, `cues_of_a` cues on T0, no change pending, and
/// the checker finds nothing wrong.
fn whole(dev: &Path, tracks: usize, playlists: usize, cues_of_a: usize) {
    assert_eq!(read_back(dev), (tracks, playlists, cues_of_a));
    assert!(commit::pending(dev).is_none());
    let report = Plugin::device_check(dev, &Default::default(), &Value::Null).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.libraries.len(), 1);
    let lib = &report.libraries[0];
    assert_eq!(
        (lib.present, lib.readable, lib.tracks, lib.playlists),
        (true, true, tracks, playlists)
    );
}

#[test]
fn a_pull_out_at_every_step_of_an_export_leaves_one_whole_generation() {
    let r = Root::new("pull");
    export(
        &r.dev(),
        &snapshot(&r, &[0, 1], 1),
        &ExportOptions::default(),
    )
    .unwrap();
    whole(&r.dev(), 2, 2, 1);
    // The second export adds tracks and gives T0 a second cue.
    let second = snapshot(&r, &[0, 1, 2, 3], 2);
    let mut cut = 0;
    for step in 0.. {
        let dev = r.0.join(format!("dev{step}"));
        copy_tree(&r.dev(), &dev);
        let out = with_host_fail(step, || {
            export_raw(&dev, &second, &ExportOptions::default())
        });
        match out {
            Ok(report) => {
                assert_eq!(report.verified, Some(true));
                whole(&dev, 4, 3, 2);
                break;
            }
            Err(e) => {
                assert!(e.contains("injected"), "{e}");
                // Before the recovery the stick holds the old generation or the new one, never a mix.
                let seen = read_back(&dev);
                assert!(
                    seen == (2, 2, 1) || seen == (4, 3, 2),
                    "step {step}: {seen:?}"
                );
                assert!(commit::pending(&dev).is_some(), "step {step}");
                let check = Plugin::device_check(&dev, &Default::default(), &Value::Null).unwrap();
                assert!(
                    check
                        .problems
                        .iter()
                        .any(|p| p.id == "traktor:interrupted" && p.severity == Severity::Warning),
                    "{:?}",
                    check.problems
                );
                let done = commit::recover(&dev).unwrap();
                assert!(
                    matches!(done, Some(Recovered::Finished { .. })),
                    "step {step}"
                );
                whole(&dev, 4, 3, 2);
                cut += 1;
            }
        }
    }
    assert!(cut >= 2, "only {cut} steps were cut");
}

fn with_host_fail<R>(step: usize, f: impl FnOnce() -> R) -> R {
    commit::fail_at(Some(step));
    let out = f();
    commit::fail_at(None);
    out
}

#[test]
fn a_cancel_at_any_phase_leaves_the_device_byte_identical() {
    let r = Root::new("cancel");
    let full = snapshot(&r, &[0, 1, 2, 3], 2);
    export(
        &r.dev(),
        &snapshot(&r, &[0, 1], 1),
        &ExportOptions::default(),
    )
    .unwrap();
    let cases = [
        ("Copying audio", 0),
        ("Copying audio", 2), // two of the four tracks are copied
        ("Writing the library", 0),
        ("Checking the library", 0),
    ];
    for (phase, nth) in cases {
        for (name, existing) in [("existing", true), ("new", false)] {
            let dev = r.0.join(format!("dev-{name}"));
            let _ = std::fs::remove_dir_all(&dev);
            if existing {
                copy_tree(&r.dev(), &dev);
            }
            let before = tree(&dev);
            let err = with_host(|| {
                cancel_at(phase, nth);
                Plugin::export(&dev, &full, &ExportOptions::default(), &Value::Null)
            })
            .unwrap_err();
            assert_eq!(err, "cancelled", "{phase} on a {name} stick");
            assert_eq!(before, tree(&dev), "{phase} #{nth} on a {name} stick");
            assert_eq!(dev.exists(), existing, "{phase} on a {name} stick");
        }
    }
}

#[test]
fn an_error_after_the_audio_was_copied_removes_it_again() {
    let r = Root::new("error");
    let full = snapshot(&r, &[0, 1, 2, 3], 1);
    // A folder where the staged collection goes: staging fails after every track was copied.
    std::fs::create_dir_all(r.dev().join("Traktor/collection.nml.ofnew")).unwrap();
    let before = tree(&r.dev());
    let err = export(&r.dev(), &full, &ExportOptions::default()).unwrap_err();
    assert!(err.contains("could not be written"), "{err}");
    assert_eq!(before, tree(&r.dev()));
    assert!(!r.dev().join("Contents").exists());
    assert!(!r.dev().join(commit::DIR).exists());
}

#[test]
fn a_dry_run_touches_nothing_and_its_plan_is_what_the_export_does() {
    let r = Root::new("dry");
    let full = snapshot(&r, &[0, 1, 2, 3], 1);
    export(
        &r.dev(),
        &snapshot(&r, &[0, 1], 1),
        &ExportOptions::default(),
    )
    .unwrap();
    let before = tree(&r.dev());
    let dry = ExportOptions {
        dry_run: true,
        ..ExportOptions::default()
    };
    let report = export(&r.dev(), &full, &dry).unwrap();
    assert_eq!(before, tree(&r.dev()), "the tree is byte-identical");
    let plan = report.plan.clone().expect("a plan");
    assert_eq!(plan.update.len(), 2, "a and b are on the stick");
    assert_eq!(plan.add.len(), 2, "c and d are new");
    assert_eq!(plan.playlists, 3);
    assert_eq!(plan.analyse, 0, "the format has no analysis files");
    assert!(plan.blocked.is_empty() && plan.files >= 3 && plan.write_bytes > 0);
    assert!(plan.add.iter().all(|i| i.bytes > 0 && i.key.contains('|')));
    assert!(plan.update.iter().all(|i| i.bytes == 0));
    assert_eq!(
        (
            report.added,
            report.updated,
            report.tracks,
            report.playlists
        ),
        (2, 2, 4, 3)
    );

    // A stick that does not exist yet stays not existing.
    let nothing = r.0.join("not-yet");
    let report = export(&nothing, &full, &dry).unwrap();
    assert!(!nothing.exists());
    assert_eq!(report.plan.unwrap().add.len(), 4);

    let real = export(&r.dev(), &full, &ExportOptions::default()).unwrap();
    assert!(real.plan.is_none());
    assert_eq!(
        (
            real.added,
            real.updated,
            real.playlists,
            real.warnings.len()
        ),
        (plan.add.len(), plan.update.len(), plan.playlists, 0)
    );
    let after = tree(&r.dev());
    let copied: u64 = after
        .iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !before.contains_key(*k))
        .map(|(_, v)| v.len() as u64)
        .sum();
    assert_eq!(copied, plan.copy_bytes, "the audio the plan said it copies");
    assert_eq!(
        plan.add.iter().map(|i| i.bytes).sum::<u64>(),
        plan.copy_bytes
    );
    whole(&r.dev(), 4, 3, 1);
}

#[test]
fn a_dry_run_and_a_real_export_say_the_same_when_there_is_no_room() {
    let r = Root::new("room");
    let full = snapshot(&r, &[0, 1, 2, 3], 1);
    let tight = ExportOptions {
        free: Some(1000),
        ..ExportOptions::default()
    };
    let plan = export(
        &r.dev(),
        &full,
        &ExportOptions {
            dry_run: true,
            ..tight.clone()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert_eq!(plan.blocked.len(), 1, "{:?}", plan.blocked);
    let b = &plan.blocked[0];
    assert!(
        b.contains("Not enough space") && b.contains("free some space"),
        "{b}"
    );
    assert_eq!(
        plan.add.len(),
        4,
        "the plan is whole even though it is blocked"
    );
    let err = export(&r.dev(), &full, &tight).unwrap_err();
    assert_eq!(&err, b);
    assert!(!r.dev().exists());
}

#[test]
fn a_collection_that_cannot_be_read_blocks_the_dry_run_and_the_export() {
    let r = Root::new("unreadable");
    std::fs::create_dir_all(r.dev().join("Traktor")).unwrap();
    std::fs::write(r.dev().join(FILE), "not xml").unwrap();
    let full = snapshot(&r, &[0, 1], 1);
    let before = tree(&r.dev());
    let plan = export(
        &r.dev(),
        &full,
        &ExportOptions {
            dry_run: true,
            ..ExportOptions::default()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert_eq!(plan.blocked.len(), 1);
    let err = export(&r.dev(), &full, &ExportOptions::default()).unwrap_err();
    assert_eq!(err, plan.blocked[0]);
    assert_eq!(before, tree(&r.dev()));
}

#[test]
fn a_real_export_names_its_phases_counts_its_bytes_and_leaves_nothing_behind() {
    let r = Root::new("phases");
    let full = snapshot(&r, &[0, 1, 2, 3], 1);
    let report = export(&r.dev(), &full, &ExportOptions::default()).unwrap();
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
        ],
        "no analysis files in this format"
    );
    assert!(seen.iter().any(|s| s.2 > 0), "the byte total was announced");
    for k in tree(&r.dev()).keys() {
        assert!(
            !["ofnew", ".part", ".tmp", "-journal"]
                .iter()
                .any(|s| k.contains(s)),
            "{k} is left behind"
        );
    }
    assert!(!r.dev().join(commit::JOURNAL).exists());
    // The second export has nothing to copy and still writes and checks.
    export(&r.dev(), &full, &ExportOptions::default()).unwrap();
    whole(&r.dev(), 4, 3, 1);
}

#[test]
fn a_track_over_the_file_limit_is_left_out_whole_never_in_part() {
    let r = Root::new("limit");
    let full = snapshot(&r, &[0, 1, 2, 3], 1);
    let size = |n: &str| std::fs::metadata(r.src().join(n)).unwrap().len();
    // c and d are over the limit, a and b are not.
    let o = ExportOptions {
        max_file: Some(size("c.wav") - 1),
        ..ExportOptions::default()
    };
    let dry = export(
        &r.dev(),
        &full,
        &ExportOptions {
            dry_run: true,
            ..o.clone()
        },
    )
    .unwrap();
    let plan = dry.plan.clone().unwrap();
    assert_eq!(plan.add.len(), 2);
    assert_eq!(plan.blocked.len(), 2, "{:?}", plan.blocked);
    assert!(plan.blocked[0].contains("c.wav") && plan.blocked[1].contains("d.wav"));
    assert_eq!(dry.warnings.len(), 2);

    let real = export(&r.dev(), &full, &o).unwrap();
    assert_eq!(real.added, 2);
    assert_eq!(real.warnings.len(), 2, "{:?}", real.warnings);
    let files: Vec<(String, usize)> = tree(&r.dev())
        .into_iter()
        .filter(|(k, _)| k.starts_with("Contents/") && !k.ends_with('/'))
        .map(|(k, v)| (k, v.len()))
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    for (name, len) in files {
        let src = name.rsplit('/').next().unwrap();
        assert_eq!(len as u64, size(src), "{name} is whole");
    }
    // The playlists keep only the tracks that are on the stick.
    assert_eq!(read_back(&r.dev()).0, 2);
}

#[test]
fn a_name_fat_cannot_hold_is_left_out_with_a_sentence() {
    let r = Root::new("reserved");
    let mut full = snapshot(&r, &[0, 1], 1);
    let con = r.src().join("CON.wav");
    std::fs::copy(r.src().join("a.wav"), &con).unwrap();
    full.tracks[0].0.file = Some(con);
    let plan = export(
        &r.dev(),
        &full,
        &ExportOptions {
            dry_run: true,
            ..ExportOptions::default()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert_eq!(plan.add.len(), 1);
    assert_eq!(plan.blocked.len(), 1);
    assert!(
        plan.blocked[0].contains("CON.wav") && plan.blocked[0].contains("rename the file"),
        "{:?}",
        plan.blocked
    );
    let real = export(&r.dev(), &full, &ExportOptions::default()).unwrap();
    assert_eq!(real.added, 1);
    assert_eq!(real.warnings.len(), 1, "{:?}", real.warnings);
    assert!(!r.dev().join("Contents/Art/Alb/CON.wav").exists());
}

#[test]
fn a_track_without_audio_is_left_out_with_a_sentence() {
    let r = Root::new("noaudio");
    let mut full = snapshot(&r, &[0, 1, 2], 1);
    full.tracks[1].0.file = Some(r.src().join("gone.wav"));
    full.tracks[2].0.file = None;
    let real = export(&r.dev(), &full, &ExportOptions::default()).unwrap();
    assert_eq!(real.added, 1);
    assert_eq!(real.warnings.len(), 2, "{:?}", real.warnings);
    assert_eq!(read_back(&r.dev()).0, 1);
}

#[test]
fn an_export_over_an_interrupted_one_recovers_it_first() {
    let r = Root::new("recover");
    export(
        &r.dev(),
        &snapshot(&r, &[0, 1], 1),
        &ExportOptions::default(),
    )
    .unwrap();
    let second = snapshot(&r, &[0, 1, 2, 3], 2);
    let cut = with_host_fail(0, || {
        export_raw(&r.dev(), &second, &ExportOptions::default())
    });
    assert!(cut.unwrap_err().contains("injected"));
    assert_eq!(
        read_back(&r.dev()),
        (2, 2, 1),
        "the old generation is in use"
    );
    // A dry run reads and recovers nothing.
    let before = tree(&r.dev());
    export(
        &r.dev(),
        &second,
        &ExportOptions {
            dry_run: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(before, tree(&r.dev()));
    // The next real export finishes the change, then adds nothing it did not already.
    export(&r.dev(), &second, &ExportOptions::default()).unwrap();
    whole(&r.dev(), 4, 3, 2);
}

#[test]
fn every_text_of_the_plugin_is_translated() {
    for t in [
        "Copying audio",
        "Writing the library",
        "Checking the library",
        "cancelled",
    ] {
        assert_translated(t);
    }
}
