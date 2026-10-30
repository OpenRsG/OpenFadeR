//! `device_check` and `device_fix` on synthetic sticks in temp dirs: what they find, that they write nothing, what the fix repairs.

mod common;

use common::{assert_translated, snapshot, tree, with_host, Dir};
use openfader_plugin::library::{commit, CheckReport, Feature, LibraryPlugin, Severity};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use compat_virtualdj::{Plugin, DATABASE, LIBRARY};

/// A stick with one export on it (two songs, two lists, four cues on the first song).
fn stick(tag: &str) -> (Dir, Dir) {
    let (src, stick) = (
        Dir::new(&format!("{tag}-src")),
        Dir::new(&format!("{tag}-stick")),
    );
    with_host(|| {
        Plugin::export(&stick.0, &snapshot(&src), &Default::default(), &json!({})).unwrap()
    });
    (src, stick)
}

/// The check, which must not write, with every text it answers translated.
fn check(root: &Path) -> CheckReport {
    let before = tree(root);
    let c = Plugin::device_check(root, &BTreeMap::new(), &json!({})).unwrap();
    assert_eq!(tree(root), before, "device_check wrote something");
    for p in &c.problems {
        assert!(p.id.starts_with("virtualdj:"), "{}", p.id);
        for t in [&p.cause, &p.consequence, &p.action] {
            assert_translated(t);
        }
        if let Some(f) = &p.fix {
            assert!(f.starts_with("virtualdj:"), "{f}");
        }
    }
    c
}

fn ids(c: &CheckReport) -> Vec<&str> {
    c.problems.iter().map(|p| p.id.as_str()).collect()
}

#[test]
fn a_whole_stick_has_no_problem_and_the_counts_are_read() {
    let (_src, stick) = stick("ok");
    let c = check(&stick.0);
    let l = &c.libraries[0];
    assert_eq!(l.name, LIBRARY);
    assert!(l.present && l.readable && l.modified.is_some());
    assert_eq!((l.tracks, l.playlists, l.cues), (2, 2, 4));
    assert!(c.problems.is_empty(), "{c:?}");
    let info = Plugin::device_probe(&stick.0, &json!({})).unwrap();
    assert!(info.features.contains(&Feature::Plan) && info.features.contains(&Feature::Check));
    // No stick content at all: the library is simply absent.
    let empty = Dir::new("ok-empty");
    let c = check(&empty.0);
    assert!(!c.libraries[0].present && c.problems.is_empty());
}

#[test]
fn missing_audio_is_an_error_naming_the_files() {
    let (_src, stick) = stick("missing");
    let gone = tree(&stick.0)
        .into_keys()
        .find(|k| k.ends_with("/b.wav"))
        .unwrap();
    std::fs::remove_file(stick.join(&gone)).unwrap();
    let c = check(&stick.0);
    let p = c
        .problems
        .iter()
        .find(|p| p.id == "virtualdj:missing_audio")
        .unwrap();
    assert_eq!(p.severity, Severity::Error);
    assert_eq!(p.files, [gone]);
    assert!(p.fix.is_none());
    assert!(c.libraries[0].readable);
}

#[test]
fn an_unreadable_database_is_reported_and_has_no_plugin_fix() {
    let (_src, stick) = stick("bad");
    std::fs::write(
        stick.join(DATABASE),
        "<VirtualDJ_Database><Song FilePath=\"x\"",
    )
    .unwrap();
    let c = check(&stick.0);
    let l = &c.libraries[0];
    assert!(l.present && !l.readable);
    let p = &c.problems[0];
    assert_eq!(
        (p.id.as_str(), p.severity),
        ("virtualdj:unreadable", Severity::Unreadable)
    );
    assert!(p.fix.is_none() && p.files.is_empty());
}

#[test]
fn leftovers_are_cleaned_by_the_fix_and_nothing_else_changes() {
    let (_src, stick) = stick("left");
    std::fs::write(stick.join("VirtualDJ/database.xml.ofnew"), "x").unwrap();
    std::fs::write(
        stick.join("VirtualDJ/MyLists/Root_List.vdjfolder.ofnew"),
        "y",
    )
    .unwrap();
    let c = check(&stick.0);
    let p = c
        .problems
        .iter()
        .find(|p| p.id == "virtualdj:leftovers")
        .unwrap();
    assert_eq!(p.severity, Severity::Info);
    assert_eq!(p.fix.as_deref(), Some("virtualdj:clean_leftovers"));
    let db = std::fs::read(stick.join(DATABASE)).unwrap();
    let a = Plugin::device_fix(
        &stick.0,
        "virtualdj:clean_leftovers",
        &BTreeMap::new(),
        &json!({}),
    )
    .unwrap();
    assert_translated(&a.message);
    assert!(a.message.contains('2'), "{}", a.message);
    assert_eq!(std::fs::read(stick.join(DATABASE)).unwrap(), db);
    assert!(check(&stick.0).problems.is_empty());
    assert!(Plugin::device_fix(&stick.0, "virtualdj:nope", &BTreeMap::new(), &json!({})).is_err());
}

#[test]
fn a_pending_change_is_reported_and_blocks_the_cleanup_until_it_is_recovered() {
    let (src, stick) = stick("pending");
    let mut s = snapshot(&src);
    s.tracks
        .push((common::track(&src, "c.wav", "Third", 900), None));
    commit::fail_at(Some(1));
    let r = with_host(|| Plugin::export(&stick.0, &s, &Default::default(), &json!({})));
    commit::fail_at(None);
    assert!(r.is_err());
    let c = check(&stick.0);
    let p = c
        .problems
        .iter()
        .find(|p| p.id == "virtualdj:pending")
        .unwrap();
    assert_eq!(p.severity, Severity::Warning);
    assert!(!ids(&c).contains(&"virtualdj:leftovers"));
    let e = Plugin::device_fix(
        &stick.0,
        "virtualdj:clean_leftovers",
        &BTreeMap::new(),
        &json!({}),
    )
    .unwrap_err();
    assert_translated(&e);
    assert!(
        tree(&stick.0).keys().any(|k| k.ends_with(".ofnew")),
        "the fix must not remove staged files of a switching change"
    );
    commit::recover(&stick.0).unwrap();
    let c = check(&stick.0);
    assert!(c.problems.is_empty(), "{c:?}");
    assert_eq!(c.libraries[0].tracks, 3);
}
