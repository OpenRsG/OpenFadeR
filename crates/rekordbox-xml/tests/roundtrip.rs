//! Snapshot → `write_collection` → `RekordboxXml` round trip on a synthetic library, and the reader's TEMPO / POSITION_MARK
//! analysis on synthetic XML (no real rekordbox.xml was available).

use openfader_plugin::library::{
    Analysis, Beat, Cue, LibraryPlugin, Playlist, Snapshot, Source, Track,
};
use rekordbox_xml::{RekordboxXml, RekordboxXmlPlugin};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn track(title: &str, file: &str, rating: u8) -> Track {
    Track {
        title: title.into(),
        artist: "Ärtist <&>".into(),
        album: "Ålbum \"1\" & <2>".into(),
        key: "8A".into(),
        genre: "G\"enre'".into(),
        label: "La<b>el & Co".into(),
        bpm_x100: 12000,
        length_s: 10,
        file: Some(PathBuf::from(file)),
        rating,
        comment: "line1\nline2 & more".into(),
        date_added: "2026-01-02".into(),
        bitrate: 320,
        sample_rate: 44100,
        ..Track::default()
    }
}

fn beat(bar_pos: u16, bpm_x100: u16, time_ms: u32) -> Beat {
    Beat {
        bar_pos,
        bpm_x100,
        time_ms,
    }
}

fn cue(
    hot_cue: u16,
    time_ms: u32,
    loop_end_ms: Option<u32>,
    color: Option<[u8; 3]>,
    comment: Option<&str>,
) -> Cue {
    Cue {
        hot_cue,
        time_ms,
        loop_end_ms,
        color,
        comment: comment.map(Into::into),
    }
}

fn pl(id: &str, name: &str, depth: usize, is_folder: bool) -> Playlist {
    Playlist {
        id: id.into(),
        name: name.into(),
        depth,
        is_folder,
    }
}

/// A fresh path in a private temp dir (the writer needs a file that does not exist).
fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("openfader-export-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("out.xml")
}

fn export(snap: &Snapshot, settings: &Value, name: &str) -> (PathBuf, String) {
    let p = temp(name);
    let n = RekordboxXmlPlugin::write_collection(&p, snap, settings).unwrap();
    assert_eq!(n, snap.tracks.len());
    let text = std::fs::read_to_string(&p).unwrap();
    (p, text)
}

fn open(p: &Path) -> RekordboxXml {
    RekordboxXml::open(p, &Vec::new()).unwrap()
}

/// A 3/4 grid (second tempo segment starting on beat 2) survives write → read as Metro 3/4 + Battito.
#[test]
fn three_four_time_round_trips() {
    let mut beats: Vec<Beat> = (0..6)
        .map(|k| beat(k % 3 + 1, 12000, u32::from(k) * 500))
        .collect();
    beats.extend((0..6).map(|k| beat((k + 1) % 3 + 1, 9000, 3000 + u32::from(k) * 667)));
    let snap = Snapshot {
        tracks: vec![(
            track("W", "/music/w.mp3", 0),
            Some(Analysis {
                beats,
                ..Default::default()
            }),
        )],
        ..Default::default()
    };
    let (p, text) = export(&snap, &json!({}), "waltz");
    assert!(
        text.contains("Inizio=\"0.000\" Bpm=\"120.00\" Metro=\"3/4\" Battito=\"1\""),
        "{text}"
    );
    assert!(
        text.contains("Inizio=\"3.000\" Bpm=\"90.00\" Metro=\"3/4\" Battito=\"2\""),
        "{text}"
    );
    let got = open(&p);
    let a = got.analysis(&got.tracks(None).unwrap()[0]).unwrap();
    let pos = |from: u32| {
        a.beats
            .iter()
            .filter(|b| b.time_ms >= from)
            .take(4)
            .map(|b| b.bar_pos)
            .collect::<Vec<_>>()
    };
    assert_eq!(pos(0), [1, 2, 3, 1]);
    assert_eq!(pos(3000), [2, 3, 1, 2]);
    let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle
}

#[test]
fn round_trip() {
    let tracks = vec![
        track("A&B <\"x\"> 'y'", "/music/ünï cödé/日本 #1 100%.mp3", 5),
        track("Plain", "/music/b.flac", 2),
    ];
    let analysis = Analysis {
        beats: vec![
            beat(1, 12000, 0),
            beat(2, 12000, 500),
            beat(3, 12000, 1000),
            beat(4, 12000, 1500),
            beat(3, 10000, 4000),
            beat(4, 10000, 4600),
        ],
        cues: vec![
            cue(0, 1500, None, None, None),
            cue(3, 2500, None, Some([1, 2, 3]), Some("drop & <go>")),
            cue(0, 3000, Some(5000), Some([255, 0, 128]), None),
            cue(1, 6000, Some(8000), None, None),
        ],
        ..Default::default()
    };
    let snap = Snapshot {
        tracks: vec![
            (tracks[0].clone(), Some(analysis.clone())),
            (tracks[1].clone(), None),
        ],
        playlists: vec![
            pl("f", "Set & <1>", 0, true),
            pl("p", "Inner \"P\"", 1, false),
            pl("q", "Top", 0, false),
        ],
        members: vec![vec![], vec![1, 0], vec![0]],
        colors: vec![(0, 3)],
        ..Default::default()
    };
    let (p, xml_text) = export(&snap, &json!({}), "rt");
    assert!(xml_text.contains("Location=\"file://localhost/music/%C3%BCn%C3%AF%20c%C3%B6d%C3%A9/%E6%97%A5%E6%9C%AC%20%231%20100%25.mp3\""));
    assert!(xml_text.contains("Colour=\"0xFFA500\""), "{xml_text}");
    let got = open(&p);

    let mut want = tracks.clone();
    want.sort_by(|a, b| a.title.cmp(&b.title));
    let mut read = got.tracks(None).unwrap();
    for (r, w) in read.iter_mut().zip(&want) {
        r.comment = r.comment.replace('\r', ""); // attribute values keep newlines via &#10;
        assert_eq!(r, w);
    }
    assert_eq!(read.len(), 2);

    // Folder nesting and playlist order survive.
    let pls = got.playlists().unwrap();
    let shape: Vec<_> = pls
        .iter()
        .map(|p| (p.name.as_str(), p.depth, p.is_folder))
        .collect();
    assert_eq!(
        shape,
        [
            ("Set & <1>", 0, true),
            ("Inner \"P\"", 1, false),
            ("Top", 0, false)
        ]
    );
    let titles = |i: usize| {
        got.tracks(Some(&pls[i].id))
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(titles(1), ["Plain", "A&B <\"x\"> 'y'"]);
    assert_eq!(titles(2), ["A&B <\"x\"> 'y'"]);

    // One TEMPO per tempo change; cues keep number, type, colour, loop end, name.
    let a = got
        .analysis(read.iter().find(|t| t.title.starts_with("A&B")).unwrap())
        .unwrap();
    let segs: Vec<_> = a
        .beats
        .iter()
        .scan(0, |prev, b| {
            Some(
                (std::mem::replace(prev, b.bpm_x100) != b.bpm_x100)
                    .then_some((b.time_ms, b.bpm_x100, b.bar_pos)),
            )
        })
        .flatten()
        .collect();
    assert_eq!(segs, [(0, 12000, 1), (4000, 10000, 3)]);
    assert_eq!(a.cues, analysis.cues);
    let plain = got.analysis(&read[1]).unwrap(); // no grid, no cues → no TEMPO/POSITION_MARK
    assert!(plain.beats.is_empty() && plain.cues.is_empty());
    let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle
}

/// `export_grid` off leaves the TEMPO elements out; cues stay. The writer never overwrites a file.
#[test]
fn export_grid_setting_and_no_overwrite() {
    let snap = Snapshot {
        tracks: vec![(
            track("G", "/music/g.mp3", 0),
            Some(Analysis {
                beats: vec![beat(1, 12000, 0)],
                cues: vec![cue(1, 1000, None, None, None)],
                ..Default::default()
            }),
        )],
        ..Default::default()
    };
    let (p, text) = export(&snap, &json!({"export_grid": false}), "nogrid");
    assert!(
        !text.contains("<TEMPO") && text.contains("<POSITION_MARK"),
        "{text}"
    );
    assert!(RekordboxXmlPlugin::write_collection(&p, &snap, &json!({})).is_err());
    let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle
}

#[test]
fn probe_wants_a_rekordbox_xml() {
    let p = temp("probe");
    std::fs::write(&p, "<DJ_PLAYLISTS Version=\"1.0.0\"/>").unwrap();
    let hit = RekordboxXmlPlugin::probe(&p).unwrap();
    assert_eq!((hit.score, hit.kind.as_str()), (90, "rekordbox.xml"));
    std::fs::write(&p, "<plist/>").unwrap();
    assert!(RekordboxXmlPlugin::probe(&p).is_none());
    let txt = p.with_extension("txt");
    std::fs::write(&txt, "<DJ_PLAYLISTS/>").unwrap();
    assert!(RekordboxXmlPlugin::probe(&txt).is_none());
    let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle
}

const XML: &str = r#"<DJ_PLAYLISTS Version="1.0.0"><COLLECTION Entries="2">
<TRACK TrackID="1" Name="T" AverageBpm="120" TotalTime="10" Location="file://localhost/tmp/rk-xml-a.mp3">
  <TEMPO Inizio="0.000" Bpm="120.00" Metro="4/4" Battito="1"/>
  <TEMPO Inizio="2.000" Bpm="60.00" Metro="3/4" Battito="2"/>
  <POSITION_MARK Name="Drop" Type="0" Start="1.25" Num="2"/>
  <POSITION_MARK Name="" Type="0" Start="3.5" Num="-1"/>
  <POSITION_MARK Name="" Type="4" Start="4.0" End="6.0" Num="0" Red="255" Green="0" Blue="128"/>
  <POSITION_MARK Name="" Type="1" Start="9.0" Num="-1"/>
</TRACK>
<TRACK TrackID="2" Name="Bare" TotalTime="10" Location="file://localhost/tmp/rk-xml-b.mp3"/>
</COLLECTION></DJ_PLAYLISTS>"#;

#[test]
fn tempo_list_and_marks_become_analysis() {
    let p = temp("analysis");
    std::fs::write(&p, XML).unwrap();
    let lib = open(&p);
    let tracks = lib.tracks(None).unwrap();
    let a = lib
        .analysis(tracks.iter().find(|t| t.title == "T").unwrap())
        .unwrap();

    // 120 bpm for 2 s (4 beats, 4/4), then 60 bpm in 3/4 starting on beat 2 until 10 s (8 beats).
    let got: Vec<_> = a
        .beats
        .iter()
        .map(|b| (b.time_ms, b.bar_pos, b.bpm_x100))
        .collect();
    let mut want = vec![
        (0, 1, 12000),
        (500, 2, 12000),
        (1000, 3, 12000),
        (1500, 4, 12000),
    ];
    want.extend((0..8).map(|i| (2000 + i * 1000, (1 + i as u16) % 3 + 1, 6000)));
    assert_eq!(got, want);

    let cue = |hot, ms| {
        a.cues
            .iter()
            .find(|c| c.hot_cue == hot && c.time_ms == ms)
            .unwrap()
    };
    assert_eq!(a.cues.len(), 3); // Type 1 (fade-in) is ignored
    let hot = cue(3, 1250); // Num 2 → hot cue C
    assert_eq!(
        (hot.comment.as_deref(), hot.color, hot.loop_end_ms),
        (Some("Drop"), None, None)
    );
    assert_eq!(cue(0, 3500).loop_end_ms, None); // Num -1 → memory cue
    let l = cue(1, 4000); // Num 0 → hot cue A, loop with colour
    assert_eq!((l.loop_end_ms, l.color), (Some(6000), Some([255, 0, 128])));
    assert!(a.preview.is_empty() && a.detail.is_empty() && a.color_detail.is_empty());

    // A track without TEMPO still has an (empty) analysis; an unknown track has none.
    let bare = lib
        .analysis(tracks.iter().find(|t| t.title == "Bare").unwrap())
        .unwrap();
    assert!(bare.beats.is_empty() && bare.cues.is_empty());
    let mut other = tracks[0].clone();
    other.file = None;
    assert!(lib.analysis(&other).is_none());
    let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle
}
