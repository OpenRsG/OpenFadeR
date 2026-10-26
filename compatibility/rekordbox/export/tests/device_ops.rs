//! The device ops on synthetic libraries in temp dirs (never a real stick): incremental export, refresh, cancel / failure cleanup,
//! free-space and list limits, OneLibrary carry-over, delete, convert, Hot Cue Bank Lists, backup listing, Update Collection and
//! history deletion. Everything is read back through the writer's own readers and SQL.

mod common;

use common::{cue, export, fresh_dir, grid, narrowed, settings, snapshot, with_host};
use openfader_plugin::library::{
    Analysis, Bank, BankCue, ExportOptions, Feature, Playlist, RemoveTarget, Snapshot, Track,
};
use compat_rekordbox_common::on_device;
use compat_rekordbox_export::{
    anlz, backup_tracks, device_action, device_probe,
    pdb::{
        db::{Db, History},
        read,
    },
    remove,
    settings::DeviceConfig,
    update,
};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const KEY: &str = "test-key";

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let r = fresh_dir(&format!("device-{name}"));
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

/// Four tracks (a.wav … d.wav, distinct lengths so their sizes differ) and the lists P1 = [a, b], P2 = [c], P3 = [a, d], each
/// track with the grid and cue a library gives.
fn setup(r: &Root) -> Snapshot {
    let tracks: Vec<Track> = (0..4)
        .map(|i| {
            let f = r.src().join(format!("{}.wav", ["a", "b", "c", "d"][i]));
            common::click_wav(&f, 120.0, 1.0 + i as f64 * 0.5);
            Track {
                title: format!("T{i}"),
                artist: "Art".into(),
                album: if i == 0 { "Alb".into() } else { String::new() },
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
    snapshot(&tracks, &lists, |_| {
        Some(Analysis {
            beats: grid(8),
            cues: vec![cue(1, 1000)],
            ..Analysis::default()
        })
    })
}

fn db(root: &Path) -> Db {
    read::open(root, &DeviceConfig::default()).unwrap().0
}

/// Writes `d` back as the device's `export.pdb`, the way a player that logged something would.
fn write_db(root: &Path, d: &Db) {
    std::fs::write(
        root.join("PIONEER/rekordbox/export.pdb"),
        d.encode().unwrap(),
    )
    .unwrap();
}

/// Titles of every track of the device library.
fn titles(root: &Path) -> Vec<String> {
    db(root).tracks.into_iter().map(|t| t.title).collect()
}

fn audio_files(root: &Path) -> usize {
    fn walk(d: &Path) -> usize {
        std::fs::read_dir(d).map_or(0, |r| {
            r.flatten()
                .map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
    }
    walk(&root.join("Contents"))
}

fn exp(root: &Path, snap: &Snapshot) -> openfader_plugin::library::ExportReport {
    export(root, snap, &ExportOptions::default(), Some(KEY)).unwrap()
}

fn one(root: &Path) -> Connection {
    let c = Connection::open(root.join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    c.pragma_update(None, "key", KEY).unwrap();
    c
}

fn names(c: &Connection, sql: &str) -> Vec<String> {
    c.prepare(sql)
        .unwrap()
        .query_map([], |x| x.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn remove_op(
    root: &Path,
    target: &RemoveTarget,
    delete_tracks: bool,
) -> Result<openfader_plugin::library::RemoveReport, String> {
    remove(
        root,
        target,
        delete_tracks,
        &BTreeMap::new(),
        &settings(Some(KEY)),
    )
}

fn playlist(name: &str) -> RemoveTarget {
    RemoveTarget::Playlist {
        path: vec![name.into()],
    }
}

#[test]
fn export_adds_to_an_existing_library_and_keeps_ids_and_histories() {
    let r = Root::new("add");
    let full = setup(&r);
    let first = exp(&r.dev(), &narrowed(&full, &["p1"]));
    assert_eq!((first.added, first.updated, first.tracks), (2, 0, 2));
    let ids_before: Vec<(String, u32)> = db(&r.dev())
        .tracks
        .iter()
        .map(|t| (t.title.clone(), t.id))
        .collect();

    // A player logged a session in the meantime.
    let mut d = db(&r.dev());
    d.histories.push(History {
        id: 7,
        name: "HISTORY 2026-01-02".into(),
        tracks: vec![d.tracks[1].id, d.tracks[0].id],
    });
    write_db(&r.dev(), &d);

    let second = exp(&r.dev(), &narrowed(&full, &["p2", "p3"]));
    assert_eq!(
        (second.added, second.updated),
        (2, 1),
        "c and d are new, a is on the device already"
    );
    let after = db(&r.dev());
    for (title, id) in &ids_before {
        assert_eq!(
            after.tracks.iter().find(|t| &t.title == title).unwrap().id,
            *id,
            "{title} keeps its id"
        );
    }
    assert_eq!(audio_files(&r.dev()), 4, "a was not copied again");
    assert_eq!(titles(&r.dev()), ["T0", "T1", "T2", "T3"]);

    let nodes: Vec<_> = after
        .nodes
        .iter()
        .map(|p| (p.name.as_str(), p.is_folder))
        .collect();
    assert_eq!(nodes, [("P1", false), ("P2", false), ("P3", false)]);
    let hist = &after.histories[0];
    assert_eq!(hist.name, "HISTORY 2026-01-02");
    let by_id = |id: &u32| {
        after
            .tracks
            .iter()
            .find(|t| t.id == *id)
            .unwrap()
            .title
            .as_str()
    };
    assert_eq!(
        hist.tracks.iter().map(by_id).collect::<Vec<_>>(),
        ["T1", "T0"],
        "the history survived the rewrite"
    );
    let p3 = after.nodes.iter().find(|p| p.name == "P3").unwrap();
    assert_eq!(
        p3.tracks.iter().map(by_id).collect::<Vec<_>>(),
        ["T0", "T3"]
    );

    // The same export again changes nothing but refreshes every track; it adds no file.
    let third = exp(&r.dev(), &full);
    assert_eq!((third.added, third.updated), (0, 4));
    assert_eq!(audio_files(&r.dev()), 4);
    assert_eq!(db(&r.dev()).nodes.len(), 3);
}

#[test]
fn a_refresh_carries_new_ratings_and_comments_to_the_device_track() {
    let r = Root::new("refresh");
    let mut full = setup(&r);
    exp(&r.dev(), &narrowed(&full, &["p1"]));
    full.tracks[0].0.rating = 5;
    full.tracks[0].0.comment = "new".into();
    let rep = exp(&r.dev(), &narrowed(&full, &["p1"]));
    assert_eq!((rep.added, rep.updated), (0, 2));
    let d = db(&r.dev());
    let a = d.tracks.iter().find(|t| t.title == "T0").unwrap();
    assert_eq!((a.rating, a.comment.as_str()), (5, "new"));
}

#[test]
fn a_failure_mid_export_removes_what_it_copied() {
    let r = Root::new("fail");
    let mut full = setup(&r);
    exp(&r.dev(), &narrowed(&full, &["p1"]));
    let before = std::fs::read(r.dev().join("PIONEER/rekordbox/export.pdb")).unwrap();
    // Track c is fine, track d has a "cover" that is a folder: reading it fails after c was copied.
    let cover = r.src().join("cover.jpg");
    std::fs::create_dir(&cover).unwrap();
    full.tracks[3].0.artwork = Some(cover);
    let err = export(
        &r.dev(),
        &narrowed(&full, &["p2", "p3"]),
        &ExportOptions::default(),
        None,
    )
    .unwrap_err();
    assert!(err.contains("cover.jpg"), "{err}");
    assert_eq!(audio_files(&r.dev()), 2, "c's copy is gone again");
    assert_eq!(
        std::fs::read(r.dev().join("PIONEER/rekordbox/export.pdb")).unwrap(),
        before
    );
}

#[test]
fn cancel_stops_before_anything_is_written() {
    let r = Root::new("cancel");
    let full = setup(&r);
    let err = with_host(|| {
        common::cancel();
        compat_rekordbox_export::export(&r.dev(), &full, &ExportOptions::default(), &settings(None))
    })
    .unwrap_err();
    assert_eq!(err, "cancelled");
    assert!(!r.dev().join("PIONEER/rekordbox/export.pdb").exists() && audio_files(&r.dev()) == 0);
}

#[test]
fn space_and_list_limits_are_checked_up_front() {
    let r = Root::new("limits");
    let full = setup(&r);
    let tight = ExportOptions {
        free: Some(1000),
        ..ExportOptions::default()
    };
    let e = export(&r.dev(), &full, &tight, None).unwrap_err();
    assert!(e.contains("Not enough space"), "{e}");
    assert_eq!(audio_files(&r.dev()), 0);

    let many = Snapshot {
        playlists: (0..1001)
            .map(|i| Playlist {
                id: format!("p{i}"),
                name: format!("L{i}"),
                depth: 0,
                is_folder: false,
            })
            .collect(),
        members: vec![Vec::new(); 1001],
        ..Snapshot::default()
    };
    let e = export(&r.dev(), &many, &ExportOptions::default(), None).unwrap_err();
    assert!(e.contains("cannot exceed 1000"), "{e}");
}

#[test]
fn onelibrary_update_keeps_what_players_wrote() {
    let r = Root::new("one");
    let full = setup(&r);
    let first = exp(&r.dev(), &narrowed(&full, &["p1"]));
    assert!(first.notes.is_empty(), "{:?}", first.notes);
    let id_before: i64 = one(&r.dev())
        .query_row("select myTagMasterDBID from property", [], |x| x.get(0))
        .unwrap();
    {
        let c = one(&r.dev());
        c.execute("insert into history values (1, 0, 'HISTORY 001', 0, 0)", [])
            .unwrap();
        c.execute(
            "insert into history_content values (1, 1, 1), (1, 2, 2)",
            [],
        )
        .unwrap();
        c.execute(
            "insert into cue(cue_id, content_id, kind, inUsec) values (5, 1, 1, 1000000)",
            [],
        )
        .unwrap();
    }
    exp(&r.dev(), &narrowed(&full, &["p2"]));
    let c = one(&r.dev());
    let count = |t: &str| {
        c.query_row(&format!("select count(*) from {t}"), [], |x| {
            x.get::<_, i64>(0)
        })
        .unwrap()
    };
    assert_eq!(
        (
            count("content"),
            count("playlist"),
            count("history"),
            count("history_content"),
            count("cue")
        ),
        (3, 2, 1, 2, 1)
    );
    assert_eq!(
        c.query_row("select myTagMasterDBID from property", [], |x| x
            .get::<_, i64>(0))
            .unwrap(),
        id_before
    );
    assert_eq!(
        names(&c, "select name from playlist order by playlist_id"),
        ["P1", "P2"]
    );
    assert_eq!(names(&c, "select name from history"), ["HISTORY 001"]);
}

#[test]
fn deleting_a_playlist_deletes_only_tracks_no_other_playlist_lists() {
    let r = Root::new("delete");
    let full = setup(&r);
    exp(&r.dev(), &full);

    let kept = remove_op(&r.dev(), &playlist("P3"), false).unwrap();
    assert_eq!((kept.tracks, kept.playlists), (0, 1));
    assert_eq!(titles(&r.dev()).len(), 4);

    // P1 = [a, b]; a is in no other playlist now (P3 is gone), b neither: both go with the files.
    let gone = remove_op(&r.dev(), &playlist("P1"), true).unwrap();
    assert_eq!((gone.tracks, gone.playlists), (2, 1), "{:?}", gone.warnings);
    assert_eq!(titles(&r.dev()), ["T2", "T3"]);
    assert_eq!(audio_files(&r.dev()), 2);
    let anlz_dirs = std::fs::read_dir(r.dev().join("PIONEER/USBANLZ"))
        .unwrap()
        .flatten()
        .flat_map(|p| std::fs::read_dir(p.path()).unwrap().flatten())
        .count();
    assert_eq!(anlz_dirs, 2, "their analysis folders went too");
    assert!(
        !r.dev().join("Contents/Art/Alb").exists(),
        "the empty album folder went"
    );

    let t3 = db(&r.dev())
        .tracks
        .into_iter()
        .find(|t| t.title == "T3")
        .unwrap();
    let file = on_device(&r.dev(), &t3.file_path);
    let one = remove_op(&r.dev(), &RemoveTarget::Track { file }, false).unwrap();
    assert_eq!(one.tracks, 1);
    assert_eq!(titles(&r.dev()), ["T2"]);
    assert!(remove_op(&r.dev(), &playlist("nope"), false).is_err());
}

#[test]
fn convert_builds_the_onelibrary_from_the_device_library() {
    let r = Root::new("convert");
    let full = setup(&r);
    exp(&r.dev(), &full);
    std::fs::remove_file(r.dev().join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    let mut d = db(&r.dev());
    d.histories.push(History {
        id: 3,
        name: "HISTORY 001".into(),
        tracks: vec![d.tracks[0].id],
    });
    write_db(&r.dev(), &d);

    let info = device_probe(&r.dev(), &Value::Null).unwrap();
    assert!(info.can_write && info.features.contains(&Feature::Update));
    assert_eq!(
        (info.actions[0].id.as_str(), info.actions[0].enabled),
        ("convert", true)
    );
    assert_eq!(info.backup, ["PIONEER"]);
    assert!(info.modified.is_some());
    assert_eq!(info.prefetch, ["PIONEER/rekordbox/export.pdb"]);

    let act = |confirmed| {
        device_action(
            &r.dev(),
            "convert",
            confirmed,
            &BTreeMap::new(),
            &settings(Some(KEY)),
        )
        .unwrap()
    };
    let done = act(false);
    assert!(
        done.confirm.is_none() && done.message.contains("4 track(s) and 3 playlist(s)"),
        "{done:?}"
    );
    let c = one(&r.dev());
    assert_eq!(
        c.query_row("select count(*) from content", [], |x| x.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        names(&c, "select name from playlist order by playlist_id"),
        ["P1", "P2", "P3"]
    );
    assert_eq!(names(&c, "select name from history"), ["HISTORY 001"]);
    drop(c);

    assert_eq!(
        device_probe(&r.dev(), &Value::Null).unwrap().libraries,
        ["Device Library", "OneLibrary"]
    );
    let ask = act(false);
    assert!(
        ask.confirm
            .as_ref()
            .unwrap()
            .text
            .contains("OneLibrary already exists"),
        "{ask:?}"
    );
    assert!(act(true).confirm.is_none());

    assert!(device_action(&r.dev(), "bogus", true, &BTreeMap::new(), &settings(None)).is_err());
}

#[test]
fn update_imports_only_what_the_player_changed() {
    let r = Root::new("collection");
    let mut full = setup(&r);
    let rep = exp(&r.dev(), &narrowed(&full, &["p1"]));
    let mut baselines: BTreeMap<String, i64> = rep.baselines.iter().cloned().collect();
    assert_eq!(baselines.len(), 2);
    let a_key = openfader_plugin::library::track_key(&full.tracks[0].0);
    let b_key = openfader_plugin::library::track_key(&full.tracks[1].0);
    let upd = |s: &Snapshot, b: &BTreeMap<String, i64>| {
        with_host(|| update(&r.dev(), s, b, &Value::Null)).unwrap()
    };

    // Untouched device: nothing to import, even though the collection changed after the export.
    full.tracks[0].0.rating = 2;
    let u = upd(&full, &baselines);
    assert_eq!((u.matched, u.edits.len(), u.unmatched), (2, 0, 0));
    assert!(u.baselines.is_empty());

    // The player rates and recolours a, comments it, and a hot cue of b moves.
    let mut d = db(&r.dev());
    let b = d.tracks[1].clone();
    {
        let t = &mut d.tracks[0];
        (t.rating, t.color, t.comment) = (4, 3, "from the player".into());
    }
    write_db(&r.dev(), &d);
    let moved = Analysis {
        beats: grid(8),
        cues: vec![cue(1, 2000), cue(0, 500)],
        ..Analysis::default()
    };
    let files = anlz::encode(&moved, &b.file_path);
    let dir = r
        .dev()
        .join(b.analyze_path.trim_start_matches('/'))
        .parent()
        .unwrap()
        .to_owned();
    for (ext, bytes) in [
        ("DAT", &files.dat),
        ("EXT", &files.ext),
        ("2EX", &files.ex2),
    ] {
        std::fs::write(dir.join(format!("ANLZ0000.{ext}")), bytes).unwrap();
    }

    let u = upd(&full, &baselines);
    assert_eq!((u.matched, u.edits.len()), (2, 2), "{u:?}");
    let ea = u.edits.iter().find(|e| e.key == a_key).unwrap();
    assert_eq!(
        (ea.rating, ea.color, ea.comment.as_deref()),
        (Some(4), Some(3), Some("from the player"))
    );
    assert!(ea.cues.is_none() && ea.grid.is_none());
    let eb = u.edits.iter().find(|e| e.key == b_key).unwrap();
    let times: Vec<_> = eb
        .cues
        .as_ref()
        .unwrap()
        .iter()
        .map(|c| (c.hot_cue, c.time_ms))
        .collect();
    assert_eq!(times, [(0, 500), (1, 2000)]);
    assert!(eb.rating.is_none() && eb.comment.is_none() && eb.color.is_none() && eb.grid.is_none());

    // Running it again with the new baselines imports nothing: the device is the baseline now.
    baselines.extend(u.baselines.iter().cloned());
    assert_eq!(upd(&full, &baselines).edits.len(), 0);
}

/// Mirrors a pdb history into the OneLibrary the way a player that writes both would.
fn carry_history_into_onelibrary(root: &Path, name: &str, ids: &[u32]) {
    let c = one(root);
    c.execute("insert into history values (3, 0, ?1, 0, 0)", [name])
        .unwrap();
    for (i, id) in [ids[1], ids[0], ids[3]].iter().enumerate() {
        c.execute(
            "insert into history_content values (3, ?1, ?2)",
            [*id as i64, i as i64 + 1],
        )
        .unwrap();
    }
}

#[test]
fn histories_are_deleted_from_both_libraries() {
    let r = Root::new("history");
    let full = setup(&r);
    exp(&r.dev(), &full);
    let mut d = db(&r.dev());
    let ids: Vec<u32> = d.tracks.iter().map(|t| t.id).collect();
    d.histories.push(History {
        id: 3,
        name: "HISTORY 2026-02-03".into(),
        tracks: vec![ids[1], ids[0], ids[3]],
    });
    write_db(&r.dev(), &d);
    carry_history_into_onelibrary(&r.dev(), "HISTORY 2026-02-03", &ids);

    let target = RemoveTarget::Histories {
        names: vec!["HISTORY 2026-02-03".into()],
    };
    assert_eq!(remove_op(&r.dev(), &target, false).unwrap().histories, 1);
    assert!(db(&r.dev()).histories.is_empty());
    assert!(names(&one(&r.dev()), "select name from history").is_empty());
    assert_eq!(titles(&r.dev()).len(), 4, "the tracks stay");
}

#[test]
fn hot_cue_bank_lists_are_written_to_the_onelibrary_and_survive_updates() {
    let r = Root::new("bank");
    let full = setup(&r);
    let bank = |name: &str, i: usize, ms| Bank {
        name: name.into(),
        cues: vec![BankCue {
            file: full.tracks[i].0.file.clone().unwrap(),
            cue: cue(1, ms),
        }],
    };
    let o = ExportOptions {
        banks: vec![bank("Opening", 0, 1500), bank("Gone", 3, 10)],
        ..ExportOptions::default()
    };
    let rep = export(&r.dev(), &narrowed(&full, &["p1"]), &o, Some(KEY)).unwrap();
    assert!(
        rep.warnings.iter().any(|w| w.contains("Gone")),
        "{:?}",
        rep.warnings
    ); // track d is not in P1
    let q = |sql: &str| {
        one(&r.dev())
            .query_row(sql, [], |x| x.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(
        (
            q("select count(*) from hotCueBankList where name = 'Opening'"),
            q("select inUsec from cue"),
            q("select kind from cue")
        ),
        (1, 1_500_000, 1)
    );
    // A later export without banks keeps the device's list.
    exp(&r.dev(), &narrowed(&full, &["p2"]));
    assert_eq!(
        (
            q("select count(*) from hotCueBankList"),
            q("select count(*) from cue"),
            q("select count(*) from hotCueBankList_cue")
        ),
        (2, 1, 1)
    );
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

#[test]
fn a_backup_lists_the_tracks_the_restore_copies_back() {
    let r = Root::new("backup");
    let full = setup(&r);
    exp(&r.dev(), &narrowed(&full, &["p1"]));
    let backup = r.0.join("backup");
    copy_tree(&r.dev().join("PIONEER"), &backup.join("PIONEER"));
    let listed = backup_tracks(&backup, &Value::Null).unwrap();
    let want: Vec<_> = db(&r.dev())
        .tracks
        .iter()
        .map(|t| {
            (
                t.title.clone(),
                t.filename.clone(),
                u64::from(t.file_size),
                t.file_path.trim_start_matches('/').to_owned(),
            )
        })
        .collect();
    assert_eq!(
        listed
            .iter()
            .map(|t| (t.title.clone(), t.name.clone(), t.size, t.file.clone()))
            .collect::<Vec<_>>(),
        want
    );
    assert_eq!(listed[0].name, "a.wav");
    assert_eq!(
        listed[0].size,
        std::fs::metadata(r.src().join("a.wav")).unwrap().len()
    );
    assert!(
        backup_tracks(&r.src(), &Value::Null)
            .unwrap_err()
            .contains("not a device library backup")
    );
}
