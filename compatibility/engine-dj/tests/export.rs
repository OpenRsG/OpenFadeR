//! Export round trip: synthetic tracks → `export` into a temp dir → the written `m.db` read back with plain SQLite and a blob
//! reader written here from the libdjinterop layouts (not the encoder under test). No real stick or player is involved.

mod common;

use common::*;
use compat_engine_dj::{database, perf};
use flate2::read::ZlibDecoder;
use openfader_plugin::library::{Analysis, Beat, ExportOptions, LibraryPlugin, Snapshot};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

fn open(root: &Path) -> Connection {
    Connection::open_with_flags(database(root), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn id_of(c: &Connection, title: &str) -> i64 {
    c.query_row("SELECT id FROM Track WHERE title = ?1", [title], |r| {
        r.get(0)
    })
    .unwrap()
}

fn blob(c: &Connection, id: i64, col: &str) -> Option<Vec<u8>> {
    c.query_row(
        &format!("SELECT {col} FROM PerformanceData WHERE trackId = ?1"),
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

/// A cursor over blob bytes.
struct Rd(Vec<u8>, usize);

impl Rd {
    /// A compressed blob: 4-byte big-endian length, then a zlib stream of that many bytes.
    fn inflated(b: &[u8]) -> Self {
        let mut out = Vec::new();
        ZlibDecoder::new(&b[4..]).read_to_end(&mut out).unwrap();
        assert_eq!(
            out.len(),
            u32::from_be_bytes(b[..4].try_into().unwrap()) as usize,
            "length prefix"
        );
        Self(out, 0)
    }
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let v = self.0[self.1..self.1 + N].try_into().unwrap();
        self.1 += N;
        v
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn be_i64(&mut self) -> i64 {
        i64::from_be_bytes(self.take())
    }
    fn be_f64(&mut self) -> f64 {
        f64::from_be_bytes(self.take())
    }
    fn le_i64(&mut self) -> i64 {
        i64::from_le_bytes(self.take())
    }
    fn le_f64(&mut self) -> f64 {
        f64::from_le_bytes(self.take())
    }
    fn label(&mut self) -> String {
        let n = self.u8() as usize;
        let s = String::from_utf8(self.0[self.1..self.1 + n].to_vec()).unwrap();
        self.1 += n;
        s
    }
    fn done(&self) -> bool {
        self.1 == self.0.len()
    }
}

/// `(label, sample offset, argb)` of the quick cue slots and `(adjusted main, flag, default main)`.
fn read_quick_cues(b: &[u8]) -> (Vec<(String, f64, [u8; 4])>, (f64, u8, f64)) {
    let mut r = Rd::inflated(b);
    let n = r.be_i64();
    let cues = (0..n)
        .map(|_| (r.label(), r.be_f64(), r.take::<4>()))
        .collect();
    let main = (r.be_f64(), r.u8(), r.be_f64());
    assert!(r.done());
    (cues, main)
}

/// `(label, start, end, start set, end set, argb)`; the loops blob is not compressed.
fn read_loops(b: &[u8]) -> Vec<(String, f64, f64, u8, u8, [u8; 4])> {
    let mut r = Rd(b.to_vec(), 0);
    let n = r.le_i64();
    let loops = (0..n)
        .map(|_| {
            (
                r.label(),
                r.le_f64(),
                r.le_f64(),
                r.u8(),
                r.u8(),
                r.take::<4>(),
            )
        })
        .collect();
    assert!(r.done());
    loops
}

/// `(rate, samples, is set, markers (offset, number, beats to the next))` of the default grid, after checking the adjusted one equals it.
fn read_beats(b: &[u8]) -> (f64, f64, u8, Vec<(f64, i64, i32)>) {
    let mut r = Rd::inflated(b);
    let (rate, samples, set) = (r.be_f64(), r.be_f64(), r.u8());
    let mut grid = || {
        let n = r.be_i64();
        (0..n)
            .map(|_| {
                let m = (r.le_f64(), r.le_i64(), i32::from_le_bytes(r.take()));
                assert_eq!(r.take::<4>(), [0; 4]);
                m
            })
            .collect::<Vec<_>>()
    };
    let (default, adjusted) = (grid(), grid());
    assert_eq!(default, adjusted);
    assert_eq!(r.0.len() - r.1, 9, "nine trailing zero bytes");
    (rate, samples, set, default)
}

/// `(rate, samples, key)`; the loudnesses are 0.
fn read_track_data(b: &[u8]) -> (f64, i64, i32) {
    let mut r = Rd::inflated(b);
    let out = (r.be_f64(), r.be_i64(), i32::from_be_bytes(r.take()));
    assert_eq!([r.be_f64(), r.be_f64(), r.be_f64()], [0.0; 3]);
    assert!(r.done());
    out
}

/// `(samples per point, points, maximum)`.
fn read_overview(b: &[u8]) -> (f64, Vec<[u8; 3]>, [u8; 3]) {
    let mut r = Rd::inflated(b);
    let (n, again, spp) = (r.be_i64(), r.be_i64(), r.be_f64());
    assert_eq!(n, again);
    let points = (0..n).map(|_| r.take::<3>()).collect();
    let max = r.take::<3>();
    assert!(r.done());
    (spp, points, max)
}

/// Sample offset of beat `number` on a decoded grid: linear between the two markers around it.
fn at(grid: &[(f64, i64, i32)], number: i64) -> f64 {
    let i = grid
        .windows(2)
        .position(|w| number >= w[0].1 && number <= w[1].1)
        .expect("beat inside the grid");
    let (a, b) = (grid[i], grid[i + 1]);
    a.0 + (b.0 - a.0) * (number - a.1) as f64 / (b.1 - a.1) as f64
}

fn ids(c: &Connection, list: &str) -> Vec<i64> {
    // The chain of a list: the entity whose next is 0 is last, each earlier one is named by the next.
    let id: i64 = c
        .query_row("SELECT id FROM Playlist WHERE title = ?1", [list], |r| {
            r.get(0)
        })
        .unwrap();
    let mut rows: Vec<(i64, i64, i64)> = c
        .prepare("SELECT id, trackId, nextEntityId FROM PlaylistEntity WHERE listId = ?1")
        .unwrap()
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = vec![];
    let mut next = 0;
    while let Some(p) = rows.iter().position(|r| r.2 == next) {
        let r = rows.remove(p);
        out.push(r.1);
        next = r.0;
    }
    assert!(rows.is_empty(), "entity chain is broken");
    out.reverse();
    out
}

fn files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().path())
                .map(|p| if p.is_dir() { files(&p) } else { 1 })
                .sum()
        })
        .unwrap_or(0)
}

#[test]
fn fresh_library_holds_everything() {
    let d = Dirs::new("fresh");
    let report = export(&d, &setup(&d)).unwrap();
    assert_eq!(
        (
            report.tracks,
            report.added,
            report.updated,
            report.playlists
        ),
        (2, 2, 0, 3)
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("Gamma")),
        "{:?}",
        report.warnings
    );
    assert!(
        report.notes.iter().any(|n| n.starts_with("2 cues")),
        "{:?}",
        report.notes
    );
    assert!(
        report.notes.iter().any(|n| n.starts_with("1 new tracks")),
        "{:?}",
        report.notes
    );

    let c = open(&d.stick());
    let ok: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let (uuid, v): (String, (i64, i64, i64)) = c
        .query_row("SELECT uuid, schemaVersionMajor, schemaVersionMinor, schemaVersionPatch FROM Information", [], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })
        .unwrap();
    assert_eq!((uuid.len(), v), (36, (3, 0, 0)));
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM Information", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM Track", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );

    // Track row of A: the audio sits at the shared place, the path points at it from Engine Library.
    let a = id_of(&c, "Alpha é");
    type Row = (
        String,
        String,
        i64,
        i64,
        f64,
        i64,
        i64,
        String,
        String,
        i64,
        i64,
        String,
        i64,
        i64,
        String,
        i64,
    );
    let row: Row = c
        .query_row(
            "SELECT path, filename, length, bpm, bpmAnalyzed, bitrate, fileBytes, artist, album, key, rating, fileType, \
             isAnalyzed, dateAdded, label, isAvailable FROM Track WHERE id = ?1",
            [a],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                    r.get(14)?,
                    r.get(15)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "../Contents/Art_ist_ X/Album/a.wav");
    assert!(d.stick().join("Contents/Art_ist_ X/Album/a.wav").is_file());
    assert_eq!(row.1, "a.wav");
    assert_eq!((row.2, row.3, row.4, row.5), (12, 120, 120.0, 1411));
    assert_eq!(
        row.6,
        std::fs::metadata(d.src().join("a.wav")).unwrap().len() as i64
    );
    assert_eq!((row.7.as_str(), row.8.as_str()), ("Art/ist: X", "Album"));
    assert_eq!((row.9, row.10, row.11.as_str(), row.12), (1, 80, "wav", 1));
    assert_eq!(row.13, 1767312000, "2026-01-02 00:00 UTC");
    assert_eq!((row.14.as_str(), row.15), ("Label", 1));

    // Key of B comes from the track's own text, and it has no analysis to refuse over.
    let b = id_of(&c, "Beta");
    let (key, analysed): (i64, i64) = c
        .query_row(
            "SELECT key, isAnalyzed FROM Track WHERE id = ?1",
            [b],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((key, analysed), (6, 0));
    for col in [
        "trackData",
        "overviewWaveFormData",
        "beatData",
        "quickCues",
        "loops",
    ] {
        assert!(
            blob(&c, b, col).is_none(),
            "{col} of an unanalysed track stays empty"
        );
    }
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|w| w.contains("sample rate"))
            .count(),
        0,
        "B has no analysis: nothing to skip"
    );

    // Quick cues: hot cue n is slot n-1, own colour or the stock pad colour, empty slots as libdjinterop writes them.
    let (cues, main) = read_quick_cues(&blob(&c, a, "quickCues").unwrap());
    let s = |ms: f64| ms * f64::from(RATE) / 1000.0;
    assert_eq!(cues.len(), 8);
    assert_eq!(
        cues[0],
        ("Drop é".to_string(), s(750.0), [0xff, 0xff, 0x00, 0x17])
    );
    assert_eq!(
        cues[1],
        (String::new(), s(6000.0), [0xff, 0xEA, 0x8F, 0x32])
    );
    assert_eq!(
        cues[2],
        (String::new(), s(1500.0), [0xff, 0xB8, 0x55, 0xBF])
    );
    assert!(cues[3..]
        .iter()
        .all(|c| *c == (String::new(), -1.0, [0; 4])));
    assert_eq!(main, (s(1250.0), 1, s(1250.0)), "first memory cue");

    // Loops: the memory loop, then the hot cue loop; empty slots up to 8.
    let loops = read_loops(&blob(&c, a, "loops").unwrap());
    assert_eq!(loops.len(), 8);
    assert_eq!(
        loops[0],
        (
            "Loop".into(),
            s(3250.0),
            s(5250.0),
            1,
            1,
            [0xff, 0x86, 0xC6, 0x4B]
        )
    );
    assert_eq!(
        (loops[1].1, loops[1].2, loops[1].3, loops[1].4),
        (s(6000.0), s(7000.0), 1, 1)
    );
    assert!(loops[2..]
        .iter()
        .all(|l| *l == (String::new(), -1.0, -1.0, 0, 0, [0; 4])));

    // Grid: three markers for the two tempi, every input beat within one sample.
    let (rate, samples, set, grid) = read_beats(&blob(&c, a, "beatData").unwrap());
    assert_eq!(
        (rate, samples, set),
        (f64::from(RATE), 12.0 * f64::from(RATE), 1)
    );
    assert_eq!(grid.len(), 3);
    assert_eq!((grid[0].1, grid[1].1, grid[2].1), (-4, 16, 26));
    assert_eq!((grid[0].2, grid[1].2, grid[2].2), (20, 10, 0));
    assert!(grid[2].0 >= samples, "last marker is past the end");
    for (k, beat) in beats().iter().enumerate() {
        let got = at(&grid, k as i64);
        assert!(
            (got - s(f64::from(beat.time_ms))).abs() < 1.0,
            "beat {k}: {got} vs {}",
            beat.time_ms
        );
    }

    assert_eq!(
        read_track_data(&blob(&c, a, "trackData").unwrap()),
        (f64::from(RATE), 12 * i64::from(RATE), 1)
    );

    // Overview: 1024 points spread over the track rounded to the quantisation number (2 * (44100 / 210) = 420).
    let (spp, points, max) = read_overview(&blob(&c, a, "overviewWaveFormData").unwrap());
    assert_eq!(points.len(), 1024);
    assert_eq!(
        spp,
        (12.0 * f64::from(RATE) / 420.0).floor() * 420.0 / 1024.0
    );
    let want = |i: usize| {
        let p = 1200 * (2 * i + 1) / 2048;
        [(p % 256) as u8, (p * 2 % 256) as u8, 7]
    };
    assert!((0..1024).all(|i| points[i] == want(i)));
    assert_eq!(
        max,
        [0, 1, 2].map(|ch| (0..1024).map(|i| want(i)[ch]).max().unwrap())
    );

    // Lists: Sets > Warm and Top as the tree, in order, each with its tracks once.
    let list_row = |t: &str| -> (i64, i64, i64, i64) {
        c.query_row(
            "SELECT id, parentListId, nextListId, isPersisted FROM Playlist WHERE title = ?1",
            [t],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    let (sets, warm, top) = (list_row("Sets"), list_row("Warm"), list_row("Top"));
    assert_eq!((sets.1, sets.2, sets.3), (0, top.0, 1));
    assert_eq!((warm.1, warm.2, top.1, top.2), (sets.0, 0, 0, 0));
    assert_eq!(ids(&c, "Warm"), vec![a, b]);
    assert_eq!(ids(&c, "Top"), vec![b, a]);
    assert_eq!(ids(&c, "Sets"), Vec::<i64>::new());
    let paths: Vec<String> = c
        .prepare("SELECT path FROM PlaylistPath ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // The view lists the ancestors in the order SQLite's GROUP_CONCAT gives, so compare as sets of names.
    let names: Vec<std::collections::BTreeSet<&str>> = paths
        .iter()
        .map(|p| p.split(';').filter(|s| !s.is_empty()).collect())
        .collect();
    assert_eq!(names.len(), 3);
    assert!(
        names.contains(&["Sets"].into())
            && names.contains(&["Top"].into())
            && names.contains(&["Sets", "Warm"].into())
    );
}

#[test]
fn second_export_refreshes_in_place_and_merges() {
    let d = Dirs::new("merge");
    let snap = setup(&d);
    export(&d, &snap).unwrap();
    let before = open(&d.stick());
    let (a, b) = (id_of(&before, "Alpha é"), id_of(&before, "Beta"));
    let (grid_before, overview_before, b_row_before) = (
        blob(&before, a, "beatData"),
        blob(&before, a, "overviewWaveFormData"),
        before
            .query_row(
                "SELECT lastEditTime, dateAdded FROM Track WHERE id = ?1",
                [b],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap(),
    );
    drop(before);

    // The vendor's own software (or a player) added a track and a list of its own.
    {
        let c = Connection::open(database(&d.stick())).unwrap();
        c.execute(
            "INSERT INTO Track (path, filename, title, length, isAnalyzed, albumArtId, originDatabaseUuid, originTrackId) \
             VALUES ('../Foreign/f.mp3', 'f.mp3', 'Foreign', 200, 1, 1, 'other-library', 77)",
            [],
        )
        .unwrap();
        c.execute("UPDATE PerformanceData SET quickCues = x'0102' WHERE trackId = (SELECT id FROM Track WHERE title = 'Foreign')", []).unwrap();
        c.execute("INSERT INTO Playlist (title, parentListId, isPersisted, nextListId, lastEditTime, isExplicitlyExported) VALUES ('Mine', 0, 1, 0, 0, 1)", []).unwrap();
        let mine: i64 = c
            .query_row("SELECT id FROM Playlist WHERE title = 'Mine'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let f: i64 = c
            .query_row("SELECT id FROM Track WHERE title = 'Foreign'", [], |r| {
                r.get(0)
            })
            .unwrap();
        c.execute("INSERT INTO PlaylistEntity (listId, trackId, databaseUuid, nextEntityId, membershipReference) VALUES (?1, ?2, 'other-library', 0, 0)", params![mine, f]).unwrap();
    }
    let audio = files(&d.stick().join("Contents"));
    assert_eq!(audio, 2);

    // The host pushes one track with a moved hot cue A and only that: no lists, no grid, no waveform.
    let mut moved = snap.tracks[0].clone();
    moved.1 = Some(Analysis {
        cues: vec![cue(1, 800, None, None, Some("Moved"))],
        ..Default::default()
    });
    moved.0.rating = 2;
    let report = export(
        &d,
        &Snapshot {
            tracks: vec![moved],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        (
            report.tracks,
            report.added,
            report.updated,
            report.playlists
        ),
        (1, 0, 1, 0)
    );

    let c = open(&d.stick());
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM Track", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3,
        "A, B and the foreign track"
    );
    assert_eq!(id_of(&c, "Alpha é"), a, "same row");
    assert_eq!(
        files(&d.stick().join("Contents")),
        audio,
        "no second copy of the audio"
    );
    let rating: i64 = c
        .query_row("SELECT rating FROM Track WHERE id = ?1", [a], |r| r.get(0))
        .unwrap();
    assert_eq!(rating, 40);
    let (cues, main) = read_quick_cues(&blob(&c, a, "quickCues").unwrap());
    assert_eq!(cues[0].0, "Moved");
    assert_eq!(cues[0].1, 800.0 * f64::from(RATE) / 1000.0);
    assert_eq!(cues[2].1, -1.0, "cues are replaced, not merged");
    assert_eq!(
        main.0,
        800.0 * f64::from(RATE) / 1000.0,
        "no memory cue: the first hot cue"
    );
    assert!(
        read_loops(&blob(&c, a, "loops").unwrap())
            .iter()
            .all(|l| l.3 == 0),
        "the loops are gone"
    );
    assert_eq!(
        blob(&c, a, "beatData"),
        grid_before,
        "a snapshot without a grid keeps the library's"
    );
    assert_eq!(blob(&c, a, "overviewWaveFormData"), overview_before);
    let analysed: i64 = c
        .query_row("SELECT isAnalyzed FROM Track WHERE id = ?1", [a], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(analysed, 1);

    // B was not in the snapshot: untouched. The foreign track, its blob and its list survive; the lists of the first export too.
    assert_eq!(
        c.query_row(
            "SELECT lastEditTime, dateAdded FROM Track WHERE id = ?1",
            [b],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        b_row_before
    );
    let f = id_of(&c, "Foreign");
    assert_eq!(blob(&c, f, "quickCues"), Some(vec![1, 2]));
    assert_eq!(ids(&c, "Mine"), vec![f]);
    assert_eq!(ids(&c, "Warm"), vec![a, b]);
    assert_eq!(ids(&c, "Top"), vec![b, a]);
    let ok: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    assert!(!database(&d.stick()).with_extension("db.new").exists());
}

#[test]
fn same_path_list_is_replaced_and_keeps_its_place() {
    let d = Dirs::new("lists");
    let snap = setup(&d);
    export(&d, &snap).unwrap();
    let c = open(&d.stick());
    let (a, b) = (id_of(&c, "Alpha é"), id_of(&c, "Beta"));
    let rows = |c: &Connection| -> Vec<(String, i64, i64, i64)> {
        c.prepare("SELECT title, id, parentListId, nextListId FROM Playlist ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let before = rows(&c);
    drop(c);

    // Same folder and list names, other members, and a new list next to them.
    let again = Snapshot {
        tracks: snap.tracks.clone(),
        playlists: vec![
            list("Sets", 0, true),
            list("Warm", 1, false),
            list("New", 0, false),
        ],
        members: vec![vec![], vec![1], vec![0]],
        ..Default::default()
    };
    let report = export(&d, &again).unwrap();
    assert_eq!((report.added, report.updated), (0, 2));
    let c = open(&d.stick());
    assert_eq!(ids(&c, "Warm"), vec![b], "content replaced");
    assert_eq!(ids(&c, "New"), vec![a]);
    assert_eq!(
        ids(&c, "Top"),
        vec![b, a],
        "a list the snapshot does not name stays"
    );
    let after = rows(&c);
    // Same ids, titles and parents; only the end of the root chain moved on (Top used to be last).
    let same = |v: &[(String, i64, i64, i64)]| {
        v[..3]
            .iter()
            .map(|r| (r.0.clone(), r.1, r.2))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        same(&after),
        same(&before),
        "rows of Sets, Warm and Top did not move"
    );
    assert_eq!((after[0].3, before[2].3), (before[0].3, 0));
    // New is the last of the root chain: Sets > Top > New.
    let top_next: i64 = c
        .query_row(
            "SELECT nextListId FROM Playlist WHERE title = 'Top'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(top_next, after[3].1);
    assert_eq!(after[3].3, 0);
}

#[test]
fn a_track_without_sample_rate_gets_a_row_and_a_warning() {
    let d = Dirs::new("rate");
    let mut snap = setup(&d);
    snap.tracks[0].0.sample_rate = 0;
    // No host table: the sample rate of the audio is unknown.
    let report = compat_engine_dj::Plugin::export(
        &d.stick(),
        &snap,
        &openfader_plugin::library::ExportOptions::default(),
        &serde_json::Value::Null,
    )
    .unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("Alpha") && w.contains("sample rate")),
        "{:?}",
        report.warnings
    );
    let c = open(&d.stick());
    let a = id_of(&c, "Alpha é");
    assert!(blob(&c, a, "quickCues").is_none() && blob(&c, a, "beatData").is_none());
    assert_eq!(report.added, 2);
}

#[test]
fn another_schema_version_is_refused_untouched() {
    let d = Dirs::new("version");
    let db = database(&d.stick());
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    {
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE Information (id INTEGER PRIMARY KEY, uuid TEXT, schemaVersionMajor INTEGER, schemaVersionMinor INTEGER, \
             schemaVersionPatch INTEGER); INSERT INTO Information VALUES (1, 'x', 3, 1, 0);",
        )
        .unwrap();
    }
    let bytes = std::fs::read(&db).unwrap();
    let err = export(&d, &setup(&d)).unwrap_err();
    assert!(err.contains("3.1.0") && err.contains("3.0.2"), "{err}");
    assert_eq!(
        std::fs::read(&db).unwrap(),
        bytes,
        "the library was not touched"
    );
    assert!(
        !db.with_extension("db.new").exists(),
        "no temporary file is left"
    );

    // A file that is no Engine library at all is refused too.
    std::fs::write(&db, b"not a database").unwrap();
    assert!(export(&d, &setup(&d)).is_err());
    assert_eq!(std::fs::read(&db).unwrap(), b"not a database");
}

fn version(c: &Connection) -> (i64, i64, i64) {
    c.query_row(
        "SELECT schemaVersionMajor, schemaVersionMinor, schemaVersionPatch FROM Information",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

/// Makes the library of the stick look like one another Engine release wrote: `sql` adds what that schema has more, `v` is its version.
fn relabel(d: &Dirs, v: (i64, i64, i64), sql: &str) {
    let c = Connection::open(database(&d.stick())).unwrap();
    c.execute_batch(sql).unwrap();
    c.execute(
        "UPDATE Information SET schemaVersionMajor = ?1, schemaVersionMinor = ?2, schemaVersionPatch = ?3",
        params![v.0, v.1, v.2],
    )
    .unwrap();
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A only, as an earlier export left it.
fn first_export(d: &Dirs) -> Snapshot {
    let snap = setup(d);
    export(
        d,
        &Snapshot {
            tracks: vec![snap.tracks[0].clone()],
            ..Default::default()
        },
    )
    .unwrap();
    snap
}

#[test]
fn libraries_of_engine_4_3_and_later_are_extended_in_place() {
    // 3.0.1 only changes triggers; 3.0.2 adds the album art source hash column, which stays NULL.
    for (patch, sql) in [
        (1, ""),
        (
            2,
            "ALTER TABLE Track ADD COLUMN albumArtSourceHash CHAR(40);",
        ),
    ] {
        let d = Dirs::new(&format!("v30{patch}"));
        let snap = first_export(&d);
        relabel(&d, (3, 0, patch), sql);
        let report = export(&d, &snap).unwrap();
        assert_eq!(
            (report.added, report.updated),
            (1, 1),
            "B added, A refreshed"
        );

        let c = open(&d.stick());
        assert_eq!(version(&c), (3, 0, patch), "the library keeps its version");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM Track"), 2);
        let a = id_of(&c, "Alpha é");
        assert!(blob(&c, a, "beatData").is_some() && blob(&c, a, "quickCues").is_some());
        assert_eq!(ids(&c, "Warm"), vec![a, id_of(&c, "Beta")]);
        if patch == 2 {
            assert_eq!(
                count(
                    &c,
                    "SELECT COUNT(*) FROM Track WHERE albumArtSourceHash IS NOT NULL"
                ),
                0
            );
        }
        assert_eq!(
            c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
}

#[test]
fn engine_2_schema_keeps_the_analysis_in_track() {
    let d = Dirs::new("v2");
    let snap = setup(&d);
    let report = export_schema(&d, &snap, "2.18.0").unwrap();
    assert_eq!((report.added, report.playlists), (2, 3));

    let c = open(&d.stick());
    assert_eq!(version(&c), (2, 18, 0));
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    // 2.18.0: PerformanceData is a view over Track, and Track has neither activeOnLoadLoops nor lastEditTime.
    assert_eq!(
        c.query_row(
            "SELECT type FROM sqlite_master WHERE name = 'PerformanceData'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "view"
    );
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM pragma_table_info('Track') WHERE name IN ('activeOnLoadLoops', 'lastEditTime')"),
        0
    );
    assert_eq!(count(&c, "SELECT COUNT(*) FROM AlbumArt WHERE id = 1"), 1);

    // The blobs are columns of Track, and they decode as in the 3.x library.
    let a = id_of(&c, "Alpha é");
    let direct = |col: &str| -> Option<Vec<u8>> {
        c.query_row(
            &format!("SELECT {col} FROM Track WHERE id = ?1"),
            [a],
            |r| r.get(0),
        )
        .unwrap()
    };
    for col in [
        "trackData",
        "overviewWaveFormData",
        "beatData",
        "quickCues",
        "loops",
    ] {
        assert!(direct(col).is_some(), "{col}");
        assert_eq!(direct(col), blob(&c, a, col), "{col} through the view");
    }
    assert_eq!(
        read_track_data(&direct("trackData").unwrap()),
        (f64::from(RATE), 12 * i64::from(RATE), 1)
    );
    let (rate, samples, set, grid) = read_beats(&direct("beatData").unwrap());
    assert_eq!(
        (rate, samples, set, grid.len()),
        (f64::from(RATE), 12.0 * f64::from(RATE), 1, 3)
    );
    let (cues, main) = read_quick_cues(&direct("quickCues").unwrap());
    assert_eq!(cues[0].0, "Drop é");
    assert_eq!(cues[0].1, 750.0 * f64::from(RATE) / 1000.0);
    assert_eq!(main.1, 1);
    assert_eq!(read_loops(&direct("loops").unwrap())[0].0, "Loop");
    assert_eq!(
        read_overview(&direct("overviewWaveFormData").unwrap())
            .1
            .len(),
        1024
    );
    assert_eq!(
        count(&c, &format!("SELECT isAnalyzed FROM Track WHERE id = {a}")),
        1
    );

    // The unanalysed track and the lists.
    let b = id_of(&c, "Beta");
    assert_eq!(
        count(&c, &format!("SELECT isAnalyzed FROM Track WHERE id = {b}")),
        0
    );
    assert!(blob(&c, b, "beatData").is_none() && blob(&c, b, "quickCues").is_none());
    assert_eq!(ids(&c, "Warm"), vec![a, b]);
    assert_eq!(ids(&c, "Top"), vec![b, a]);
    drop(c);

    // A later export, with the default option, extends it and keeps 2.18.0.
    let delta = track(&d, "d.wav", "Delta", "Other", 3);
    let report = export(
        &d,
        &Snapshot {
            tracks: vec![(delta.clone(), None)],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((report.added, report.updated), (1, 0));
    assert!(
        report.notes.iter().all(|n| !n.contains("schema")),
        "{:?}",
        report.notes
    );
    let c = open(&d.stick());
    assert_eq!(version(&c), (2, 18, 0));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM Track"), 3);
    drop(c);

    // The same library relabelled as 2.20.3 (which has lastEditTime): a new track gets one.
    relabel(
        &d,
        (2, 20, 3),
        "ALTER TABLE Track ADD COLUMN activeOnLoadLoops INTEGER; ALTER TABLE Track ADD COLUMN lastEditTime DATETIME;",
    );
    let echo = track(&d, "e.wav", "Echo", "Other", 3);
    export(
        &d,
        &Snapshot {
            tracks: vec![(echo, Some(analysis()))],
            ..Default::default()
        },
    )
    .unwrap();
    let c = open(&d.stick());
    assert_eq!(version(&c), (2, 20, 3));
    let e = id_of(&c, "Echo");
    assert!(
        count(
            &c,
            &format!("SELECT lastEditTime FROM Track WHERE id = {e}")
        ) > 1_700_000_000
    );
    assert!(blob(&c, e, "beatData").is_some());
}

#[test]
fn the_schema_option_only_decides_a_new_library() {
    let d = Dirs::new("option");
    let snap = setup(&d);
    let err = export_schema(&d, &snap, "2.20.3").unwrap_err();
    assert!(
        err.contains("Choose 3.0.0 or 2.18.0") && err.contains("2.20.3"),
        "{err}"
    );
    assert!(
        !d.stick().join("Contents").exists() && !d.stick().join("Engine Library").exists(),
        "nothing copied or created"
    );

    export(&d, &snap).unwrap();
    let report = export_schema(&d, &snap, "2.18.0").unwrap();
    assert_eq!(
        version(&open(&d.stick())),
        (3, 0, 0),
        "an existing library is not converted"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("3.0.0") && n.contains("2.18.0")),
        "{:?}",
        report.notes
    );
}

#[test]
fn old_two_file_library_and_full_stick_are_refused_before_any_write() {
    let d = Dirs::new("refuse");
    let snap = setup(&d);
    let full = compat_engine_dj::Plugin::export(
        &d.stick(),
        &snap,
        &ExportOptions {
            free: Some(10),
            ..Default::default()
        },
        &Value::Null,
    );
    assert!(full.unwrap_err().contains("Not enough space"));
    assert!(
        !d.stick().join("Contents").exists() && !d.stick().join("Engine Library").exists(),
        "nothing copied or created"
    );

    std::fs::create_dir_all(d.stick().join("Engine Library")).unwrap();
    std::fs::write(d.stick().join("Engine Library/m.db"), b"legacy").unwrap();
    assert!(export(&d, &snap).unwrap_err().contains("old two-file"));
    assert!(!database(&d.stick()).exists());
}

#[test]
fn device_probe_reports_the_library() {
    let d = Dirs::new("probe");
    let none = compat_engine_dj::Plugin::device_probe(&d.stick(), &Value::Null).unwrap();
    assert!(none.libraries.is_empty() && none.can_write && none.modified.is_none());
    export(&d, &setup(&d)).unwrap();
    let some = compat_engine_dj::Plugin::device_probe(&d.stick(), &Value::Null).unwrap();
    assert_eq!(some.libraries, ["Engine Library"]);
    assert!(some.modified.is_some_and(|t| t > 1_700_000_000));
    assert_eq!(some.title, "Engine DJ");
    assert!(
        !some.about.is_empty() && some.about.len() < 140,
        "{}",
        some.about
    );
    let [field] = &some.options[..] else {
        panic!("one option")
    };
    let openfader_plugin::library::FieldKind::Choice { choices, .. } = &field.kind else {
        panic!("a choice")
    };
    assert_eq!(field.key, compat_engine_dj::SCHEMA_OPTION);
    assert_eq!(field.default, "3.0.0");
    let values: Vec<&str> = choices.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, ["3.0.0", "2.18.0"]);
}

/// 128 BPM with beat times rounded to whole ms, as the host stores them: the grid stays at a few markers and no beat is further
/// than the 1 ms the times can be told apart by.
#[test]
fn rounded_times_make_a_short_grid() {
    let beats: Vec<Beat> = (0..300u32)
        .map(|k| Beat {
            bar_pos: (k % 4) as u16 + 1,
            bpm_x100: 12800,
            time_ms: (f64::from(k) * 468.75).round() as u32,
        })
        .collect();
    let total = 141.0 * f64::from(RATE);
    let m = perf::markers(&beats, f64::from(RATE), total);
    assert!(m.len() <= 4, "{} markers", m.len());
    let grid: Vec<(f64, i64, i32)> = m.iter().map(|m| (m.offset, m.number, 0)).collect();
    for (k, b) in beats.iter().enumerate() {
        let err = (at(&grid, k as i64) - f64::from(b.time_ms) * f64::from(RATE) / 1000.0).abs();
        assert!(
            err <= f64::from(RATE) / 1000.0,
            "beat {k}: {err} samples off"
        );
    }
    // A first beat that is not a downbeat keeps its place in the bar: beat 0 is number 2 (the third of the bar).
    let third: Vec<Beat> = beats
        .iter()
        .map(|b| Beat {
            bar_pos: (b.bar_pos + 1) % 4 + 1,
            ..*b
        })
        .collect();
    let m = perf::markers(&third, f64::from(RATE), total);
    assert_eq!(m[0].number, -4);
    assert!((m[0].offset - (-4.0 - 2.0) * 468.75 * f64::from(RATE) / 1000.0).abs() < 1.0);
}

#[test]
fn camelot_keys_are_engine_key_ids() {
    // Engine order: C, Am, G, Em, D, Bm, A, F#m, E, C#m, B, G#m, F#, D#m, C#, A#m, G#, Fm, D#, Cm, A#, Gm, F, Dm.
    let camelot = [
        "8B", "8A", "9B", "9A", "10B", "10A", "11B", "11A", "12B", "12A", "1B", "1A", "2B", "2A",
        "3B", "3A", "4B", "4A", "5B", "5A", "6B", "6A", "7B", "7A",
    ];
    for (id, k) in camelot.iter().enumerate() {
        assert_eq!(perf::key_id(k), Some(id as i32), "{k}");
    }
    assert_eq!(perf::key_id(" 8a "), Some(1));
    for bad in ["", "A", "13A", "0B", "8C", "Am", "é"] {
        assert_eq!(perf::key_id(bad), None, "{bad:?}");
    }
}
