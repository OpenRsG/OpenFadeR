//! The stick checker (`device_check`) and its fixes (`device_fix`) on synthetic sticks in temp dirs: every fault that is injected is
//! found with the right severity, fix and numbers, the fix repairs it, and the stick checks clean afterwards.

mod common;

use common::{cue, fresh_dir, grid, settings, snapshot, with_host};
use openfader_plugin::library::{
    commit, ActionAnswer, Analysis, CheckReport, ExportOptions, Problem, Severity, Snapshot, Track,
};
use rekordbox_common::on_device;
use rekordbox_export::{
    check::{DL, OL},
    onelibrary,
    pdb::{
        db::{Db, Item, Node, Tags},
        read,
    },
    settings::DeviceConfig,
    ONE_OPTION, PDB,
};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const KEY: &str = "test-key";

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        Self(fresh_dir(&format!("check-{name}")))
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

/// Four tracks T0 … T3, one cue each, the lists P1 = [T0, T1], P2 = [T2], P3 = [T0, T3]; `n` of them (from the first) are in.
fn snap(r: &Root, n: usize) -> Snapshot {
    let tracks: Vec<Track> = (0..4)
        .map(|i| {
            let f = r.0.join(format!("{i}.wav"));
            if !f.exists() {
                common::click_wav(&f, 120.0, 1.0 + i as f64 * 0.5);
            }
            Track {
                title: format!("T{i}"),
                artist: "Art".into(),
                album: "Alb".into(),
                key: "8A".into(),
                bpm_x100: 12000,
                length_s: 1,
                file: Some(f),
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
    let all = snapshot(&tracks, &lists, |_| {
        Some(Analysis {
            beats: grid(8),
            cues: vec![cue(1, 1000)],
            ..Analysis::default()
        })
    });
    if n == 4 {
        return all;
    }
    common::narrowed(&all, &["p1"])
}

fn export(dev: &Path, s: &Snapshot) {
    common::export(dev, s, &ExportOptions::default(), Some(KEY)).unwrap();
}

fn check_with(dev: &Path, config: &BTreeMap<String, String>) -> CheckReport {
    let report = rekordbox_export::device_check(dev, config, &settings(Some(KEY))).unwrap();
    for p in &report.problems {
        [&p.cause, &p.consequence, &p.action]
            .into_iter()
            .for_each(|t| common::assert_translated(t));
    }
    report
}

fn check(dev: &Path) -> CheckReport {
    check_with(dev, &BTreeMap::new())
}

fn fix(dev: &Path, id: &str) -> ActionAnswer {
    let done =
        with_host(|| rekordbox_export::device_fix(dev, id, &BTreeMap::new(), &settings(Some(KEY))))
            .unwrap();
    common::assert_translated(&done.message);
    done
}

fn problem<'a>(r: &'a CheckReport, what: &str) -> &'a Problem {
    r.problems
        .iter()
        .find(|p| p.id == format!("rekordbox-device:{what}"))
        .unwrap_or_else(|| panic!("no {what} in {:?}", r.problems))
}

fn clean(dev: &Path) {
    let r = check(dev);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}

fn one(dev: &Path) -> Connection {
    let c = Connection::open(dev.join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    c.pragma_update(None, "key", KEY).unwrap();
    c
}

fn one_tracks(dev: &Path) -> i64 {
    one(dev)
        .query_row("select count(*) from content", [], |r| r.get(0))
        .unwrap()
}

fn track(dev: &Path, title: &str) -> Item {
    let (db, _) = read::open(dev, &DeviceConfig::default()).unwrap();
    db.tracks.into_iter().find(|t| t.title == title).unwrap()
}

#[test]
fn a_whole_stick_checks_clean_and_says_what_it_holds() {
    let r = Root::new("whole");
    export(&r.dev(), &snap(&r, 4));
    let report = check(&r.dev());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let names: Vec<&str> = report.libraries.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, [DL, OL]);
    assert_eq!(DL, "Device Library");
    assert_eq!(OL, "OneLibrary");
    for l in &report.libraries {
        assert!(l.present && l.readable && l.modified.is_some(), "{l:?}");
        assert_eq!((l.tracks, l.playlists, l.cues), (4, 3, 4), "{}", l.name);
    }
}

#[test]
fn a_fix_that_cannot_run_says_why_and_changes_nothing() {
    let r = Root::new("cannot");
    export(&r.dev(), &snap(&r, 4));
    let p = r.dev().join("PIONEER/rekordbox/exportLibrary.db");
    std::fs::write(&p, b"not a database").unwrap();
    let before = std::fs::read(r.dev().join(PDB)).unwrap();
    let e = with_host(|| {
        rekordbox_export::device_fix(
            &r.dev(),
            "rebuild_device_library",
            &BTreeMap::new(),
            &settings(Some(KEY)),
        )
    })
    .unwrap_err();
    assert!(
        e.starts_with(
            "The Device Library cannot be rebuilt: the OneLibrary it is built from cannot be read"
        ),
        "{e}"
    );
    common::assert_translated(&e);
    assert_eq!(std::fs::read(r.dev().join(PDB)).unwrap(), before);
    assert_eq!(std::fs::read(&p).unwrap(), b"not a database");
    let e = with_host(|| {
        rekordbox_export::device_fix(&r.dev(), "nonsense", &BTreeMap::new(), &settings(Some(KEY)))
    })
    .unwrap_err();
    assert_eq!(e, "unknown fix nonsense");
    common::assert_translated(&e);
    assert!(commit::pending(&r.dev()).is_none());
}

#[test]
fn a_truncated_device_library_is_unreadable_and_is_rebuilt_from_the_onelibrary() {
    let r = Root::new("truncated");
    export(&r.dev(), &snap(&r, 4));
    let p = r.dev().join(PDB);
    let bytes = std::fs::read(&p).unwrap();
    std::fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
    let report = check(&r.dev());
    assert!(report.libraries[0].present && !report.libraries[0].readable);
    assert!(report.libraries[1].readable);
    let pr = problem(&report, "unreadable_library");
    assert_eq!(pr.severity, Severity::Unreadable);
    assert_eq!(pr.library, DL);
    assert_eq!(pr.fix.as_deref(), Some("rebuild_device_library"));
    assert!(pr.cause.contains("Device Library") && pr.action.contains("OneLibrary"));
    let done = fix(&r.dev(), pr.fix.as_deref().unwrap());
    assert!(
        done.message
            .contains("Rebuilt the Device Library from the OneLibrary: 4 tracks"),
        "{}",
        done.message
    );
    clean(&r.dev());
    assert_eq!(track(&r.dev(), "T2").artist, "Art");
    // the playlists came with it
    let (db, _) = read::open(&r.dev(), &DeviceConfig::default()).unwrap();
    let list = db.nodes.iter().find(|n| n.name == "P3").unwrap();
    assert_eq!(list.tracks.len(), 2);
}

#[test]
fn with_no_other_library_to_build_from_there_is_no_fix() {
    let r = Root::new("nofix");
    export(&r.dev(), &snap(&r, 4));
    std::fs::remove_file(r.dev().join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    let p = r.dev().join(PDB);
    let bytes = std::fs::read(&p).unwrap();
    std::fs::write(&p, &bytes[..bytes.len() / 3]).unwrap();
    let report = check(&r.dev());
    let pr = problem(&report, "unreadable_library");
    assert_eq!(pr.fix, None);
    assert!(
        pr.action.contains("Export to this stick again"),
        "{}",
        pr.action
    );
    assert!(report
        .problems
        .iter()
        .all(|p| p.id != "rekordbox-device:missing_library"));
}

#[test]
fn a_missing_onelibrary_is_a_warning_and_is_built_from_the_device_library() {
    let r = Root::new("noone");
    export(&r.dev(), &snap(&r, 4));
    std::fs::remove_file(r.dev().join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    // Left out on purpose (the device option is off): nothing to report.
    let off = BTreeMap::from([(ONE_OPTION.to_owned(), "0".to_owned())]);
    assert!(check_with(&r.dev(), &off).problems.is_empty());
    let report = check(&r.dev());
    assert!(!report.libraries[1].present);
    let pr = problem(&report, "missing_library");
    assert_eq!((pr.severity, pr.library.as_str()), (Severity::Warning, OL));
    assert_eq!(pr.fix.as_deref(), Some("rebuild_onelibrary"));
    assert!(pr.cause.contains("no OneLibrary") && pr.action.contains("Device Library"));
    fix(&r.dev(), "rebuild_onelibrary");
    clean(&r.dev());
    assert_eq!(one_tracks(&r.dev()), 4);
}

#[test]
fn a_missing_device_library_is_built_from_the_onelibrary() {
    let r = Root::new("nopdb");
    export(&r.dev(), &snap(&r, 4));
    std::fs::remove_file(r.dev().join(PDB)).unwrap();
    let pr = problem(&check(&r.dev()), "missing_library").clone();
    assert_eq!((pr.severity, pr.library.as_str()), (Severity::Warning, DL));
    fix(&r.dev(), pr.fix.as_deref().unwrap());
    clean(&r.dev());
    assert_eq!(track(&r.dev(), "T1").title, "T1");
}

#[test]
fn audio_that_is_listed_but_gone_is_an_error_the_host_has_to_copy_again() {
    let r = Root::new("audio");
    export(&r.dev(), &snap(&r, 4));
    let t = track(&r.dev(), "T1");
    std::fs::remove_file(on_device(&r.dev(), &t.file_path)).unwrap();
    let report = check(&r.dev());
    let pr = problem(&report, "audio_missing");
    assert_eq!(pr.severity, Severity::Error);
    assert_eq!(pr.fix, None);
    assert_eq!(pr.files, [t.file_path.trim_start_matches('/')]);
    assert!(pr.cause.starts_with("1 track(s)"), "{}", pr.cause);
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
}

#[test]
fn an_old_onelibrary_next_to_a_new_device_library_is_rebuilt_from_the_most_complete() {
    let r = Root::new("old-one");
    export(&r.dev(), &snap(&r, 2));
    let old_one = std::fs::read(r.dev().join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    let old_pdb = std::fs::read(r.dev().join(PDB)).unwrap();
    export(&r.dev(), &snap(&r, 4));

    // Another program wrote the Device Library only: the OneLibrary is the old one.
    std::fs::write(r.dev().join("PIONEER/rekordbox/exportLibrary.db"), &old_one).unwrap();
    let report = check(&r.dev());
    let pr = problem(&report, "count_mismatch");
    assert_eq!((pr.severity, pr.library.as_str()), (Severity::Error, OL));
    assert!(
        pr.cause.contains("incomplete (2 tracks instead of 4)"),
        "{}",
        pr.cause
    );
    assert!(
        pr.action
            .contains("from the Device Library, which lists the most tracks"),
        "{}",
        pr.action
    );
    assert_eq!(pr.fix.as_deref(), Some("rebuild_onelibrary"));
    fix(&r.dev(), "rebuild_onelibrary");
    clean(&r.dev());
    assert_eq!(one_tracks(&r.dev()), 4);

    // And the other way round: the OneLibrary is the new one.
    std::fs::write(r.dev().join(PDB), &old_pdb).unwrap();
    let pr = problem(&check(&r.dev()), "count_mismatch").clone();
    assert_eq!(pr.library, DL);
    assert_eq!(pr.fix.as_deref(), Some("rebuild_device_library"));
    fix(&r.dev(), "rebuild_device_library");
    clean(&r.dev());
    let (db, _) = read::open(&r.dev(), &DeviceConfig::default()).unwrap();
    assert_eq!(db.tracks.len(), 4);
}

#[test]
fn leftover_staged_files_without_a_journal_are_info_and_are_cleaned() {
    let r = Root::new("left");
    export(&r.dev(), &snap(&r, 4));
    let t = track(&r.dev(), "T0");
    let anlz = on_device(&r.dev(), &t.analyze_path);
    std::fs::write(r.dev().join("PIONEER/rekordbox/export.pdb.ofnew"), b"half").unwrap();
    std::fs::write(anlz.with_extension("EXT.ofnew"), b"half").unwrap();
    let report = check(&r.dev());
    let pr = problem(&report, "leftover_files");
    assert_eq!(
        (pr.severity, pr.fix.as_deref()),
        (Severity::Info, Some("clean_leftovers"))
    );
    assert!(pr.cause.starts_with("2 unfinished"), "{}", pr.cause);
    assert_eq!(report.problems.len(), 1);
    let done = fix(&r.dev(), "clean_leftovers");
    assert!(done.message.contains("Removed 2"), "{}", done.message);
    clean(&r.dev());
    // The libraries and the audio were not touched.
    assert_eq!(
        read::open(&r.dev(), &DeviceConfig::default())
            .unwrap()
            .0
            .tracks
            .len(),
        4
    );
}

#[test]
fn a_pending_change_is_the_hosts_to_finish_not_a_problem() {
    let r = Root::new("pending");
    export(&r.dev(), &snap(&r, 2));
    let full = snap(&r, 4);
    let err = with_host(|| {
        commit::fail_at(Some(0));
        let out = rekordbox_export::export(
            &r.dev(),
            &full,
            &ExportOptions::default(),
            &settings(Some(KEY)),
        );
        commit::fail_at(None);
        out
    })
    .unwrap_err();
    assert!(err.contains("injected"));
    assert!(commit::pending(&r.dev()).is_some());
    let report = check(&r.dev());
    assert!(
        report
            .problems
            .iter()
            .all(|p| p.id != "rekordbox-device:leftover_files"),
        "{:?}",
        report.problems
    );
    commit::recover(&r.dev()).unwrap();
    clean(&r.dev());
}

#[test]
fn analysis_files_that_are_missing_or_damaged_are_written_again() {
    let r = Root::new("analysis");
    export(&r.dev(), &snap(&r, 4));
    let (t1, t2, t3) = (
        track(&r.dev(), "T1"),
        track(&r.dev(), "T2"),
        track(&r.dev(), "T3"),
    );
    let dat = |t: &Item| on_device(&r.dev(), &t.analyze_path);
    std::fs::remove_file(dat(&t1).with_extension("EXT")).unwrap();
    std::fs::write(dat(&t2), b"PMAI junk").unwrap();
    // T3 loses its analysis and its audio: it cannot be analysed again.
    std::fs::remove_file(dat(&t3)).unwrap();
    std::fs::remove_file(on_device(&r.dev(), &t3.file_path)).unwrap();
    let report = check(&r.dev());
    let pr = problem(&report, "analysis_missing");
    assert_eq!(
        (pr.severity, pr.fix.as_deref()),
        (Severity::Warning, Some("rewrite_analysis"))
    );
    assert!(pr.cause.starts_with("3 track(s)"), "{}", pr.cause);
    let done = fix(&r.dev(), "rewrite_analysis");
    assert!(
        done.message
            .contains("Rewrote the analysis files of 2 track(s); 1 could not be rebuilt"),
        "{}",
        done.message
    );
    assert!(dat(&t1).with_extension("EXT").is_file() && dat(&t2).is_file());
    // What T1's own .DAT held (its cue) survived, and the files carry the same track path as before.
    let back = rekordbox_common::anlz::load_track(&dat(&t1)).unwrap();
    assert_eq!(back.cues.len(), 1);
    assert_eq!(back.path.as_deref(), Some(t1.file_path.as_str()));
    let left = check(&r.dev());
    assert_eq!(
        left.problems
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        [
            "rekordbox-device:audio_missing",
            "rekordbox-device:analysis_missing"
        ]
    );
    assert_eq!(
        problem(&left, "analysis_missing").cause,
        "1 track(s) have no readable analysis files (for example T3)."
    );
}

/// A 1000-track stick written straight from the models (a real export of 1000 audio files would take minutes).
fn big_stick(dev: &Path, n: u32) {
    let mut db = Db {
        analyze_date: "2026-01-01".into(),
        ..Db::default()
    };
    let files: Vec<(String, String)> = (1..=n)
        .map(|i| {
            (
                format!("Contents/Artist {}/Album {}/Track {i}.mp3", i % 50, i % 7),
                format!("PIONEER/USBANLZ/P{:03X}/{i:08X}/ANLZ0000.DAT", i % 0x1000),
            )
        })
        .collect();
    for (i, (audio, anlz)) in files.iter().enumerate() {
        let id = i as u32 + 1;
        db.tracks.push(Item {
            id,
            title: format!("Track {id}"),
            artist: format!("Artist {}", id % 50),
            album: format!("Album {}", id % 7),
            file_path: format!("/{audio}"),
            filename: format!("Track {id}.mp3"),
            analyze_path: format!("/{anlz}"),
            file_size: 16,
            file_type: 1,
            ..Item::default()
        });
        std::fs::create_dir_all(dev.join(audio).parent().unwrap()).unwrap();
        std::fs::write(dev.join(audio), [0u8; 16]).unwrap();
        let a = Analysis {
            beats: grid(600),
            cues: vec![cue(1, 1000), cue(2, 5000)],
            ..Analysis::default()
        };
        let f = rekordbox_export::anlz::encode(&a, &format!("/{audio}"));
        let dat = dev.join(anlz);
        std::fs::create_dir_all(dat.parent().unwrap()).unwrap();
        std::fs::write(&dat, f.dat).unwrap();
        std::fs::write(dat.with_extension("EXT"), f.ext).unwrap();
        std::fs::write(dat.with_extension("2EX"), f.ex2).unwrap();
    }
    db.nodes.push(Node {
        id: 1,
        parent: 0,
        name: "All".into(),
        is_folder: false,
        tracks: db.tracks.iter().map(|t| t.id).collect(),
    });
    let dir = dev.join("PIONEER/rekordbox");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("export.pdb"), db.encode().unwrap()).unwrap();
    onelibrary::write(&dir.join("exportLibrary.db"), KEY, &db, &Tags::default(), 1).unwrap();
}

#[test]
fn checking_a_thousand_track_stick_is_fast() {
    let r = Root::new("big");
    big_stick(&r.dev(), 1000);
    let t = std::time::Instant::now();
    let report = check(&r.dev());
    let took = t.elapsed();
    eprintln!("device_check of 1000 tracks: {took:?}");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(
        report
            .libraries
            .iter()
            .map(|l| (l.tracks, l.cues))
            .collect::<Vec<_>>(),
        [(1000, 2000), (1000, 2000)]
    );
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
}
