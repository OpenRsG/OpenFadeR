//! Reading round trip: synthetic tracks → `export` (the crate's writer) → `probe` and `open` of the stick → what the host reads
//! back, for both schema lines, plus probe cases and the paths and chains a desktop library has. No real stick or Engine involved.

mod common;

use common::*;
use compat_engine_dj::{database, perf, Plugin};
use openfader_plugin::library::{LibraryPlugin, Source, Track};
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

fn open(path: &Path) -> Box<dyn Source> {
    Plugin::open(path, &Value::Null).unwrap()
}

fn db(d: &Dirs) -> Connection {
    Connection::open(database(&d.stick())).unwrap()
}

fn titles(tracks: &[Track]) -> Vec<&str> {
    tracks.iter().map(|t| t.title.as_str()).collect()
}

/// A stick written with `schema`, read back.
fn round_trip(schema: &str) {
    let d = Dirs::new(&format!("read-{schema}"));
    export_schema(&d, &setup(&d), schema).unwrap();
    let src = open(&d.stick());
    assert_eq!(src.kind(), "Engine DJ");

    let lists = src.playlists().unwrap();
    assert_eq!(
        lists
            .iter()
            .map(|p| (p.name.as_str(), p.depth, p.is_folder))
            .collect::<Vec<_>>(),
        [("Sets", 0, true), ("Warm", 1, false), ("Top", 0, false)]
    );
    let all = src.tracks(None).unwrap();
    assert_eq!(titles(&all), ["Alpha é", "Beta"], "C has no audio file");
    // Playlist order is the chain, not the id order: Top lists B before A.
    let in_list = |i: usize| src.tracks(Some(&lists[i].id)).unwrap();
    assert_eq!(titles(&in_list(1)), ["Alpha é", "Beta"]);
    assert_eq!(titles(&in_list(2)), ["Beta", "Alpha é"]);
    assert!(in_list(0).is_empty());

    let a = &all[0];
    assert_eq!(
        (
            a.artist.as_str(),
            a.album.as_str(),
            a.genre.as_str(),
            a.label.as_str()
        ),
        ("Art/ist: X", "Album", "House", "Label")
    );
    assert_eq!(
        (a.key.as_str(), a.bpm_x100, a.length_s, a.rating),
        ("8A", 12000, 12, 4)
    );
    assert_eq!(
        (
            a.comment.as_str(),
            a.date_added.as_str(),
            a.bitrate,
            a.sample_rate
        ),
        ("nice", "2026-01-02", 1411, RATE)
    );
    let file = a.file.as_ref().unwrap();
    assert!(file.is_absolute() && file.starts_with(d.stick()) && file.is_file());
    assert_eq!(file.file_name().unwrap(), "a.wav");
    assert_eq!(std::fs::read(file).unwrap(), wav(12));

    // B has no analysis, so no sample rate; its key comes from the track.
    let b = &all[1];
    assert_eq!(
        (b.key.as_str(), b.sample_rate, b.bpm_x100),
        ("11B", 0, 12000)
    );

    // Beat grid: the grid starts at beat -4 and ends past the track, so the beats are the 25 written, then more at the last tempo
    // up to the end of the track. The tempo of a beat is that of the stretch it starts.
    let an = src.analysis(a).expect("A has an analysis");
    assert_eq!(an.key.as_deref(), Some("8A"));
    assert_eq!(an.path.as_deref(), Some(file.to_str().unwrap()));
    let want = beats();
    assert!(an.beats.len() >= want.len());
    for (i, (g, w)) in an.beats.iter().zip(&want).enumerate() {
        assert_eq!((g.time_ms, g.bar_pos), (w.time_ms, w.bar_pos), "beat {i}");
        let bpm = if i < 16 { 12000 } else { 15000 };
        assert_eq!(g.bpm_x100, bpm, "beat {i}");
    }
    for w in an.beats[want.len() - 1..].windows(2) {
        assert_eq!(w[1].time_ms - w[0].time_ms, 400);
    }
    assert!(an.beats.last().unwrap().time_ms <= 12_000);

    // Cues: the main cue is the first memory cue, a loop on a hot cue is that cue's loop end, the second memory cue and the
    // hot cue above H did not fit the format. Cues without a colour got the stock pad colour of their slot.
    let mut cues = an.cues;
    cues.sort_by_key(|c| (c.hot_cue, c.time_ms));
    let want = [
        cue(0, 1250, None, None, None),
        cue(0, 3250, Some(5250), Some([0x86, 0xC6, 0x4B]), Some("Loop")),
        cue(1, 750, None, Some([0xFF, 0x00, 0x17]), Some("Drop é")),
        cue(2, 6000, Some(7000), Some([0xEA, 0x8F, 0x32]), None),
        cue(3, 1500, None, Some([0xB8, 0x55, 0xBF]), None),
    ];
    assert_eq!(cues, want);

    let an = src.analysis(b).expect("B has its key");
    assert_eq!(an.key.as_deref(), Some("11B"));
    assert!(an.beats.is_empty() && an.cues.is_empty());
}

#[test]
fn schema_3_round_trip() {
    round_trip("3.0.0");
}

#[test]
fn schema_2_round_trip() {
    round_trip("2.18.0");
}

#[test]
fn camelot_is_the_inverse_of_the_key_id() {
    for n in 1..=12 {
        for m in ['A', 'B'] {
            let k = format!("{n}{m}");
            assert_eq!(perf::camelot(i64::from(perf::key_id(&k).unwrap())), Some(k));
        }
    }
    assert_eq!(perf::camelot(24), None);
    assert_eq!(perf::camelot(-1), None);
}

#[test]
fn probe_claims_only_an_engine_library() {
    let d = Dirs::new("read-probe");
    let stick = d.stick();
    assert!(Plugin::probe(&stick).is_none(), "an empty folder");
    export(&d, &setup(&d)).unwrap();
    let lib = stick.join("Engine Library");
    let m = database(&stick);
    for p in [&stick, &lib, &lib.join("Database2"), &m] {
        let pr = Plugin::probe(p).unwrap_or_else(|| panic!("{}", p.display()));
        assert!(pr.score > 0 && pr.kind == "Engine DJ");
    }
    // The file scores above the folders it could also be reached through.
    assert!(Plugin::probe(&m).unwrap().score > Plugin::probe(&stick).unwrap().score);

    assert!(
        Plugin::probe(&d.src()).is_none(),
        "a folder without a library"
    );
    assert!(
        Plugin::probe(&d.src().join("a.wav")).is_none(),
        "an audio file"
    );
    // An m.db outside Database2 is the old two-file layout; one that is no SQLite file is nothing.
    let old = d.src().join("m.db");
    std::fs::copy(&m, &old).unwrap();
    assert!(Plugin::probe(&old).is_none());
    std::fs::write(&m, b"not a database, not at all").unwrap();
    assert!(Plugin::probe(&m).is_none() && Plugin::probe(&stick).is_none());
    assert!(Plugin::open(&stick, &Value::Null).is_err());
}

#[test]
fn schemas_outside_2_18_to_3_are_refused() {
    let d = Dirs::new("read-schema");
    export(&d, &setup(&d)).unwrap();
    for (v, text) in [
        ((4, 0, 0), "4.0.0"),
        ((1, 2, 3), "1.2.3"),
        ((2, 17, 0), "2.17.0"),
    ] {
        db(&d)
            .execute(
                "UPDATE Information SET schemaVersionMajor = ?1, schemaVersionMinor = ?2, schemaVersionPatch = ?3",
                [v.0, v.1, v.2],
            )
            .unwrap();
        let e = Plugin::open(&d.stick(), &Value::Null)
            .err()
            .expect("refused");
        assert!(e.contains(text), "{e}");
    }
    db(&d)
        .execute(
            "UPDATE Information SET schemaVersionMinor = 0, schemaVersionMajor = 3",
            [],
        )
        .unwrap();
    assert!(Plugin::open(&d.stick(), &Value::Null).is_ok());
}

/// What a desktop library has that a stick written by the exporter does not: paths that climb out of the folder, absolute paths,
/// rows without a path, lists of another database, chains that lost their end and lists that are not persisted.
#[test]
fn desktop_paths_and_damaged_chains() {
    let d = Dirs::new("read-desktop");
    export(&d, &setup(&d)).unwrap();
    {
        let c = db(&d);
        c.execute(
            "UPDATE Track SET path = '../../Music/./x/../y.mp3' WHERE title = 'Alpha é'",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE Track SET path = '/abs/b.wav' WHERE title = 'Beta'",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO Track (path, filename, title, length, isAnalyzed, albumArtId, originDatabaseUuid, originTrackId, key, rating, bpm) \
             VALUES ('../Foreign/f.mp3', 'f.mp3', 'Foreign', 200, 1, 1, 'other-library', 77, 23, 55, 128)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO Track (path, filename, title, length, isAnalyzed, albumArtId) VALUES ('', '', 'Streamed', 200, 1, 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO Playlist (title, parentListId, isPersisted, nextListId, isExplicitlyExported) VALUES ('Other', 0, 1, 0, 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO Playlist (title, parentListId, isPersisted, nextListId, isExplicitlyExported) VALUES ('Temp', 0, 0, 0, 1)",
            [],
        )
        .unwrap();
        let other: i64 = c
            .query_row("SELECT id FROM Playlist WHERE title = 'Other'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // The entity of another database names the track there; one of this database that names a missing track is skipped.
        for (track, uuid) in [(77, "other-library"), (999, "other-library")] {
            c.execute(
                "INSERT INTO PlaylistEntity (listId, trackId, databaseUuid, nextEntityId, membershipReference) VALUES (?1, ?2, ?3, 0, 0)",
                rusqlite::params![other, track, uuid],
            )
            .unwrap();
        }
        // Top's chain loses its end: both entities say they are last. Nothing may be lost.
        c.execute(
            "UPDATE PlaylistEntity SET nextEntityId = 0 WHERE listId = (SELECT id FROM Playlist WHERE title = 'Top')",
            [],
        )
        .unwrap();
    }
    let src = open(&d.stick());
    let all = src.tracks(None).unwrap();
    assert_eq!(
        titles(&all),
        ["Alpha é", "Beta", "Foreign"],
        "no path: no track"
    );
    // `Engine Library/../../Music/./x/../y.mp3`: the folder of the stick is one level up, and `.` and `..` are folded away.
    assert_eq!(
        all[0].file.as_deref(),
        Some(d.stick().parent().unwrap().join("Music/y.mp3").as_path())
    );
    // A rooted path stays as it is (on Windows on the library's drive).
    assert_eq!(
        all[1].file.as_deref(),
        Some(d.stick().join("/abs/b.wav").as_path())
    );
    let foreign = &all[2];
    assert_eq!(
        (
            foreign.key.as_str(),
            foreign.rating,
            foreign.bpm_x100,
            foreign.length_s
        ),
        ("7A", 3, 12800, 200),
        "rating 55 rounds to 3 stars, key id 23 is 7A"
    );

    let lists = src.playlists().unwrap();
    assert_eq!(
        lists.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["Sets", "Warm", "Top", "Other"],
        "Temp is not persisted"
    );
    let ids = |name: &str| {
        let id = &lists.iter().find(|p| p.name == name).unwrap().id;
        titles(&src.tracks(Some(id)).unwrap())
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("Other"), ["Foreign"]);
    let mut top = ids("Top");
    top.sort();
    assert_eq!(top, ["Alpha é", "Beta"]);
    assert!(src.tracks(Some("nope")).is_err());
}

#[test]
fn analysis_ref_needs_no_open_handle() {
    let d = Dirs::new("read-ref");
    export_schema(&d, &setup(&d), "3.0.0").unwrap();
    let all = {
        let src = open(&d.stick());
        src.tracks(None)
            .unwrap()
            .into_iter()
            .map(|t| {
                let a = src.analysis(&t);
                (t, a)
            })
            .collect::<Vec<_>>()
    };
    for (t, a) in all {
        assert!(a.is_some());
        let r = Plugin::analysis_ref(t.analysis_ref.as_deref().unwrap(), &Value::Null);
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(&a).unwrap()
        );
    }
}

#[test]
fn corrupt_grid_beats_are_capped_in_total() {
    use compat_engine_dj::blob::Marker;
    let m = |number, offset| Marker { offset, number };
    // Many markers each far apart, then numbers that overflow a subtraction.
    let grid: Vec<Marker> = (0..50).map(|i| m(i * 1_000_000, i as f64 * 1e9)).collect();
    assert!(perf::beats_from(&grid, 44100.0, 0.0).len() <= 100_001);
    let wild = [m(i64::MIN, 0.0), m(i64::MAX, 1e12)];
    assert!(perf::beats_from(&wild, 44100.0, 0.0).len() <= 100_001);
}
