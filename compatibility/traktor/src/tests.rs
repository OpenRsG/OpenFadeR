//! Export into a temp "stick": the written NML is parsed back with the crate's own reader and checked track by track, then
//! merged into, cancelled, refused and probed.

use super::*;
use openfader_plugin::library::{host::Host, Analysis, Beat, Buf, Cue, Playlist, Track};
use std::cell::Cell;
use std::path::PathBuf;

thread_local! {
    static CANCEL: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C" fn call(_req: *const u8, _len: usize) -> Buf {
    let out = format!("{{\"cancel\": {}}}", CANCEL.get())
        .into_bytes()
        .into_boxed_slice();
    let len = out.len();
    Buf {
        ptr: Box::into_raw(out).cast::<u8>(),
        len,
    }
}

unsafe extern "C" fn free(buf: Buf) {
    // SAFETY: `buf` is an answer of `call`, freed once.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.ptr, buf.len)) });
}

/// `export` as a job of a host that answers every progress report with `cancel`.
fn run(
    root: &Path,
    snap: &Snapshot,
    options: &ExportOptions,
    cancel: bool,
) -> Result<ExportReport, String> {
    CANCEL.set(cancel);
    host::serving(&Host { call, free }, || export(root, snap, options))
}

/// A fresh `<tmp>/traktor-<tag>-<pid>/STICK` (the folder name is the volume) and, beside it, a `src` folder for the audio.
fn dirs(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("traktor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("STICK")).unwrap();
    std::fs::create_dir_all(base.join("src")).unwrap();
    (base.join("STICK"), base.join("src"))
}

/// A 0.1 s mono 8 kHz 8-bit WAV; `seed` makes files of the same length differ.
fn wav(path: &Path, seed: u8) {
    let data = vec![seed; 800];
    let mut b = Vec::new();
    b.extend(b"RIFF");
    b.extend((36 + data.len() as u32).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend([1, 0, 1, 0]);
    b.extend(8000u32.to_le_bytes());
    b.extend(8000u32.to_le_bytes());
    b.extend([1, 0, 8, 0]);
    b.extend(b"data");
    b.extend((data.len() as u32).to_le_bytes());
    b.extend(data);
    std::fs::write(path, b).unwrap();
}

fn track(src: &Path, file: &str, title: &str, artist: &str, album: &str) -> Track {
    wav(&src.join(file), file.len() as u8);
    Track {
        title: title.into(),
        artist: artist.into(),
        album: album.into(),
        genre: "House".into(),
        bpm_x100: 12800,
        length_s: 1,
        bitrate: 320,
        rating: 4,
        comment: "a < b & \"c\"".into(),
        date_added: "2024-03-05".into(),
        file: Some(src.join(file)),
        ..Track::default()
    }
}

fn cue(hot: u16, ms: u32, end: Option<u32>, name: Option<&str>) -> Cue {
    Cue {
        hot_cue: hot,
        time_ms: ms,
        loop_end_ms: end,
        color: Some([1, 2, 3]),
        comment: name.map(Into::into),
    }
}

/// A 128 BPM grid whose first downbeat is the third beat (bar position 3, 4, 1, 2, ...).
fn analysis(cues: Vec<Cue>) -> Analysis {
    let beats = (0..8u32)
        .map(|i| Beat {
            bar_pos: ((i + 2) % 4 + 1) as u16,
            bpm_x100: 12800,
            time_ms: 100 + i * 469,
        })
        .collect();
    Analysis {
        beats,
        cues,
        key: Some("8A".into()),
        ..Analysis::default()
    }
}

fn list(name: &str, depth: usize, folder: bool) -> Playlist {
    Playlist {
        id: name.into(),
        name: name.into(),
        depth,
        is_folder: folder,
    }
}

fn read(root: &Path) -> El {
    xml::parse(&std::fs::read_to_string(root.join(FILE)).unwrap()).unwrap()
}

fn entries(nml: &El) -> Vec<&El> {
    nml.child("COLLECTION")
        .unwrap()
        .kids
        .iter()
        .filter(|e| e.name == "ENTRY")
        .collect()
}

/// The entry stored as `file` (`LOCATION FILE`).
fn entry_of<'a>(nml: &'a El, file: &str) -> &'a El {
    entries(nml)
        .into_iter()
        .find(|e| e.child("LOCATION").unwrap().get("FILE") == Some(file))
        .unwrap()
}

/// `(TYPE, HOTCUE, START, LEN, NAME)` of every CUE_V2 of an entry.
fn cues_of(e: &El) -> Vec<(String, i32, f64, f64, String)> {
    e.kids
        .iter()
        .filter(|k| k.name == "CUE_V2")
        .map(|c| {
            let f = |k: &str| c.get(k).unwrap().parse::<f64>().unwrap();
            (
                c.get("TYPE").unwrap().into(),
                f("HOTCUE") as i32,
                f("START"),
                f("LEN"),
                c.get("NAME").unwrap().into(),
            )
        })
        .collect()
}

/// The root folder's `SUBNODES` children.
fn root_nodes(nml: &El) -> &[El] {
    let root = &nml.child("PLAYLISTS").unwrap().kids[0];
    assert_eq!(root.get("NAME"), Some("$ROOT"));
    let subs = root.child("SUBNODES").unwrap();
    assert_eq!(
        subs.get("COUNT").unwrap().parse::<usize>().unwrap(),
        subs.kids.len()
    );
    &subs.kids
}

fn keys_of(node: &El) -> Vec<String> {
    let pl = node.child("PLAYLIST").unwrap();
    assert_eq!(
        pl.get("ENTRIES").unwrap().parse::<usize>().unwrap(),
        pl.kids.len()
    );
    pl.kids
        .iter()
        .map(|e| {
            e.child("PRIMARYKEY")
                .unwrap()
                .get("KEY")
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The `VOLUME + DIR + FILE` of an entry: what a playlist's PRIMARYKEY must equal.
fn key_of(e: &El) -> String {
    let l = e.child("LOCATION").unwrap();
    format!(
        "{}{}{}",
        l.get("VOLUME").unwrap(),
        l.get("DIR").unwrap(),
        l.get("FILE").unwrap()
    )
}

fn snap_ab(src: &Path) -> Snapshot {
    let a = track(src, "a.wav", "Alpha", "Art", "Alb");
    let b = track(src, "bb.wav", "Beta & Co", "Art", "Alb");
    let a_cues = vec![
        cue(1, 1000, None, Some("Drop")),
        cue(3, 5000, None, None),
        cue(2, 8000, Some(12000), Some("Loop")),
        cue(0, 2000, None, None),
        cue(0, 30000, Some(34000), None),
        cue(9, 9000, None, None),
    ];
    Snapshot {
        tracks: vec![(a, Some(analysis(a_cues))), (b, None)],
        playlists: vec![
            list("Sets", 0, true),
            list("Night", 1, false),
            list("Top", 0, false),
        ],
        members: vec![vec![], vec![1, 0], vec![0]],
        colors: vec![(0, 5)],
        ..Snapshot::default()
    }
}

fn audio_files(root: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("Contents"), &mut out);
    out.sort();
    out
}

#[test]
fn writes_a_collection_that_reads_back() {
    let (root, src) = dirs("write");
    let snap = snap_ab(&src);
    let rep = run(&root, &snap, &ExportOptions::default(), false).unwrap();
    assert_eq!(
        (rep.tracks, rep.added, rep.updated, rep.playlists),
        (2, 2, 0, 3)
    );
    assert!(
        rep.warnings.iter().any(|w| w.contains("hot cue 9")),
        "{:?}",
        rep.warnings
    );
    assert_eq!(audio_files(&root).len(), 2);
    assert!(root.join("Contents/Art/Alb/a.wav").is_file());
    assert!(!root.join("Traktor/collection.nml.part").exists());

    let nml = read(&root);
    assert_eq!((nml.name.as_str(), nml.get("VERSION")), ("NML", Some("19")));
    assert_eq!(nml.child("HEAD").unwrap().get("PROGRAM"), Some("Traktor"));
    let all = entries(&nml);
    assert_eq!(nml.child("COLLECTION").unwrap().get("ENTRIES"), Some("2"));
    assert_eq!(all.len(), 2);

    let a = entry_of(&nml, "a.wav");
    assert_eq!(
        (a.get("TITLE"), a.get("ARTIST")),
        (Some("Alpha"), Some("Art"))
    );
    assert_eq!(a.get("LOCK"), Some("1"));
    let loc = a.child("LOCATION").unwrap();
    assert_eq!(loc.get("DIR"), Some("/:Contents/:Art/:Alb/:"));
    assert_eq!(
        (loc.get("VOLUME"), loc.get("VOLUMEID")),
        (Some(volume(&root).as_str()), Some(volume(&root).as_str()))
    );
    assert_eq!(a.child("ALBUM").unwrap().get("TITLE"), Some("Alb"));
    let info = a.child("INFO").unwrap();
    assert_eq!(info.get("COMMENT"), Some("a < b & \"c\""));
    assert_eq!(
        [
            info.get("GENRE"),
            info.get("BITRATE"),
            info.get("RANKING"),
            info.get("PLAYTIME"),
            info.get("IMPORT_DATE")
        ],
        [
            Some("House"),
            Some("320000"),
            Some("204"),
            Some("1"),
            Some("2024/3/5")
        ]
    );
    assert_eq!(info.get("FILESIZE"), Some("1")); // 844 bytes round up to 1 KiB
    assert_eq!(
        (info.get("KEY"), info.get("COLOR")),
        (Some("1m"), Some("4"))
    );
    assert_eq!(a.child("TEMPO").unwrap().get("BPM"), Some("128.000000"));
    assert_eq!(a.child("MUSICAL_KEY").unwrap().get("VALUE"), Some("21"));

    // One grid marker at the first downbeat (the third beat, 100 + 2 x 469 ms); hot cues A..H are HOTCUE 0..7, memory -1.
    let cues = cues_of(a);
    let s = |t: &str, h: i32, start: f64, len: f64, name: &str| {
        (t.to_owned(), h, start, len, name.to_owned())
    };
    assert_eq!(
        cues,
        [
            s("4", -1, 1038.0, 0.0, "Beat Marker"),
            s("0", 0, 1000.0, 0.0, "Drop"),
            s("5", 1, 8000.0, 4000.0, "Loop"),
            s("0", 2, 5000.0, 0.0, "n.n."),
            s("0", -1, 2000.0, 0.0, "n.n."),
            s("5", -1, 30000.0, 4000.0, "n.n."),
        ]
    );

    // The entry without analysis has tags only.
    let b = entry_of(&nml, "bb.wav");
    assert_eq!(b.get("TITLE"), Some("Beta & Co"));
    assert!(cues_of(b).is_empty() && b.child("MUSICAL_KEY").is_none() && b.get("LOCK").is_none());
    assert!(b.child("INFO").unwrap().get("COLOR").is_none());

    // Playlists: the folder with its list inside, the top-level list, every key an entry of the collection.
    let top = root_nodes(&nml);
    assert_eq!(top.len(), 2);
    assert_eq!(
        (top[0].get("TYPE"), top[0].get("NAME")),
        (Some("FOLDER"), Some("Sets"))
    );
    let night = &top[0].child("SUBNODES").unwrap().kids[0];
    assert_eq!(
        (night.get("TYPE"), night.get("NAME")),
        (Some("PLAYLIST"), Some("Night"))
    );
    let (ka, kb) = (key_of(a), key_of(b));
    assert_eq!(ka, format!("{}/:Contents/:Art/:Alb/:a.wav", volume(&root)));
    assert_eq!(keys_of(night), [kb.clone(), ka.clone()]);
    assert_eq!(keys_of(&top[1]), [ka]);
    assert_eq!(
        night.child("PLAYLIST").unwrap().get("UUID").unwrap().len(),
        32
    );
}

#[test]
fn second_export_updates_in_place_and_keeps_the_playlists() {
    let (root, src) = dirs("update");
    let snap = snap_ab(&src);
    run(&root, &snap, &ExportOptions::default(), false).unwrap();
    let first = read(&root);
    let uuid = |n: &El| n.child("PLAYLIST").unwrap().get("UUID").unwrap().to_owned();
    let night_uuid = uuid(&root_nodes(&first)[0].child("SUBNODES").unwrap().kids[0]);

    // The host pushes one changed cue of track A: no playlists, one track.
    let mut changed = Snapshot {
        tracks: vec![snap.tracks[0].clone()],
        ..Snapshot::default()
    };
    changed.tracks[0].1.as_mut().unwrap().cues[0].time_ms = 3000;
    let rep = run(&root, &changed, &ExportOptions::default(), false).unwrap();
    assert_eq!(
        (rep.tracks, rep.added, rep.updated, rep.playlists),
        (1, 0, 1, 0)
    );
    assert_eq!(audio_files(&root).len(), 2, "no second audio copy");

    let nml = read(&root);
    assert_eq!(entries(&nml).len(), 2);
    let hot_a: Vec<_> = cues_of(entry_of(&nml, "a.wav"))
        .into_iter()
        .filter(|c| c.1 == 0)
        .collect();
    assert_eq!(hot_a.len(), 1);
    assert_eq!(hot_a[0].2, 3000.0);
    // The other entry and the playlists are what the first export wrote.
    assert_eq!(entry_of(&nml, "bb.wav"), entry_of(&first, "bb.wav"));
    assert_eq!(root_nodes(&nml), root_nodes(&first));
    assert_eq!(
        uuid(&root_nodes(&nml)[0].child("SUBNODES").unwrap().kids[0]),
        night_uuid
    );

    // Exporting the same snapshot again changes neither the track count nor the audio.
    run(&root, &snap, &ExportOptions::default(), false).unwrap();
    assert_eq!(entries(&read(&root)).len(), 2);
    assert_eq!(audio_files(&root).len(), 2);
}

#[test]
fn foreign_entries_and_playlists_survive_and_ours_are_replaced() {
    let (root, src) = dirs("merge");
    let foreign = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n\
<NML VERSION=\"20\"><HEAD COMPANY=\"www.native-instruments.com\" PROGRAM=\"Traktor Pro 4\"/><MUSICFOLDERS/>\n\
<COLLECTION ENTRIES=\"2\">\n\
<ENTRY MODIFIED_DATE=\"2020/1/2\" AUDIO_ID=\"AQID\" TITLE=\"Tom &amp; Jerry &apos;99\" ARTIST=\"Them\">\
<LOCATION DIR=\"/:Music/:\" FILE=\"x.mp3\" VOLUME=\"E:\" VOLUMEID=\"90556b77\"/>\
<INFO PLAYCOUNT=\"7\" KEY=\"12d\"/><MUSICAL_KEY VALUE=\"5\"/>\
<CUE_V2 NAME=\"AutoGrid\" DISPL_ORDER=\"0\" TYPE=\"4\" START=\"52.315876\" LEN=\"0.000000\" REPEATS=\"-1\" HOTCUE=\"0\"/></ENTRY>\n\
<ENTRY TITLE=\"Old\" ARTIST=\"Art\" AUDIO_ID=\"KEEP\" LOCK=\"1\">\
<LOCATION DIR=\"/:Contents/:Art/:Alb/:\" FILE=\"bb.wav\" VOLUME=\"E:\" VOLUMEID=\"E:\"/>\
<CUE_V2 NAME=\"mine\" DISPL_ORDER=\"0\" TYPE=\"0\" START=\"777.000000\" LEN=\"0.000000\" REPEATS=\"-1\" HOTCUE=\"4\"/></ENTRY>\n\
</COLLECTION><SETS ENTRIES=\"0\"/>\n\
<PLAYLISTS><NODE TYPE=\"FOLDER\" NAME=\"$ROOT\"><SUBNODES COUNT=\"3\">\
<NODE TYPE=\"PLAYLIST\" NAME=\"Mine\"><PLAYLIST ENTRIES=\"1\" TYPE=\"LIST\" UUID=\"abc\"><ENTRY><PRIMARYKEY TYPE=\"TRACK\" KEY=\"E:/:Music/:x.mp3\"/></ENTRY></PLAYLIST></NODE>\
<NODE TYPE=\"FOLDER\" NAME=\"Sets\"><SUBNODES COUNT=\"2\">\
<NODE TYPE=\"PLAYLIST\" NAME=\"Night\"><PLAYLIST ENTRIES=\"1\" TYPE=\"LIST\" UUID=\"keepme\"><ENTRY><PRIMARYKEY TYPE=\"TRACK\" KEY=\"E:/:Music/:x.mp3\"/></ENTRY></PLAYLIST></NODE>\
<NODE TYPE=\"PLAYLIST\" NAME=\"Other\"><PLAYLIST ENTRIES=\"0\" TYPE=\"LIST\" UUID=\"o\"></PLAYLIST></NODE>\
</SUBNODES></NODE>\
<NODE TYPE=\"PLAYLIST\" NAME=\"Top\"><PLAYLIST ENTRIES=\"0\" TYPE=\"LIST\" UUID=\"t\"></PLAYLIST></NODE>\
</SUBNODES></NODE></PLAYLISTS><INDEXING/></NML>";
    std::fs::create_dir_all(root.join("Traktor")).unwrap();
    std::fs::write(root.join(FILE), foreign).unwrap();
    let before = xml::parse(foreign).unwrap();

    let snap = snap_ab(&src);
    let rep = run(&root, &snap, &ExportOptions::default(), false).unwrap();
    // bb.wav has the same folder and file as the foreign second entry (another volume): replaced, not duplicated.
    assert_eq!((rep.added, rep.updated), (1, 1));
    let nml = read(&root);
    assert_eq!(nml.get("VERSION"), Some("20"));
    assert_eq!(nml.child("HEAD"), before.child("HEAD"));
    assert_eq!(nml.child("COLLECTION").unwrap().get("ENTRIES"), Some("3"));
    assert_eq!(entries(&nml).len(), 3);

    // The foreign entry is untouched down to its attributes.
    assert_eq!(entry_of(&nml, "x.mp3"), entry_of(&before, "x.mp3"));
    assert_eq!(
        entry_of(&nml, "x.mp3").get("TITLE"),
        Some("Tom & Jerry '99")
    );
    // The replaced one has no analysis in the snapshot: Traktor's data and cues stay, the volume is ours now.
    let b = entry_of(&nml, "bb.wav");
    assert_eq!(
        (b.get("AUDIO_ID"), b.get("LOCK"), b.get("TITLE")),
        (Some("KEEP"), Some("1"), Some("Beta & Co"))
    );
    assert_eq!(
        b.child("LOCATION").unwrap().get("VOLUME"),
        Some(volume(&root).as_str())
    );
    assert_eq!(cues_of(b).len(), 1);
    assert_eq!(cues_of(b)[0].2, 777.0);

    // Playlists: "Mine", "Other" and the foreign "Top" content are kept, "Night" and ours replaced, no duplicate nodes.
    let top = root_nodes(&nml);
    assert_eq!(
        top.iter()
            .map(|n| n.get("NAME").unwrap())
            .collect::<Vec<_>>(),
        ["Mine", "Sets", "Top"]
    );
    assert_eq!(keys_of(&top[0]), ["E:/:Music/:x.mp3"]);
    let sets = top[1].child("SUBNODES").unwrap();
    assert_eq!(sets.get("COUNT"), Some("2"));
    assert_eq!(sets.kids[1].get("NAME"), Some("Other"));
    let night = &sets.kids[0];
    assert_eq!(night.child("PLAYLIST").unwrap().get("UUID"), Some("keepme"));
    assert_eq!(keys_of(night), [key_of(b), key_of(entry_of(&nml, "a.wav"))]);
    assert_eq!(keys_of(&top[2]), [key_of(entry_of(&nml, "a.wav"))]);
}

#[test]
fn a_file_that_is_no_collection_is_never_overwritten() {
    let (root, src) = dirs("broken");
    std::fs::create_dir_all(root.join("Traktor")).unwrap();
    for bad in ["<NML><COLLECTION>", "<html><body/></html>", "not xml"] {
        std::fs::write(root.join(FILE), bad).unwrap();
        let err = run(&root, &snap_ab(&src), &ExportOptions::default(), false).unwrap_err();
        assert!(
            err.contains("cannot be read") && err.contains("stays as it is"),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(root.join(FILE)).unwrap(), bad);
    }
}

#[test]
fn cancel_and_a_full_stick_leave_the_stick_as_it_was() {
    let (root, src) = dirs("cancel");
    let snap = snap_ab(&src);
    assert_eq!(
        run(&root, &snap, &ExportOptions::default(), true).unwrap_err(),
        "cancelled"
    );
    assert!(!root.join(FILE).exists());

    let tight = ExportOptions {
        free: Some(10),
        ..ExportOptions::default()
    };
    let err = run(&root, &snap, &tight, false).unwrap_err();
    assert!(err.contains("Not enough space"), "{err}");
    assert!(!root.join("Contents").exists() && !root.join(FILE).exists());

    // A cancel after a first export keeps that file.
    run(&root, &snap, &ExportOptions::default(), false).unwrap();
    let written = std::fs::read(root.join(FILE)).unwrap();
    assert_eq!(
        run(&root, &snap, &ExportOptions::default(), true).unwrap_err(),
        "cancelled"
    );
    assert_eq!(std::fs::read(root.join(FILE)).unwrap(), written);
}

#[test]
fn a_track_without_audio_is_skipped_with_a_warning() {
    let (root, src) = dirs("missing");
    let mut snap = snap_ab(&src);
    snap.tracks[1].0.file = Some(src.join("gone.wav"));
    let rep = run(&root, &snap, &ExportOptions::default(), false).unwrap();
    assert_eq!(
        (
            rep.tracks,
            rep.warnings
                .iter()
                .filter(|w| w.contains("gone.wav"))
                .count()
        ),
        (1, 1)
    );
    let nml = read(&root);
    assert_eq!(entries(&nml).len(), 1);
    // The playlist that listed it keeps only the written track.
    assert_eq!(
        keys_of(&root_nodes(&nml)[0].child("SUBNODES").unwrap().kids[0]),
        [key_of(entry_of(&nml, "a.wav"))]
    );
}

#[test]
fn the_device_probe_sees_the_library_it_wrote() {
    let (root, src) = dirs("probe");
    let before = Plugin::device_probe(&root, &Value::Null).unwrap();
    assert_eq!(
        (before.libraries.len(), before.can_write, before.modified),
        (0, true, None)
    );
    assert_eq!(
        before.features,
        [Feature::Export, Feature::Plan, Feature::Check]
    );
    run(&root, &snap_ab(&src), &ExportOptions::default(), false).unwrap();
    let after = Plugin::device_probe(&root, &Value::Null).unwrap();
    assert_eq!(after.libraries, [LIBRARY]);
    assert!(after.modified.unwrap() > 1_600_000_000);
    assert!(
        !Plugin::device_probe(&root.join("nope"), &Value::Null)
            .unwrap()
            .can_write
    );
    let nope = root.join("nope");
    assert!(Plugin::probe(&nope).is_none() && Plugin::open(&nope, &Value::Null).is_err());
}

#[test]
fn the_volume_is_the_drive_letter_or_the_mount_folder() {
    assert_eq!(volume(Path::new("/run/media/dj/KINGSTON")), "KINGSTON");
    assert_eq!(volume(Path::new("/Volumes/My Stick/")), "My Stick");
    assert_eq!(volume(Path::new("E:\\")), "E:");
    assert_eq!(volume(Path::new("e:/")), "E:");
    assert_eq!(volume(Path::new("/")), "USB");
}

#[test]
fn the_reader_reads_back_what_the_writer_wrote() {
    let (root, src) = dirs("reader");
    run(&root, &snap_ab(&src), &ExportOptions::default(), false).unwrap();
    let file = root.join(FILE);
    assert_eq!(
        Plugin::probe(&root).map(|p| (p.score, p.kind)),
        Some((70, "Traktor".into()))
    );
    assert_eq!(Plugin::probe(&file).map(|p| p.score), Some(95));
    let txt = src.join("x.txt");
    std::fs::write(&txt, "hello").unwrap();
    assert!(Plugin::probe(&txt).is_none() && Plugin::probe(&src).is_none());
    let bad = src.join("bad.nml");
    std::fs::write(&bad, "<other/>").unwrap();
    assert!(Plugin::probe(&bad).is_none());

    for p in [&root, &file] {
        let s = Plugin::open(p, &Value::Null).unwrap();
        let all = s.tracks(None).unwrap();
        assert_eq!(all.len(), 2);
        let a = all.iter().find(|t| t.title == "Alpha").unwrap();
        assert_eq!(
            (
                a.artist.as_str(),
                a.album.as_str(),
                a.genre.as_str(),
                a.key.as_str()
            ),
            ("Art", "Alb", "House", "1m")
        );
        assert_eq!(
            (
                a.bpm_x100,
                a.rating,
                a.bitrate,
                a.length_s,
                a.comment.as_str(),
                a.date_added.as_str()
            ),
            (12800, 4, 320, 1, "a < b & \"c\"", "2024-03-05")
        );
        assert_eq!(
            a.file.as_deref(),
            Some(root.join("Contents/Art/Alb/a.wav").as_path())
        );
        assert!(a.file.as_ref().unwrap().is_file());

        let lists = s.playlists().unwrap();
        let l: Vec<_> = lists
            .iter()
            .map(|p| (p.id.as_str(), p.depth, p.is_folder))
            .collect();
        assert_eq!(
            l,
            [
                ("Sets", 0, true),
                ("Sets/Night", 1, false),
                ("Top", 0, false)
            ]
        );
        let night: Vec<_> = s
            .tracks(Some("Sets/Night"))
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(night, ["Beta & Co", "Alpha"]);
        assert_eq!(s.tracks(Some("Top")).unwrap().len(), 1);

        let an = s.analysis(a).unwrap();
        assert_eq!(an.key.as_deref(), Some("8A"));
        let bars: Vec<_> = an.beats.iter().map(|b| b.bar_pos).collect();
        assert_eq!(bars, [3, 4, 1]);
        for (b, ms) in an.beats.iter().zip([100, 569, 1038]) {
            assert!(b.time_ms.abs_diff(ms) <= 1 && b.bpm_x100 == 12800, "{b:?}");
        }
        let mut cues: Vec<_> = an
            .cues
            .iter()
            .map(|c| (c.hot_cue, c.time_ms, c.loop_end_ms, c.comment.clone()))
            .collect();
        cues.sort_by_key(|c| (c.0, c.1));
        assert_eq!(
            cues,
            [
                (0, 2000, None, None),
                (0, 30000, Some(34000), None),
                (1, 1000, None, Some("Drop".into())),
                (2, 8000, Some(12000), Some("Loop".into())),
                (3, 5000, None, None),
            ]
        );
        let b = all.iter().find(|t| t.title.starts_with("Beta")).unwrap();
        assert!(s.analysis(b).is_none());
    }
}

#[test]
fn analysis_ref_needs_no_open_handle() {
    let (root, src) = dirs("ref");
    run(&root, &snap_ab(&src), &ExportOptions::default(), false).unwrap();
    let all = {
        let s = Plugin::open(&root, &Value::Null).unwrap();
        s.tracks(None)
            .unwrap()
            .into_iter()
            .map(|t| {
                let a = s.analysis(&t);
                (t, a)
            })
            .collect::<Vec<_>>()
    };
    assert!(all.iter().any(|(_, a)| a.is_some()));
    for (t, a) in all {
        let r = Plugin::analysis_ref(t.analysis_ref.as_deref().unwrap(), &Value::Null);
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(&a).unwrap()
        );
    }
}

#[test]
fn absurd_tempo_or_length_makes_no_grid() {
    let entry = |bpm: &str, playtime: &str| {
        El::new("ENTRY")
            .with(El::new("TEMPO").attr("BPM", bpm))
            .with(El::new("INFO").attr("PLAYTIME_FLOAT", playtime))
            .with(
                El::new("CUE_V2")
                    .attr("TYPE", "4")
                    .attr("START", "1000")
                    .attr("LEN", "0")
                    .attr("HOTCUE", "0"),
            )
    };
    assert_eq!(
        read::entry_analysis(&entry("128", "10"))
            .unwrap()
            .beats
            .len(),
        22
    );
    for (bpm, len) in [
        ("1e300", "10"),
        ("128", "1e300"),
        ("inf", "10"),
        ("128", "inf"),
        ("NaN", "10"),
    ] {
        let a = read::entry_analysis(&entry(bpm, len));
        assert!(a.is_none_or(|a| a.beats.is_empty()), "{bpm} {len}");
    }
}
