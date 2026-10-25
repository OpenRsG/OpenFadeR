//! Device export round trip: synthetic tracks → `export` into a temp dir → read back with the writer's own `pdb::read` and the
//! analysis-file reader. No real library or USB is involved.

mod common;

use common::{export, fresh_dir, grid, narrowed, snapshot};
use openfader_plugin::library::{Analysis, Cue, ExportOptions, PREVIEW_ENTRIES, Snapshot, Track};
use rekordbox_common::{anlz, on_device};
use rekordbox_export::{
    pdb::{
        db::{Db, Item},
        read,
    },
    settings::DeviceConfig,
};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// 16-bit mono WAV of `secs` seconds: a click every half second (120 BPM) over a quiet tone.
fn wav(secs: u32) -> Vec<u8> {
    const RATE: u32 = 44100;
    let data: Vec<u8> = (0..RATE * secs)
        .flat_map(|i| {
            let click = if i % (RATE / 2) < 400 { 20000.0 } else { 0.0 };
            let tone = 800.0 * (i as f32 * 0.05).sin();
            ((click + tone) as i16).to_le_bytes()
        })
        .collect();
    let mut b = [
        b"RIFF".as_slice(),
        &(36 + data.len() as u32).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
    ]
    .concat();
    b.extend([1, 0, 1, 0]);
    b.extend(RATE.to_le_bytes());
    b.extend((RATE * 2).to_le_bytes());
    b.extend([2, 0, 16, 0]);
    b.extend(b"data");
    b.extend((data.len() as u32).to_le_bytes());
    b.extend(data);
    b
}

fn cues() -> Vec<Cue> {
    vec![
        Cue {
            hot_cue: 1,
            time_ms: 750,
            loop_end_ms: None,
            color: Some([0xff, 0x00, 0x17]),
            comment: Some("Drop é".into()),
        },
        Cue {
            hot_cue: 5,
            time_ms: 6250,
            loop_end_ms: None,
            color: Some([0x00, 0xc4, 0xff]),
            comment: None,
        },
        Cue {
            hot_cue: 0,
            time_ms: 1250,
            loop_end_ms: None,
            color: None,
            comment: None,
        },
        Cue {
            hot_cue: 0,
            time_ms: 3250,
            loop_end_ms: Some(5250),
            color: None,
            comment: Some("Loop".into()),
        },
    ]
}

struct Dirs {
    src: PathBuf,
    dest: PathBuf,
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.src.parent().unwrap());
    }
}

/// The three tracks (the third has no audio file) and the snapshot of them with the lists Sets > Warm and Top. Track 0 comes
/// with a grid and cues; track 1 has nothing (the export asks the host to analyse its audio).
fn setup(name: &str) -> (Dirs, Snapshot) {
    let root = fresh_dir(&format!("export-{name}"));
    let (src, dest) = (root.join("src"), root.join("device"));
    std::fs::create_dir_all(&src).unwrap();
    let mk = |file: &str, title: &str, artist: &str, secs, album: &str| {
        let f = src.join(file);
        std::fs::write(&f, wav(secs)).unwrap();
        Track {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            genre: "House".into(),
            key: "8A".into(),
            bpm_x100: 12000,
            length_s: secs as i64,
            file: Some(f),
            rating: 4,
            comment: "nice ✓".into(),
            date_added: "2026-01-02".into(),
            bitrate: 1411,
            sample_rate: 44100,
            ..Default::default()
        }
    };
    let mut missing = mk("c.wav", "Gamma", "Art/ist: X", 1, "");
    missing.file = Some(src.join("not-there.wav"));
    let tracks = [
        mk("a.wav", "Alpha é", "Art/ist: X", 12, "Alpha Album"),
        mk("b.wav", "Beta", "Other", 8, ""),
        missing,
    ];
    let lists = [
        ("f", "Sets", 0, true, vec![]),
        ("warm", "Warm", 1, false, vec![1, 0]),
        ("top", "Top", 0, false, vec![0, 2]),
    ];
    // What the snapshot of a library carries: the grid and cues of track 0.
    let snap = snapshot(&tracks, &lists, |t| {
        (t.title == "Alpha é").then(|| Analysis {
            beats: grid(24),
            cues: cues(),
            ..Default::default()
        })
    });
    (Dirs { src, dest }, snap)
}

fn sorted(mut c: Vec<Cue>) -> Vec<Cue> {
    c.sort_by_key(|c| (c.hot_cue, c.time_ms));
    c
}

/// What the reader returns for cues the file stores without a colour: PCP2 always carries three colour bytes.
fn as_stored(c: Vec<Cue>) -> Vec<Cue> {
    sorted(
        c.into_iter()
            .map(|c| Cue {
                color: c.color.or(Some([0, 0, 0])),
                ..c
            })
            .collect(),
    )
}

fn db(dest: &Path) -> Db {
    read::open(dest, &DeviceConfig::default()).unwrap().0
}

fn analysis_of(dest: &Path, t: &Item) -> Analysis {
    anlz::load_track(&on_device(dest, &t.analyze_path)).unwrap()
}

/// Titles of the tracks of the playlist `name`.
fn in_playlist(db: &Db, name: &str) -> Vec<String> {
    let n = db.nodes.iter().find(|n| n.name == name).unwrap();
    n.tracks
        .iter()
        .map(|id| {
            db.tracks
                .iter()
                .find(|t| t.id == *id)
                .unwrap()
                .title
                .clone()
        })
        .collect()
}

/// With the OneLibrary option off a device without one gets the Device Library only; once a device holds a OneLibrary it is
/// kept up to date whatever the option says, so the two libraries never list different tracks.
#[test]
fn the_onelibrary_option_leaves_it_out_of_a_new_library_only() {
    let (d, snap) = setup("one-off");
    let off = ExportOptions {
        config: [(rekordbox_export::ONE_OPTION.to_owned(), "0".to_owned())].into(),
        ..ExportOptions::default()
    };
    let one = d.dest.join("PIONEER/rekordbox/exportLibrary.db");
    export(&d.dest, &snap, &off, None).unwrap();
    assert!(d.dest.join("PIONEER/rekordbox/export.pdb").is_file() && !one.exists());
    export(&d.dest, &snap, &ExportOptions::default(), None).unwrap();
    assert!(one.is_file());
    let before = std::fs::metadata(&one).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    export(&d.dest, &snap, &off, None).unwrap();
    assert!(std::fs::metadata(&one).unwrap().modified().unwrap() > before);
}

#[test]
fn export_reads_back_through_our_readers() {
    let (d, snap) = setup("rt");
    let report = export(&d.dest, &snap, &ExportOptions::default(), None).unwrap();
    assert!(
        d.dest.join("PIONEER/rekordbox/exportLibrary.db").is_file(),
        "the built-in key writes the OneLibrary too"
    );
    assert_eq!((report.tracks, report.playlists), (2, 3));
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);

    let db = db(&d.dest);
    assert_eq!((db.tracks.len(), db.nodes.len()), (2, 3));
    let titles: Vec<_> = db.tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Alpha é", "Beta"]);
    let a = &db.tracks[0];
    assert_eq!(
        (
            a.artist.as_str(),
            a.genre.as_str(),
            a.key.as_str(),
            a.bpm_x100,
            a.duration_s
        ),
        ("Art/ist: X", "House", "8A", 12000, 12)
    );
    assert_eq!(
        (
            a.rating,
            a.comment.as_str(),
            a.date_added.as_str(),
            a.bitrate,
            a.sample_rate
        ),
        (4, "nice ✓", "2026-01-02", 1411, 44100)
    );

    // The audio was copied under Contents/<artist>/<album>/ (illegal characters replaced) and is byte-identical.
    assert_eq!(a.album, "Alpha Album");
    let copy = on_device(&d.dest, &a.file_path);
    assert!(
        copy.starts_with(d.dest.join("Contents/Art_ist_ X/Alpha Album")),
        "{}",
        copy.display()
    );
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(snap.tracks[0].0.file.as_ref().unwrap()).unwrap()
    );
    assert!(
        db.tracks[1].album.is_empty()
            && on_device(&d.dest, &db.tracks[1].file_path)
                .starts_with(d.dest.join("Contents/Other/UnknownAlbum"))
    );

    // Playlist tree and order.
    let shape: Vec<_> = db
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.parent != 0, n.is_folder))
        .collect();
    assert_eq!(
        shape,
        [
            ("Sets", false, true),
            ("Warm", true, false),
            ("Top", false, false)
        ]
    );
    assert_eq!(db.nodes[1].parent, db.nodes[0].id);
    assert_eq!(in_playlist(&db, "Warm"), ["Beta", "Alpha é"]);
    assert_eq!(in_playlist(&db, "Top"), ["Alpha é"]); // the left-out track is not listed

    // Analysis files: the given grid and cues survive exactly; the track without analysis got the host's.
    let ana = analysis_of(&d.dest, a);
    assert_eq!(
        ana.path.as_deref(),
        Some("/Contents/Art_ist_ X/Alpha Album/a.wav")
    );
    assert_eq!(ana.beats, grid(24));
    assert_eq!(sorted(ana.cues), as_stored(cues()));
    assert_eq!(
        (ana.preview.len(), ana.three_band_preview.len()),
        (400, PREVIEW_ENTRIES)
    );
    assert!(!ana.three_band_detail.is_empty() && ana.detail.len() == ana.three_band_detail.len());
    let ana_b = analysis_of(&d.dest, &db.tracks[1]);
    assert!(ana_b.beats.len() > 10, "{} beats", ana_b.beats.len());
    assert!(
        (ana_b.beats[0].bpm_x100 as i32 - 12000).abs() < 300,
        "{}",
        ana_b.beats[0].bpm_x100
    );
    assert_eq!(
        db.tracks.iter().map(|t| t.color).collect::<Vec<_>>(),
        [0, 0]
    );
    // The baselines of the next update, one per track written.
    assert_eq!(report.baselines.len(), 2, "{:?}", report.baselines);
}

#[test]
fn snapshot_edits_colours_user_playlists_and_mytag_are_exported() {
    let (d, mut snap) = setup("store");
    // What the host's store lays over the library: rating, comment, cues, a colour, a user playlist and MyTag.
    let edited = vec![Cue {
        hot_cue: 2,
        time_ms: 2250,
        loop_end_ms: None,
        color: Some([1, 2, 3]),
        comment: Some("mine".into()),
    }];
    snap.tracks[0].0.rating = 1;
    snap.tracks[0].0.comment = "from the store".into();
    snap.tracks[0].1.as_mut().unwrap().cues = edited.clone();
    snap.colors = vec![(0, 2)];
    snap.playlists.push(openfader_plugin::library::Playlist {
        id: "mine".into(),
        name: "My set".into(),
        depth: 0,
        is_folder: false,
    });
    snap.members.push(vec![1, 0]);
    snap.mytags = vec![("Mood".into(), vec!["Dark".into()])];
    snap.tagged = vec![(0, 0, 0)];
    export(&d.dest, &snap, &ExportOptions::default(), None).unwrap();

    let db = db(&d.dest);
    let a = &db.tracks[0];
    assert_eq!((a.rating, a.comment.as_str()), (1, "from the store"));
    assert_eq!(
        db.tracks.iter().map(|t| t.color).collect::<Vec<_>>(),
        [2, 0]
    );
    assert_eq!(analysis_of(&d.dest, a).cues, edited);
    assert_eq!(in_playlist(&db, "My set"), ["Beta", "Alpha é"]);
    assert!(d.dest.join("PIONEER/rekordbox/exportExt.pdb").is_file());

    // Written again without colours (a stick's own track saved back after a cue edit): the device keeps its colour.
    snap.colors.clear();
    export(&d.dest, &snap, &ExportOptions::default(), None).unwrap();
    assert_eq!(
        crate::db(&d.dest)
            .tracks
            .iter()
            .map(|t| t.color)
            .collect::<Vec<_>>(),
        [2, 0]
    );
}

#[test]
fn one_playlist_scope_exports_only_its_tracks() {
    let (d, snap) = setup("scope");
    let report = export(
        &d.dest,
        &narrowed(&snap, &["top"]),
        &ExportOptions::default(),
        None,
    )
    .unwrap();
    assert_eq!((report.tracks, report.playlists), (1, 1));
    let db = db(&d.dest);
    assert_eq!((db.tracks.len(), db.nodes.len()), (1, 1));
}

/// The export writes `exportLibrary.db` (here with an explicit key), which must say what `export.pdb` says (the real-USB twin of this
/// check is in `export_usb.rs`), plus the snapshot's colour and MyTag, the `b` cover copy, the settings files and a PQT2 in the
/// EXT file.
#[test]
fn onelibrary_matches_export_pdb_and_carries_snapshot_data() {
    let (d, mut snap) = setup("onelib");
    let cover = d.src.join("cover.jpg");
    std::fs::write(&cover, b"\xff\xd8 not a real jpeg \xff\xd9").unwrap();
    snap.tracks[0].0.artwork = Some(cover);
    snap.colors = vec![(0, 3)];
    snap.mytags = vec![("Mood".into(), vec!["Dark".into()])];
    snap.tagged = vec![(0, 0, 0)];
    let report = export(&d.dest, &snap, &ExportOptions::default(), Some("test-key")).unwrap();
    assert!(report.notes.is_empty(), "{:?}", report.notes);

    let one = d.dest.join("PIONEER/rekordbox/exportLibrary.db");
    let open = |key: &str| {
        let c = Connection::open_with_flags(&one, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        c.pragma_update(None, "key", key).unwrap();
        c
    };
    assert!(
        open("wrong")
            .query_row("select count(*) from content", [], |r| r.get::<_, i64>(0))
            .is_err()
    );
    let c = open("test-key");
    let rows = |sql: &str| {
        c.prepare(sql)
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let db = db(&d.dest);
    // The same tracks (by device id) with the same names, and the same playlists in the same order.
    let names = c
        .prepare("select c.title, coalesce(a.name, ''), c.content_id from content c left join album a using(album_id) order by c.content_id")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        names,
        db.tracks
            .iter()
            .map(|t| (t.title.clone(), t.album.clone(), i64::from(t.id)))
            .collect::<Vec<_>>()
    );
    assert_eq!(names[0].1, "Alpha Album");
    let lists =
        rows("select name, attribute, playlist_id_parent from playlist order by playlist_id");
    assert_eq!(
        lists
            .iter()
            .map(|l| (l.0.as_str(), l.1 == 1))
            .collect::<Vec<_>>(),
        [("Sets", true), ("Warm", false), ("Top", false)]
    );
    for n in &db.nodes {
        let ids: Vec<i64> = c
            .prepare("select content_id from playlist_content where playlist_id = ?1 order by sequenceNo")
            .unwrap()
            .query_map([i64::from(n.id)], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            ids,
            n.tracks.iter().map(|&t| i64::from(t)).collect::<Vec<_>>(),
            "order of {}",
            n.name
        );
    }
    assert_eq!(
        rows("select title, color_id, content_id from content order by content_id"),
        [("Alpha é".into(), 3, 1), ("Beta".into(), 0, 2)]
    );
    assert_eq!(
        rows("select name, attribute, myTag_id_parent from myTag order by myTag_id"),
        [("Mood".into(), 1, 0), ("Dark".into(), 0, 1)]
    );
    assert_eq!(
        rows("select 'x', myTag_id, content_id from myTag_content"),
        [("x".into(), 2, 1)]
    );
    assert_eq!(
        rows("select dbVersion, numberOfContents, backGroundColorType from property"),
        [("1000".into(), 2, 0)]
    );
    // Every path the database names exists under the destination; export.pdb names the cover a<N>.jpg, OneLibrary b<N>.jpg.
    let paths: Vec<(String, String, String)> = c
        .prepare("select c.path, c.analysisDataFilePath, i.path from content c join image i on i.image_id = c.image_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(paths.len(), 1);
    for p in [
        &paths[0].0,
        &paths[0].1,
        &paths[0].2,
        &paths[0].2.replace(".jpg", "_m.jpg"),
    ] {
        assert!(d.dest.join(p.trim_start_matches('/')).is_file(), "{p}");
    }
    assert!(paths[0].2.ends_with("/b1.jpg"));
    assert!(
        db.tracks[0]
            .artwork
            .as_deref()
            .unwrap()
            .ends_with("/a1.jpg")
    );
    assert!(db.tracks[1].artwork.is_none());

    for (name, len) in [
        ("MYSETTING.DAT", 148),
        ("MYSETTING2.DAT", 148),
        ("DJMMYSETTING.DAT", 160),
        ("DEVSETTING.DAT", 140),
    ] {
        assert_eq!(
            std::fs::metadata(d.dest.join("PIONEER").join(name))
                .unwrap()
                .len(),
            len,
            "{name}"
        );
    }
    assert!(report.fingerprint.is_some());
    // PQT2 in track 0's EXT: one entry per beat of the given grid.
    let dat = on_device(&d.dest, &db.tracks[0].analyze_path);
    let ext = std::fs::read(dat.with_extension("EXT")).unwrap();
    let at = ext
        .windows(4)
        .position(|w| w == b"PQT2")
        .expect("PQT2 section");
    assert_eq!(
        u32::from_be_bytes(ext[at + 40..at + 44].try_into().unwrap()) as usize,
        grid(24).len()
    );
}
