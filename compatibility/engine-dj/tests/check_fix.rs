//! `device_check` and `device_fix` on synthetic sticks in temp dirs: what the check finds, that it writes nothing, and that the
//! fixes repair what they offer.

mod common;

use common::{assert_translated, read_counts, setup, tree, with_host, Dirs};
use compat_engine_dj::database;
use openfader_plugin::library::{
    commit, CheckReport, ExportOptions, LibraryPlugin, Problem, Severity,
};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// `device_check` under the fake host: it must not write, and every text it answers must be translated.
fn check(root: &Path) -> CheckReport {
    let before = tree(root);
    let r = with_host(|| compat_engine_dj::Plugin::device_check(root, &BTreeMap::new(), &Value::Null))
        .unwrap();
    assert_eq!(before, tree(root), "the check wrote something");
    for p in &r.problems {
        for t in [&p.cause, &p.consequence, &p.action] {
            assert_translated(t);
        }
        assert!(p.id.starts_with("engine-dj:"), "{}", p.id);
        assert!(p.fix.as_deref().is_none_or(|f| f.starts_with("engine-dj:")));
    }
    assert_eq!(r.libraries.len(), 1);
    assert_eq!(r.libraries[0].name, "Engine Library");
    r
}

fn fix(root: &Path, id: &str) -> String {
    let a = with_host(|| compat_engine_dj::Plugin::device_fix(root, id, &BTreeMap::new(), &Value::Null))
        .unwrap();
    assert_translated(&a.message);
    a.message
}

fn find<'a>(r: &'a CheckReport, id: &str) -> &'a Problem {
    r.problems
        .iter()
        .find(|p| p.id == format!("engine-dj:{id}"))
        .unwrap_or_else(|| panic!("no {id} in {:?}", r.problems))
}

/// A stick holding the library of `setup`: tracks A and B.
fn stick(name: &str) -> Dirs {
    let d = Dirs::new(name);
    common::export(&d, &setup(&d)).unwrap();
    d
}

fn older(p: &Path, secs: u64) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

#[test]
fn a_good_stick_has_no_problem_and_its_counts() {
    let d = stick("good");
    let r = check(&d.stick());
    let l = &r.libraries[0];
    assert!(l.present && l.readable && l.modified.is_some());
    let (tracks, lists, cues) = read_counts(&d.stick());
    assert_eq!((l.tracks, l.playlists, l.cues), (tracks, lists, cues));
    assert_eq!(
        (tracks, lists),
        (2, 2),
        "A and B, the lists Warm and Top (Sets is a folder)"
    );
    assert!(cues > 0);
    assert!(r.problems.is_empty(), "{:?}", r.problems);

    // A stick with nothing on it, and one that is not there.
    let empty = Dirs::new("empty");
    let r = check(&empty.stick());
    assert!(!r.libraries[0].present && r.problems.is_empty());
    let r = check(&empty.stick().with_file_name("missing"));
    assert!(!r.libraries[0].present && r.problems.is_empty());
}

#[test]
fn audio_the_library_lists_but_the_stick_lacks_is_an_error_with_its_paths() {
    let d = stick("missing");
    let gone = tree(&d.stick())
        .into_keys()
        .find(|k| k.starts_with("Contents/") && k.ends_with("a.wav"))
        .unwrap();
    std::fs::remove_file(d.stick().join(&gone)).unwrap();
    let r = check(&d.stick());
    let p = find(&r, "missing_audio");
    assert_eq!(p.severity, Severity::Error);
    assert_eq!(p.files, [gone]);
    assert!(p.fix.is_none(), "the host copies the audio again");
    assert!(r.libraries[0].readable);
    assert_eq!(r.problems.len(), 1);
}

#[test]
fn a_library_that_cannot_be_read_is_unreadable() {
    // Not a database.
    let d = stick("garbage");
    std::fs::write(
        database(&d.stick()),
        b"This is some text that is no database, at all, really.",
    )
    .unwrap();
    let r = check(&d.stick());
    assert_eq!(find(&r, "unreadable").severity, Severity::Unreadable);
    assert!(r.libraries[0].present && !r.libraries[0].readable);

    // A database that is no Engine library.
    let d = stick("other");
    std::fs::remove_file(database(&d.stick())).unwrap();
    Connection::open(database(&d.stick()))
        .unwrap()
        .execute_batch("CREATE TABLE Other (id INTEGER)")
        .unwrap();
    assert_eq!(
        find(&check(&d.stick()), "unreadable").severity,
        Severity::Unreadable
    );

    // A schema version this plugin does not know.
    let d = stick("version");
    Connection::open(database(&d.stick()))
        .unwrap()
        .execute("UPDATE Information SET schemaVersionMajor = 9", [])
        .unwrap();
    let r = check(&d.stick());
    assert_eq!(find(&r, "unreadable").severity, Severity::Unreadable);
    assert!(
        r.problems[0].cause.contains("9.0.0"),
        "{}",
        r.problems[0].cause
    );

    // The old two-file layout.
    let d = Dirs::new("layout");
    std::fs::create_dir_all(d.stick().join("Engine Library")).unwrap();
    std::fs::write(d.stick().join("Engine Library").join("m.db"), b"x").unwrap();
    let r = check(&d.stick());
    assert_eq!(find(&r, "old_layout").severity, Severity::Unreadable);
    assert!(r.libraries[0].present && !r.libraries[0].readable);
}

#[test]
fn a_damaged_database_is_an_error_or_unreadable() {
    let d = stick("damaged");
    let db = database(&d.stick());
    let mut bytes = std::fs::read(&db).unwrap();
    assert!(bytes.len() > 40_000, "{}", bytes.len());
    // The pages after the schema are tables and indexes: overwrite a few.
    for b in &mut bytes[8192..20_480] {
        *b = 0xff;
    }
    std::fs::write(&db, bytes).unwrap();
    let r = check(&d.stick());
    assert!(
        r.problems.iter().any(|p| p.severity >= Severity::Error),
        "{:?}",
        r.problems
    );
}

#[test]
fn a_leftover_staged_copy_is_info_and_the_fix_removes_it() {
    let d = stick("leftover");
    let left = commit::Commit::staged_path(&database(&d.stick()));
    std::fs::write(&left, b"half of a database").unwrap();
    let r = check(&d.stick());
    let p = find(&r, "leftover");
    assert_eq!(p.severity, Severity::Info);
    assert_eq!(p.fix.as_deref(), Some("engine-dj:clean_leftovers"));
    let library = std::fs::read(database(&d.stick())).unwrap();
    fix(&d.stick(), "engine-dj:clean_leftovers");
    assert!(!left.exists());
    assert_eq!(
        std::fs::read(database(&d.stick())).unwrap(),
        library,
        "the library is not touched"
    );
    assert!(check(&d.stick()).problems.is_empty());
}

#[test]
fn a_pending_change_is_a_warning_and_the_fix_finishes_it() {
    let d = stick("pending");
    let next = {
        let mut s = setup(&d);
        s.tracks
            .push((common::track(&d, "d.wav", "Delta", "Other", 3), None));
        s
    };
    let out = with_host(|| {
        commit::fail_at(Some(0));
        let out =
            compat_engine_dj::Plugin::export(&d.stick(), &next, &ExportOptions::default(), &Value::Null);
        commit::fail_at(None);
        out
    });
    assert!(out.unwrap_err().contains("injected"));
    let r = check(&d.stick()); // the check wrote nothing: the journal is still there
    assert!(commit::pending(&d.stick()).is_some());
    let p = find(&r, "change_pending");
    assert_eq!(p.severity, Severity::Warning);
    assert_eq!(p.fix.as_deref(), Some("engine-dj:finish_change"));
    assert!(
        r.problems.iter().all(|p| p.id != "engine-dj:leftover"),
        "the staged file belongs to the change: {:?}",
        r.problems
    );
    assert_eq!(
        r.libraries[0].tracks, 2,
        "the old generation is in use until the change is finished"
    );
    let m = fix(&d.stick(), "engine-dj:finish_change");
    assert!(m.contains("completed"), "{m}");
    assert!(commit::pending(&d.stick()).is_none());
    let r = check(&d.stick());
    assert_eq!(r.libraries[0].tracks, 3);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    // Nothing is pending any more.
    assert!(fix(&d.stick(), "engine-dj:finish_change").contains("No interrupted export"));
}

#[test]
fn journal_files_older_than_the_library_are_a_warning_and_the_fix_removes_only_those() {
    let d = stick("sidecars");
    let db = database(&d.stick());
    let (wal, shm) = (
        compat_engine_dj::schema::sidecar(&db, "-wal"),
        compat_engine_dj::schema::sidecar(&db, "-shm"),
    );
    std::fs::write(&wal, b"old journal bytes").unwrap();
    std::fs::write(&shm, b"old index bytes").unwrap();
    older(&wal, 3600);
    // The shared-memory file is as new as the library: a player at work, not a leftover.
    let r = check(&d.stick());
    let p = find(&r, "stale_sidecar");
    assert_eq!(p.severity, Severity::Warning);
    assert_eq!(p.fix.as_deref(), Some("engine-dj:remove_sidecars"));
    assert!(
        p.cause.contains("m.db-wal") && !p.cause.contains("m.db-shm"),
        "{}",
        p.cause
    );
    // The library itself still reads: the check opened it without replaying the journal file.
    assert_eq!(r.libraries[0].tracks, 2);
    let library = std::fs::read(&db).unwrap();
    fix(&d.stick(), "engine-dj:remove_sidecars");
    assert!(!wal.exists() && shm.exists());
    assert_eq!(std::fs::read(&db).unwrap(), library);
    assert!(check(&d.stick()).problems.is_empty());
}

#[test]
fn a_fix_the_plugin_does_not_have_is_refused() {
    let d = stick("nofix");
    let e = with_host(|| {
        compat_engine_dj::Plugin::device_fix(
            &d.stick(),
            "engine-dj:nothing",
            &BTreeMap::new(),
            &Value::Null,
        )
    })
    .unwrap_err();
    assert_translated(&e);
    // A leftover next to a pending journal is not removed.
    std::fs::create_dir_all(d.stick().join(".openfader")).unwrap();
    std::fs::write(
        d.stick().join(commit::JOURNAL),
        r#"{"owner":"engine-dj","state":"staging","files":[],"started":0}"#,
    )
    .unwrap();
    let e = with_host(|| {
        compat_engine_dj::Plugin::device_fix(
            &d.stick(),
            "engine-dj:clean_leftovers",
            &BTreeMap::new(),
            &Value::Null,
        )
    })
    .unwrap_err();
    assert_translated(&e);
}
