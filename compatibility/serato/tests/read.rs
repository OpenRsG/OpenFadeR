//! Reading: the round trip through the plugin's own writer (every container, cues, loops, grid, playlists in folders), a hand
//! built desktop layout, and the probe's yes and no cases.

mod common;

use common::*;
use openfader_plugin::library::{
    Cue, ExportOptions, LibraryPlugin, Playlist, Probe, Source, Track,
};
use compat_serato::{crate_file, database, Plugin};
use std::fs;
use std::path::{Path, PathBuf};

const SHARED: &str = "Contents/Artist/Album/";
const NULL: serde_json::Value = serde_json::Value::Null;

fn open(path: &Path) -> Box<dyn Source> {
    Plugin::open(path, &NULL).unwrap()
}

fn probe(path: &Path) -> Option<Probe> {
    Plugin::probe(path)
}

fn cue(hot_cue: u16, time_ms: u32, end: Option<u32>, color: [u8; 3], name: Option<&str>) -> Cue {
    Cue {
        hot_cue,
        time_ms,
        loop_end_ms: end,
        color: Some(color),
        comment: name.map(str::to_string),
    }
}

/// What [`analysis`] reads back as: Serato has no memory cues (dropped), the hot cue loop is a cue and a memory loop.
fn expected_cues(cue_ms: u32) -> Vec<Cue> {
    vec![
        cue(1, cue_ms, None, [1, 2, 3], Some("Drop")),
        cue(2, 30000, None, [0xcc, 0x88, 0], None),
        cue(0, 30000, Some(32000), [0x27, 0xaa, 0xe1], None),
    ]
}

fn export(usb: &Path, src: &Path, name: &str) {
    let mut t = track(src, "Song");
    t.label = "Lbl".into();
    t.comment = "Nice".into();
    let mut snap = snapshot(t, analysis(1500));
    snap.playlists = vec![
        playlist("Sets", 0, true),
        playlist(name, 1, false),
        playlist("Peak", 0, false),
    ];
    snap.members = vec![vec![], vec![0], vec![0]];
    Plugin::export(usb, &snap, &ExportOptions::default(), &NULL).unwrap();
}

fn assert_round_trip(file: &str, bytes: &[u8], rel: &str) {
    let d = tmp(&format!("read_{file}"));
    let (usb, src) = (d.join("usb"), d.join("lib").join(file));
    fs::write(&src, bytes).unwrap();
    export(&usb, &src, "Warm");

    let lib = open(&usb.join("_Serato_"));
    assert_eq!(lib.kind(), "Serato DJ");
    let tracks = lib.tracks(None).unwrap();
    let want = Track {
        title: "Song".into(),
        artist: "Artist".into(),
        album: "Album".into(),
        genre: "House".into(),
        label: "Lbl".into(),
        key: "Am".into(),
        comment: "Nice".into(),
        bpm_x100: 12000,
        length_s: 200,
        bitrate: 320,
        sample_rate: 44100,
        date_added: "2024-05-06".into(),
        file: Some(usb.join(rel)),
        ..Track::default()
    };
    let want = Track {
        analysis_ref: tracks[0].analysis_ref.clone(),
        ..want
    };
    assert_eq!(tracks, [want], "{file}");

    let a = lib
        .analysis(&tracks[0])
        .unwrap_or_else(|| panic!("{file}: no analysis"));
    assert_eq!(a.cues, expected_cues(1500), "{file}");
    assert_eq!(a.key.as_deref(), Some("8A"));
    assert_eq!(a.beats.len(), 400, "{file}: one beat per 500 ms over 200 s");
    assert_eq!(a.beats[..16], analysis(1500).beats[..], "{file}");
    let last = a.beats[399];
    assert_eq!(
        (last.bar_pos, last.bpm_x100, last.time_ms),
        (4, 12000, 199_750)
    );
}

#[test]
fn every_container_reads_back_what_the_export_wrote() {
    assert_round_trip("a.mp3", &mp3(4096), &format!("{SHARED}a.mp3"));
    assert_round_trip("a.flac", &flac(Some(4096)), &format!("{SHARED}a.flac"));
    assert_round_trip("a.m4a", &m4a(Some(8192)), &format!("{SHARED}a.m4a"));
    // No room for the tags in the shared copy: the library points at the private one.
    assert_round_trip("a.wav", &wav(), "_Serato_/Audio/Artist/Album/a.wav");
    assert_round_trip(
        "a.mp3",
        &mp3(ID3_TIGHT),
        "_Serato_/Audio/Artist/Album/a.mp3",
    );
}

#[test]
fn playlists_are_the_crate_tree_and_tracks_follow_the_crates() {
    let d = tmp("read_tree");
    let (usb, src) = (d.join("usb"), d.join("lib/a.mp3"));
    fs::write(&src, mp3(4096)).unwrap();
    export(&usb, &src, "Warm");
    let lib = open(&usb);

    let list: Vec<(String, String, usize, bool)> = lib
        .playlists()
        .unwrap()
        .into_iter()
        .map(|p: Playlist| (p.id, p.name, p.depth, p.is_folder))
        .collect();
    let row =
        |id: &str, name: &str, depth, folder| (id.to_string(), name.to_string(), depth, folder);
    assert_eq!(
        list,
        [
            row("Peak", "Peak", 0, false),
            row("Sets", "Sets", 0, true),
            row("Sets%%Warm", "Warm", 1, false),
        ]
    );
    assert_eq!(lib.tracks(Some("Sets%%Warm")).unwrap().len(), 1);
    assert_eq!(lib.tracks(Some("Peak")).unwrap()[0].title, "Song");
    assert!(lib.tracks(Some("Sets")).unwrap().is_empty());
    assert!(lib.tracks(Some("Nothing")).unwrap().is_empty());
    assert!(lib.tracks(Some("../x")).is_err());
    assert!(lib.analysis(&Track::default()).is_none());
}

#[test]
fn a_crate_with_crates_below_it_keeps_its_own_tracks_in_a_child() {
    let d = tmp("read_own");
    let (usb, src) = (d.join("usb"), d.join("lib/a.mp3"));
    fs::write(&src, mp3(4096)).unwrap();
    export(&usb, &src, "Warm");
    let sub = usb.join("_Serato_/Subcrates");
    let own = crate_file::build(&[&format!("{SHARED}a.mp3")]);
    fs::write(sub.join("Sets.crate"), own).unwrap();
    let lib = open(&usb);
    let list: Vec<(String, usize, bool)> = lib
        .playlists()
        .unwrap()
        .into_iter()
        .map(|p| (p.id, p.depth, p.is_folder))
        .collect();
    let row = |id: &str, depth, folder| (id.to_string(), depth, folder);
    assert_eq!(
        list,
        [
            row("Peak", 0, false),
            row("Sets", 0, true),
            row("Sets/", 1, false),
            row("Sets%%Warm", 1, false),
        ]
    );
    assert_eq!(lib.tracks(Some("Sets/")).unwrap()[0].title, "Song");
}

#[test]
fn analysis_ref_needs_no_open_library() {
    let d = tmp("read_ref");
    let (usb, src) = (d.join("usb"), d.join("lib/a.mp3"));
    fs::write(&src, mp3(4096)).unwrap();
    export(&usb, &src, "Warm");
    let (track, want) = {
        let lib = open(&usb);
        let t = lib.tracks(None).unwrap().remove(0);
        let a = lib.analysis(&t).expect("analysis");
        (t, a)
    };
    let got = Plugin::analysis_ref(track.analysis_ref.as_deref().unwrap(), &NULL);
    assert_eq!(
        serde_json::to_value(got.unwrap()).unwrap(),
        serde_json::to_value(want).unwrap()
    );
}

/// A library of a computer: the files are named from the drive root, above the folder holding `_Serato_`.
#[test]
fn a_desktop_library_finds_its_files_below_the_drive_root() {
    let d = tmp("read_desktop");
    let drive = d.join("drive");
    let music = drive.join("Users/me/Music");
    fs::create_dir_all(music.join("_Serato_/Subcrates")).unwrap();
    let mut tracks = vec![];
    for (name, ext) in [("one", "mp3"), ("two", "flac"), ("three", "ogg")] {
        let rel = format!("Users/me/Music/{name}.{ext}");
        fs::write(drive.join(&rel), audio(300)).unwrap();
        let t = Track {
            title: name.into(),
            bitrate: 256,
            sample_rate: 48000,
            length_s: 75,
            bpm_x100: 9950,
            ..Track::default()
        };
        tracks.push((rel.clone(), database::track(&rel, &t, 300, 1_700_000_000)));
    }
    let (db, _) = database::merge(None, &tracks, &[]).unwrap();
    fs::write(music.join(database::FILE), db).unwrap();
    // A leading slash and a member the database does not know.
    let crate_ = crate_file::build(&[
        "Users/me/Music/two.flac",
        "/Users/me/Music/one.mp3",
        "Users/me/Music/lost.mp3",
    ]);
    fs::write(music.join("_Serato_/Subcrates/Folder%%Mix.crate"), crate_).unwrap();

    let lib = open(&music.join("_Serato_/database V2"));
    let all = lib.tracks(None).unwrap();
    assert_eq!(
        all.iter()
            .map(|t| t.file.clone().unwrap())
            .collect::<Vec<PathBuf>>(),
        ["one.mp3", "two.flac", "three.ogg"].map(|n| music.join(n))
    );
    assert_eq!(
        (
            all[0].bitrate,
            all[0].sample_rate,
            all[0].length_s,
            all[0].bpm_x100
        ),
        (256, 48000, 75, 9950)
    );
    assert_eq!(all[0].date_added, "2023-11-14", "from uadd");

    let ids: Vec<String> = lib.playlists().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(ids, ["Folder", "Folder%%Mix"]);
    let mix = lib.tracks(Some("Folder%%Mix")).unwrap();
    let titles: Vec<&str> = mix.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(
        titles,
        ["two", "one", "lost"],
        "crate order; a member without a record gets its file name"
    );
    assert_eq!(
        mix[2].file,
        Some(music.join("Users/me/Music/lost.mp3")),
        "a missing file is placed below the folder of _Serato_"
    );

    // The ogg container is not read.
    assert!(lib.analysis(&all[2]).is_none());
}

#[test]
fn probe_claims_only_serato_libraries() {
    let d = tmp("read_probe");
    let usb = d.join("usb");
    let score = |p: &Path| probe(p).map(|p| (p.score, p.kind));
    assert_eq!(score(&usb), None);
    assert_eq!(score(&d.join("missing")), None);

    let src = d.join("lib/a.mp3");
    fs::write(&src, mp3(4096)).unwrap();
    export(&usb, &src, "Warm");
    let yes = |n| Some((n, "Serato DJ".to_string()));
    assert_eq!(score(&usb), yes(70));
    assert_eq!(score(&usb.join("_Serato_")), yes(95));
    assert_eq!(score(&usb.join("_Serato_/database V2")), yes(95));
    assert_eq!(score(&usb.join("_Serato_/Subcrates")), None);
    assert_eq!(score(&usb.join("_Serato_/Subcrates/Peak.crate")), None);
    assert_eq!(score(&usb.join("Contents")), None);

    // The right name without the right first bytes, and the right bytes under another name.
    let fake = d.join("fake/_Serato_");
    fs::create_dir_all(&fake).unwrap();
    fs::write(fake.join("database V2"), b"SQLite format 3\0").unwrap();
    assert_eq!(score(&fake), None);
    assert_eq!(score(&fake.join("database V2")), None);
    assert_eq!(score(&fake.parent().unwrap().to_path_buf()), None);
    let moved = d.join("moved");
    fs::copy(usb.join("_Serato_/database V2"), &moved).unwrap();
    assert_eq!(score(&moved), None);
    assert!(Plugin::open(&fake, &NULL).is_err());

    let caps = Plugin::capabilities(&NULL);
    assert_eq!((caps.hot_cues, caps.rating_max), (Some(8), 0));
}

#[test]
fn a_grid_with_a_tempo_change_expands_to_every_beat_and_damaged_data_is_bounded() {
    use openfader_plugin::library::Beat;
    use compat_serato::tag::markers::{beatgrid, grid_beats};
    let beat = |i: u16, bpm_x100, time_ms| Beat {
        bar_pos: i % 4 + 1,
        bpm_x100,
        time_ms,
    };
    // 8 beats at 120 BPM from 0, then 8 at 100 BPM from 4000 ms.
    let beats: Vec<Beat> = (0..16u16)
        .map(|i| match i {
            0..8 => beat(i, 12000, 500 * u32::from(i)),
            _ => beat(i, 10000, 4000 + 600 * u32::from(i - 8)),
        })
        .collect();
    let grid = beatgrid(&beats).unwrap();
    assert_eq!(
        grid_beats(&grid, 7600 + 600),
        beats.as_slice(),
        "ends with the track"
    );
    assert_eq!(
        grid_beats(&grid, 4000).len(),
        9,
        "the last marker's run is cut at the track end"
    );
    assert_eq!(
        grid_beats(&grid, 0).len(),
        9,
        "no length: only the last marker's own beat"
    );

    assert!(grid_beats(&[], 1000).is_empty());
    assert!(
        grid_beats(&[1, 0, 0, 0, 0, 5, 0, 0], 1000).is_empty(),
        "cut marker list"
    );
    // One marker at 0 s with BPM 1e-3 (NaN or absurd tempos are skipped, tiny ones give few beats).
    let one = |bpm: f32| {
        [
            &[1u8, 0, 0, 0, 0, 1][..],
            &0f32.to_be_bytes(),
            &bpm.to_be_bytes(),
            &[0],
        ]
        .concat()
    };
    assert_eq!(grid_beats(&one(f32::NAN), 60_000).len(), 0);
    assert_eq!(grid_beats(&one(-5.0), 60_000).len(), 0);
    assert_eq!(grid_beats(&one(0.001), 60_000).len(), 1);
    assert_eq!(grid_beats(&one(1e9), u32::MAX).len(), 100_000, "capped");
}
