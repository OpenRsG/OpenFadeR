//! All-or-nothing writes on synthetic sticks in temp dirs (never a real stick): a pull-out at every step of the commit, a cancel
//! at every phase, a dry run that touches nothing, the file size limit and names that FAT and the libraries must carry.

mod common;

use common::{cue, fresh_dir, grid, narrowed, reports, settings, snapshot, with_host};
use openfader_plugin::library::{
    commit::{self, Recovered},
    Analysis, ExportOptions, ExportReport, Severity, Snapshot, Track,
};
use compat_rekordbox_common::{anlz, on_device};
use compat_rekordbox_export::{device_check, pdb::read, settings::DeviceConfig};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const KEY: &str = "test-key";

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let r = fresh_dir(&format!("safe-{name}"));
        std::fs::create_dir_all(r.join("src")).unwrap();
        Self(r)
    }
    fn src(&self) -> PathBuf {
        self.0.join("src")
    }
    fn dev(&self) -> PathBuf {
        self.0.join("device")
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Four tracks (a.wav … d.wav, distinct lengths) and the lists P1 = [a, b], P2 = [c], P3 = [a, d]; track a carries `cues_of_a`
/// cues, the others one.
fn setup(r: &Root, cues_of_a: u32) -> Snapshot {
    let tracks: Vec<Track> = (0..4)
        .map(|i| {
            let f = r.src().join(format!("{}.wav", ["a", "b", "c", "d"][i]));
            if !f.exists() {
                common::click_wav(&f, 120.0, 1.0 + i as f64 * 0.5);
            }
            Track {
                title: format!("T{i}"),
                artist: "Art".into(),
                album: "Alb".into(),
                key: "8A".into(),
                genre: "House".into(),
                bpm_x100: 12000,
                length_s: 1,
                file: Some(f),
                date_added: "2026-01-02".into(),
                bitrate: 1411,
                sample_rate: 44100,
                ..Track::default()
            }
        })
        .collect();
    let lists = [
        ("p1", "P1", 0, false, vec![0, 1]),
        ("p2", "P2", 0, false, vec![2]),
        ("p3", "P3", 0, false, vec![0, 3]),
    ];
    snapshot(&tracks, &lists, |t| {
        let n = if t.title == "T0" { cues_of_a } else { 1 };
        Some(Analysis {
            beats: grid(8),
            cues: (1..=n).map(|i| cue(i as u16, i * 1000)).collect(),
            ..Analysis::default()
        })
    })
}

/// An export under the fake host; every text it answers the user with must be translated.
fn export(dev: &Path, snap: &Snapshot, o: &ExportOptions) -> Result<ExportReport, String> {
    let out = common::export(dev, snap, o, Some(KEY));
    match &out {
        Ok(r) => r
            .warnings
            .iter()
            .chain(&r.notes)
            .chain(r.plan.iter().flat_map(|p| &p.blocked))
            .for_each(|t| common::assert_translated(t)),
        Err(e) => common::assert_translated(e),
    }
    out
}

/// Every file and folder below `root` (folders end in `/`) with the bytes of the files.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

fn one(root: &Path) -> Connection {
    let c = Connection::open(root.join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    c.pragma_update(None, "key", KEY).unwrap();
    c
}

/// The stick holds one whole generation: both libraries list `tracks` tracks (the same ones), track T0 has `cues_of_a` cues in
/// its analysis files, no change is pending and the checker finds nothing wrong.
fn whole(dev: &Path, tracks: usize, cues_of_a: usize) {
    let (db, _) = read::open(dev, &DeviceConfig::default()).unwrap();
    assert_eq!(db.tracks.len(), tracks, "Device Library");
    let c = one(dev);
    let mut ids: Vec<u32> = c
        .prepare("select content_id from content")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    ids.sort_unstable();
    let mut want: Vec<u32> = db.tracks.iter().map(|t| t.id).collect();
    want.sort_unstable();
    assert_eq!(ids, want, "both libraries list the same tracks");
    let a = db.tracks.iter().find(|t| t.title == "T0").unwrap();
    let cues = anlz::load_track(&on_device(dev, &a.analyze_path))
        .unwrap()
        .cues
        .len();
    assert_eq!(
        cues, cues_of_a,
        "the analysis of T0 belongs to the same generation"
    );
    assert!(commit::pending(dev).is_none());
    let report = device_check(dev, &BTreeMap::new(), &settings(Some(KEY))).unwrap();
    assert!(
        report
            .problems
            .iter()
            .all(|p| p.severity < Severity::Warning),
        "{:?}",
        report.problems
    );
}

#[test]
fn a_pull_out_at_every_step_of_an_export_leaves_one_whole_generation() {
    let r = Root::new("pull");
    let first = setup(&r, 1);
    export(
        &r.dev(),
        &narrowed(&first, &["p1"]),
        &ExportOptions::default(),
    )
    .unwrap();
    whole(&r.dev(), 2, 1);
    // The second export adds tracks and gives T0 a second cue: the analysis of a track the stick holds is rewritten too.
    let second = setup(&r, 2);
    let mut cut = 0;
    for step in 0.. {
        let dev = r.0.join(format!("dev{step}"));
        copy_tree(&r.dev(), &dev);
        let out = with_host(|| {
            commit::fail_at(Some(step));
            let out = compat_rekordbox_export::export(
                &dev,
                &second,
                &ExportOptions::default(),
                &settings(Some(KEY)),
            );
            commit::fail_at(None);
            out
        });
        match out {
            Ok(report) => {
                assert_eq!(report.verified, Some(true));
                whole(&dev, 4, 2);
                break;
            }
            Err(e) => {
                assert!(e.contains("injected"), "{e}");
                assert!(commit::pending(&dev).is_some(), "step {step}");
                let done = commit::recover(&dev).unwrap();
                assert!(
                    matches!(done, Some(Recovered::Finished { .. })),
                    "step {step}"
                );
                whole(&dev, 4, 2);
                cut += 1;
            }
        }
    }
    // OneLibrary, export.pdb and the two analysis files of T0 that changed: the journal is cut after each rename.
    assert!(cut >= 5, "only {cut} steps were cut");
}

#[test]
fn a_cancel_at_any_phase_leaves_the_device_byte_identical() {
    let r = Root::new("cancel");
    let full = setup(&r, 2);
    export(
        &r.dev(),
        &narrowed(&setup(&r, 1), &["p1"]),
        &ExportOptions::default(),
    )
    .unwrap();
    let existing = tree(&r.dev());
    for phase in [
        "Copying audio",
        "Writing analysis files",
        "Writing the library",
        "Checking the library",
    ] {
        for (name, base) in [("existing", Some(&existing)), ("new", None)] {
            let dev = r.0.join(format!("dev-{name}"));
            let _ = std::fs::remove_dir_all(&dev);
            if base.is_some() {
                copy_tree(&r.dev(), &dev);
            }
            let before = tree(&dev);
            let err = with_host(|| {
                common::cancel_at(phase);
                compat_rekordbox_export::export(
                    &dev,
                    &full,
                    &ExportOptions::default(),
                    &settings(Some(KEY)),
                )
            })
            .unwrap_err();
            assert_eq!(err, "cancelled", "{phase} on a {name} stick");
            assert_eq!(before, tree(&dev), "{phase} on a {name} stick");
            assert!(before.is_empty() || dev.exists());
        }
    }
}

#[test]
fn a_dry_run_touches_nothing_and_its_plan_is_what_the_export_does() {
    let r = Root::new("dry");
    let full = setup(&r, 1);
    export(
        &r.dev(),
        &narrowed(&full, &["p1"]),
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
    assert_eq!(
        plan.analyse, 4,
        "the snapshot carries no waveform: the host analyses all four"
    );
    assert!(plan.blocked.is_empty() && plan.files >= 12 && plan.write_bytes > 0);
    assert!(plan.add.iter().all(|i| i.bytes > 0 && i.key.contains('|')));
    assert!(plan.update.iter().all(|i| i.bytes == 0));

    // A stick that does not exist yet stays not existing.
    let nothing = r.0.join("not-yet");
    let report = export(&nothing, &full, &dry).unwrap();
    assert!(!nothing.exists());
    assert_eq!(report.plan.unwrap().add.len(), 4);

    let real = export(&r.dev(), &full, &ExportOptions::default()).unwrap();
    assert!(real.plan.is_none());
    assert_eq!(
        (real.added, real.updated, real.playlists),
        (plan.add.len(), plan.update.len(), plan.playlists)
    );
    let after = tree(&r.dev());
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
}

#[test]
fn a_real_export_names_its_phases_counts_its_bytes_and_leaves_nothing_behind() {
    let r = Root::new("phases");
    let full = setup(&r, 1);
    let report = export(&r.dev(), &full, &ExportOptions::default()).unwrap();
    assert_eq!(report.verified, Some(true));
    let seen = reports();
    let mut phases: Vec<&str> = seen.iter().map(|s| s.0.as_str()).collect();
    phases.dedup();
    let mut order = [
        "Copying audio",
        "Writing analysis files",
        "Writing the library",
        "Checking the library",
    ]
    .into_iter();
    let mut next = order.next();
    for p in &phases {
        if Some(*p) == next {
            next = order.next();
        }
    }
    assert!(next.is_none(), "phases seen: {phases:?}");
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
}

#[test]
fn a_track_over_the_file_limit_is_left_out_whole_never_in_part() {
    let r = Root::new("limit");
    let full = setup(&r, 1);
    let size = |n: &str| std::fs::metadata(r.src().join(n)).unwrap().len();
    // c and d are over the limit, a and b are not.
    let limit = size("c.wav") - 1;
    let o = ExportOptions {
        max_file: Some(limit),
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
    let plan = dry.plan.unwrap();
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
    let (db, _) = read::open(&r.dev(), &DeviceConfig::default()).unwrap();
    assert_eq!(db.tracks.len(), 2);
    whole_two(&r.dev());
}

/// Both libraries hold exactly two tracks and the checker is content.
fn whole_two(dev: &Path) {
    let c: i64 = one(dev)
        .query_row("select count(*) from content", [], |r| r.get(0))
        .unwrap();
    assert_eq!(c, 2);
    let report = device_check(dev, &BTreeMap::new(), &settings(Some(KEY))).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

#[test]
fn accents_emoji_long_names_and_deep_folders_export_and_read_back() {
    let r = Root::new("names");
    let deep = r
        .src()
        .join("a/very/deep/folder/tree/of/music/that/goes/on");
    std::fs::create_dir_all(&deep).unwrap();
    let long = "Ä".repeat(200);
    let names = [
        (
            "Ünïcødé — 日本語 🎧",
            "Björk / Sigur Rós",
            "💿 Album: Vol.1?",
            "é à ü.wav",
        ),
        (long.as_str(), long.as_str(), long.as_str(), "long.wav"),
        ("Dots...", "Trailing dots.. ", "…", "dots. .wav"),
    ];
    let tracks: Vec<Track> = names
        .iter()
        .enumerate()
        .map(|(i, (title, artist, album, file))| {
            let f = deep.join(file);
            common::click_wav(&f, 120.0, 1.0 + i as f64 * 0.25);
            Track {
                title: (*title).into(),
                artist: (*artist).into(),
                album: (*album).into(),
                file: Some(f),
                length_s: 1,
                bpm_x100: 12000,
                ..Track::default()
            }
        })
        .collect();
    let snap = snapshot(
        &tracks,
        &[("p", "Ünï 🎧", 0, false, vec![0, 1, 2])],
        |_| {
            Some(Analysis {
                beats: grid(4),
                ..Analysis::default()
            })
        },
    );
    let report = export(&r.dev(), &snap, &ExportOptions::default()).unwrap();
    assert_eq!(report.added, 3, "{:?}", report.warnings);
    let (db, _) = read::open(&r.dev(), &DeviceConfig::default()).unwrap();
    for (t, (title, artist, album, _)) in db.tracks.iter().zip(&names) {
        assert_eq!(
            (&t.title[..], &t.artist[..], &t.album[..]),
            (*title, *artist, *album)
        );
        let path = on_device(&r.dev(), &t.file_path);
        assert!(path.is_file(), "{}", path.display());
        assert!(
            t.file_path.chars().count() <= 255,
            "{} is longer than a row holds",
            t.file_path
        );
        for part in t.file_path.trim_start_matches('/').split('/') {
            assert!(
                part.chars().count() <= 120 && !part.ends_with(['.', ' ']),
                "{part}"
            );
        }
    }
    let c = one(&r.dev());
    let titles: Vec<String> = c
        .prepare("select title from content order by content_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(titles, names.map(|n| n.0.to_owned()));
    let list: String = c
        .query_row("select name from playlist", [], |r| r.get(0))
        .unwrap();
    assert_eq!(list, "Ünï 🎧");
    let check = device_check(&r.dev(), &BTreeMap::new(), &settings(Some(KEY))).unwrap();
    assert!(check.problems.is_empty(), "{:?}", check.problems);
}

#[test]
fn a_dry_run_says_in_cause_consequence_action_what_stops_the_export() {
    let r = Root::new("blocked");
    let full = setup(&r, 1);
    let dry = |o: ExportOptions| {
        export(&r.dev(), &full, &ExportOptions { dry_run: true, ..o })
            .unwrap()
            .plan
            .unwrap()
    };
    let tight = dry(ExportOptions {
        free: Some(1000),
        ..ExportOptions::default()
    });
    assert_eq!(tight.blocked.len(), 1, "{:?}", tight.blocked);
    assert!(
        tight.blocked[0].starts_with("Not enough space on the stick: about")
            && tight.blocked[0].contains("Nothing is written; free some space"),
        "{:?}",
        tight.blocked
    );
    // The real export stops with the same text and writes nothing.
    let e = export(
        &r.dev(),
        &full,
        &ExportOptions {
            free: Some(1000),
            ..ExportOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(e, tight.blocked[0]);
    assert!(!r.dev().exists());

    // A name no FAT holds: the track is left out and said so.
    let con = r.src().join("CON.wav");
    std::fs::copy(r.src().join("a.wav"), &con).unwrap();
    let mut snap = setup(&r, 1);
    snap.tracks[0].0.file = Some(con);
    let plan = export(
        &r.dev(),
        &snap,
        &ExportOptions {
            dry_run: true,
            ..ExportOptions::default()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert_eq!(plan.add.len(), 3);
    assert!(
        plan.blocked[0].contains("CON.wav") && plan.blocked[0].contains("rename the file"),
        "{:?}",
        plan.blocked
    );

    // More lists than a stick holds.
    let many = Snapshot {
        playlists: (0..1001)
            .map(|i| openfader_plugin::library::Playlist {
                id: format!("p{i}"),
                name: format!("L{i}"),
                depth: 0,
                is_folder: false,
            })
            .collect(),
        members: vec![Vec::new(); 1001],
        ..Snapshot::default()
    };
    let plan = export(
        &r.dev(),
        &many,
        &ExportOptions {
            dry_run: true,
            ..ExportOptions::default()
        },
    )
    .unwrap()
    .plan
    .unwrap();
    assert!(
        plan.blocked[0].contains("cannot exceed 1000 on one stick"),
        "{:?}",
        plan.blocked
    );
    assert!(!r.dev().exists());
}
