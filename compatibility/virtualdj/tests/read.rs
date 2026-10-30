//! `probe` / `open`: the round trip through this crate's own writer, a database as VirtualDJ writes it (other drive names,
//! cues without a type, flat keys, lists of both kinds), and the paths that are not a VirtualDJ library.

mod common;

use common::{snapshot, Dir};
use openfader_plugin::library::{Analysis, ExportOptions, LibraryPlugin, Source, Track};
use serde_json::json;
use std::path::Path;
use compat_virtualdj::song::camelot;
use compat_virtualdj::{Plugin, DATABASE};

fn open(path: &Path) -> Box<dyn Source> {
    Plugin::open(path, &json!({})).unwrap()
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn names(src: &dyn Source, list: Option<&str>) -> Vec<String> {
    src.tracks(list)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect()
}

#[test]
fn round_trip_of_the_writer() {
    let (src, stick) = (Dir::new("r-src"), Dir::new("r-stick"));
    let s = snapshot(&src);
    Plugin::export(&stick.0, &s, &ExportOptions::default(), &json!({})).unwrap();

    // The drive root, the VirtualDJ folder and the file itself open the same library.
    for p in [
        stick.0.clone(),
        stick.join("VirtualDJ"),
        stick.join(DATABASE),
    ] {
        let probe = Plugin::probe(&p).unwrap_or_else(|| panic!("{}", p.display()));
        assert_eq!((probe.score, probe.kind.as_str()), (100, "VirtualDJ"));
        assert_eq!(open(&p).tracks(None).unwrap().len(), 2);
    }

    let lib = open(&stick.0);
    let tracks = lib.tracks(None).unwrap();
    let a = &tracks[0];
    let want = &s.tracks[0].0;
    assert_eq!(
        (a.title.as_str(), a.artist.as_str(), a.album.as_str()),
        (want.title.as_str(), want.artist.as_str(), "Album")
    );
    assert_eq!((a.genre.as_str(), a.label.as_str()), ("House", "Lab\"el"));
    assert_eq!((a.key.as_str(), a.bpm_x100, a.length_s), ("Am", 12000, 90));
    assert_eq!((a.rating, a.bitrate), (4, 320));
    assert_eq!(a.comment, want.comment);
    assert_eq!(a.date_added, "2026-03-04");
    let file = a.file.as_ref().unwrap();
    assert!(file.starts_with(&stick.0) && file.is_file(), "{file:?}");
    assert_eq!(std::fs::metadata(file).unwrap().len(), 44 + 400);

    // Lists: folder, list in it, top level list; members in order.
    let lists = lib.playlists().unwrap();
    let shape: Vec<_> = lists
        .iter()
        .map(|p| (p.name.as_str(), p.depth, p.is_folder))
        .collect();
    assert_eq!(
        shape,
        [
            ("Root_List", 0, false),
            ("Sets", 0, true),
            ("Warm up", 1, false)
        ]
    );
    let id = |name: &str| lists.iter().find(|p| p.name == name).unwrap().id.as_str();
    assert_eq!(
        names(&*lib, Some(id("Warm up"))),
        [want.title.as_str(), "Plain"]
    );
    assert_eq!(names(&*lib, Some(id("Root_List"))), ["Plain"]);
    assert!(lib.tracks(Some("nope")).is_err());

    // Analysis: grid expanded over the track, cues and loops as written.
    let an = lib.analysis(a).unwrap();
    assert_eq!(an.key.as_deref(), Some("8A"));
    assert_eq!(an.beats.len(), 180);
    let first = an.beats[0];
    assert_eq!(
        (first.bar_pos, first.bpm_x100, first.time_ms),
        (1, 12000, 250)
    );
    assert_eq!((an.beats[1].bar_pos, an.beats[1].time_ms), (2, 750));
    assert_eq!(an.beats[4].bar_pos, 1);
    let mut got = an.cues.clone();
    got.sort_by_key(|c| c.time_ms);
    let mut expected = s.tracks[0].1.as_ref().unwrap().cues.clone();
    expected.sort_by_key(|c| c.time_ms);
    assert_eq!(got, expected);
    // A track without a grid has its key and nothing else.
    let plain = lib.analysis(&tracks[1]).unwrap();
    assert!(plain.beats.is_empty() && plain.cues.is_empty());
    assert_eq!(plain.key.as_deref(), Some("8A"));
}

const FOREIGN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<VirtualDJ_Database Version="8.5">
 <Song FilePath="C:\Music\Song One.mp3" FileSize="1">
  <Tags Author="Art" Bpm="0" Stars="9"/>
  <Infos SongLength="10.000000" FirstSeen="1772582400" Bitrate="256"/>
  <Comment>hi &amp; bye</Comment>
  <Scan Bpm="0.500000" Key="Bb" Flag="32768"/>
  <Poi Pos="1.000000" Type="beatgrid"/>
  <Poi Name="Cue 2" Pos="3.500000" Num="2"/>
  <Poi Name="Hook" Pos="4.000000" Num="3" Color="4294901760"/>
  <Poi Name="Intro" Type="loop" Pos="8.000000" Size="2.000000" Num="-1" Slot="1"/>
  <Poi Pos="2.000000" Type="automix" Point="cutStart"/>
 </Song>
 <Song FilePath="/Volumes/DJ/Tunes/x.mp3"><Tags Title="X" Stars="2"/></Song>
 <Song FilePath="Rel/y.mp3"/>
 <Song FilePath="/abs/z.mp3"/>
</VirtualDJ_Database>
"#;

#[test]
fn reads_a_database_as_virtualdj_writes_it() {
    let dir = Dir::new("f-db");
    write(&dir.0, DATABASE, FOREIGN);
    write(
        &dir.0,
        "VirtualDJ/MyLists/Folder/Mix.vdjfolder",
        r#"<VirtualFolder noDuplicates="no" ordered="yes">
 <song path="C:\Music\Song One.mp3" idx="1"/>
 <song path="C:\Elsewhere\gone.mp3" artist="Q" title="Gone" idx="0"/>
</VirtualFolder>"#,
    );
    write(
        &dir.0,
        "VirtualDJ/Playlists/Old.m3u",
        "#EXTM3U\n#EXTINF:1,x\nC:\\Music\\Song One.mp3\nsub\\q.mp3\n",
    );
    let lib = open(&dir.join(DATABASE));
    let tracks = lib.tracks(None).unwrap();
    assert_eq!(tracks.len(), 4);

    let one = &tracks[0];
    // No title: the file name. Tags Bpm 0 is no tempo: Scan's one is used. Stars are clamped.
    assert_eq!(
        (one.title.as_str(), one.artist.as_str()),
        ("Song One", "Art")
    );
    assert_eq!(
        (one.bpm_x100, one.rating, one.key.as_str()),
        (12000, 5, "Bb")
    );
    assert_eq!((one.length_s, one.bitrate), (10, 256));
    assert_eq!(
        (one.date_added.as_str(), one.comment.as_str()),
        ("2026-03-04", "hi & bye")
    );
    assert_eq!(tracks[1].rating, 2);

    #[cfg(unix)]
    {
        let file = |t: &Track| t.file.clone().unwrap();
        assert_eq!(file(one), dir.join("Music/Song One.mp3"));
        assert_eq!(file(&tracks[1]), dir.join("Tunes/x.mp3"));
        assert_eq!(file(&tracks[2]), dir.join("Rel/y.mp3"));
        assert_eq!(file(&tracks[3]), Path::new("/abs/z.mp3"));
    }

    let an: Analysis = lib.analysis(one).unwrap();
    assert_eq!(an.key.as_deref(), Some("6B"));
    // Grid: Scan Bpm from the beatgrid Poi at 1 s, back to 0 s (bar position 3) and on to the 10 s of the song.
    assert_eq!(an.beats.len(), 21);
    assert_eq!((an.beats[0].time_ms, an.beats[0].bar_pos), (0, 3));
    assert_eq!((an.beats[2].time_ms, an.beats[2].bar_pos), (1000, 1));
    assert_eq!(an.beats[20].time_ms, 10_000);
    // Cues: a typeless Poi is a cue, a default name is no comment, automix is no cue.
    let cue = |n: usize| &an.cues[n];
    assert_eq!(an.cues.len(), 3);
    assert_eq!(
        (cue(0).hot_cue, cue(0).time_ms, cue(0).comment.as_deref()),
        (2, 3500, None)
    );
    assert_eq!(
        (cue(1).hot_cue, cue(1).color, cue(1).comment.as_deref()),
        (3, Some([255, 0, 0]), Some("Hook"))
    );
    assert_eq!(
        (
            cue(2).hot_cue,
            cue(2).time_ms,
            cue(2).loop_end_ms,
            cue(2).comment.as_deref()
        ),
        (0, 8000, Some(9000), Some("Intro"))
    );
    assert!(lib.analysis(&tracks[2]).is_none());

    // Lists of both kinds; a song the database lacks comes from the list itself.
    let lists = lib.playlists().unwrap();
    let shape: Vec<_> = lists
        .iter()
        .map(|p| (p.name.as_str(), p.depth, p.is_folder))
        .collect();
    assert_eq!(
        shape,
        [("Folder", 0, true), ("Mix", 1, false), ("Old", 0, false)]
    );
    let mix = lib.tracks(Some(&lists[1].id)).unwrap();
    assert_eq!(
        mix.iter()
            .map(|t| (t.title.as_str(), t.artist.as_str()))
            .collect::<Vec<_>>(),
        [("Gone", "Q"), ("Song One", "Art")]
    );
    let old = lib.tracks(Some(&lists[2].id)).unwrap();
    assert_eq!(old[0].title, "Song One");
    assert_eq!(old[1].title, "q");
    #[cfg(unix)]
    assert_eq!(
        old[1].file.clone().unwrap(),
        dir.join("VirtualDJ/Playlists/sub/q.mp3")
    );
}

#[test]
fn analysis_ref_needs_no_open_handle() {
    let dir = Dir::new("f-ref");
    write(&dir.0, DATABASE, FOREIGN);
    let all = {
        let lib = open(&dir.join(DATABASE));
        lib.tracks(None)
            .unwrap()
            .into_iter()
            .map(|t| {
                let a = lib.analysis(&t);
                (t, a)
            })
            .collect::<Vec<_>>()
    };
    assert!(all[0].1.is_some());
    for (t, a) in all {
        let r = Plugin::analysis_ref(t.analysis_ref.as_deref().unwrap(), &json!({}));
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(&a).unwrap()
        );
    }
}

#[test]
fn an_absurd_length_makes_no_grid_instead_of_filling_memory() {
    let dir = Dir::new("f-huge");
    for length in ["1e300", "inf"] {
        write(
            &dir.0,
            DATABASE,
            &FOREIGN.replace(
                "SongLength=\"10.000000\"",
                &format!("SongLength=\"{length}\""),
            ),
        );
        let lib = open(&dir.join(DATABASE));
        let one = &lib.tracks(None).unwrap()[0];
        let an = lib.analysis(one).unwrap();
        assert!(an.beats.is_empty(), "{length}");
    }
}

#[test]
fn probe_says_no_to_anything_else() {
    let dir = Dir::new("n-no");
    assert!(Plugin::probe(&dir.0).is_none());
    assert!(Plugin::probe(&dir.join("missing.xml")).is_none());
    write(&dir.0, "other.xml", "<?xml version=\"1.0\"?><Other/>");
    write(&dir.0, "notes.txt", "VirtualDJ_Database");
    write(&dir.0, "VirtualDJ/MyLists/a.vdjfolder", "<VirtualFolder/>");
    for p in ["other.xml", "notes.txt", "VirtualDJ"] {
        assert!(Plugin::probe(&dir.join(p)).is_none(), "{p}");
    }
    assert!(Plugin::open(&dir.join("other.xml"), &json!({})).is_err());
    // Any name will do for a file that opens like a VirtualDJ database.
    write(&dir.0, "copy.xml", FOREIGN);
    assert!(Plugin::probe(&dir.join("copy.xml")).is_some());
}

#[test]
fn camelot_of_virtualdj_keys() {
    for (key, want) in [
        ("Am", Some("8A")),
        ("C", Some("8B")),
        ("Bbm", Some("3A")),
        ("Eb", Some("5B")),
        ("F#", Some("2B")),
        ("Cmin", Some("5A")),
        ("8a", Some("8A")),
        ("12B", Some("12B")),
        ("", None),
        ("H", None),
        ("Cmaj7", None),
    ] {
        assert_eq!(camelot(key).as_deref(), want, "{key}");
    }
}
