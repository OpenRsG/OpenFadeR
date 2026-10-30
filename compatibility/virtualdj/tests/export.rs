//! Snapshot → `export` → `VirtualDJ/database.xml` and lists on a synthetic stick, read back with the crate's XML parser and as
//! plain text; a second export, a database VirtualDJ wrote, and the failures that must leave the stick alone.

mod common;

use common::{analysis, cue, playlist, snapshot, track, Dir};
use openfader_plugin::library::{ExportOptions, LibraryPlugin, Snapshot};
use serde_json::json;
use std::path::Path;
use compat_virtualdj::song::{identity, key_text, Style};
use compat_virtualdj::xml::{self, El};
use compat_virtualdj::{Plugin, DATABASE, LIBRARY};

fn export(root: &Path, s: &Snapshot) -> openfader_plugin::library::ExportReport {
    Plugin::export(root, s, &ExportOptions::default(), &json!({})).unwrap()
}

fn songs(root: &Path) -> Vec<El> {
    let text = std::fs::read_to_string(root.join(DATABASE)).unwrap();
    let doc = xml::split(&text).unwrap();
    doc.items
        .iter()
        .filter(|i| i.file_path.is_some())
        .map(|i| xml::parse(&i.raw).unwrap())
        .collect()
}

fn child<'a>(e: &'a El, name: &'a str) -> &'a El {
    e.children(name)
        .next()
        .unwrap_or_else(|| panic!("no <{name}> in {e:?}"))
}

fn pois(song: &El) -> Vec<&El> {
    song.children("Poi").collect()
}

fn poi<'a>(song: &'a El, key: &str, val: &str) -> &'a El {
    pois(song)
        .into_iter()
        .find(|p| p.attr(key) == Some(val))
        .unwrap_or_else(|| panic!("no Poi {key}={val}"))
}

fn list_songs(path: &Path) -> Vec<(String, String)> {
    let list = xml::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(list.name, "VirtualFolder");
    list.children("song")
        .map(|s| {
            (
                s.attr("path").unwrap().to_owned(),
                s.attr("idx").unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn writes_songs_cues_grid_and_lists() {
    let (src, stick) = (Dir::new("w-src"), Dir::new("w-stick"));
    let s = snapshot(&src);
    let r = export(&stick.0, &s);
    assert_eq!((r.tracks, r.added, r.updated, r.playlists), (2, 2, 0, 3));
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);

    let songs = songs(&stick.0);
    assert_eq!(songs.len(), 2);
    let a = &songs[0];
    // FilePath: drive style, audio really below the root.
    let fp = a.attr("FilePath").unwrap();
    // The drive of the stick on Windows, `E:` where the root has no drive letter.
    let drive = if cfg!(windows) {
        stick.0.to_string_lossy()[..2].to_owned()
    } else {
        "E:".into()
    };
    assert!(fp.starts_with(&format!("{drive}\\Contents\\")), "{fp}");
    assert!(stick.0.join(fp[3..].replace('\\', "/")).is_file(), "{fp}");
    assert_eq!(a.attr("FileSize"), Some((44 + 400).to_string().as_str()));
    // Escaping round trip, and the raw text really escaped.
    let tags = child(a, "Tags");
    assert_eq!(tags.attr("Title"), Some("Title \"A\" & <1> 'x'\nb"));
    assert_eq!(tags.attr("Author"), Some("Art & <Ist>"));
    assert_eq!(tags.attr("Label"), Some("Lab\"el"));
    let text = std::fs::read_to_string(stick.join(DATABASE)).unwrap();
    assert!(text.starts_with(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<VirtualDJ_Database Version=\"8.5\">"
    ));
    assert!(text.contains("Author=\"Art &amp; &lt;Ist&gt;\""), "{text}");
    assert!(text.contains("Label=\"Lab&quot;el\"") && text.contains("&#10;b\""));
    assert!(text.contains("<Comment>line one\nline two &amp; &lt;more&gt;</Comment>"));
    assert_eq!(child(a, "Comment").text(), "line one\nline two & <more>");
    // Bpm is seconds per beat.
    assert_eq!(tags.attr("Bpm"), Some("0.500000"));
    assert_eq!(tags.attr("Key"), Some("Am"));
    assert_eq!(tags.attr("Stars"), Some("4"));
    let scan = child(a, "Scan");
    assert_eq!(
        (scan.attr("Bpm"), scan.attr("Key"), scan.attr("Phase")),
        (Some("0.500000"), Some("Am"), Some("0.250000"))
    );
    let infos = child(a, "Infos");
    assert_eq!(
        (infos.attr("SongLength"), infos.attr("Bitrate")),
        (Some("90.000000"), Some("320"))
    );
    assert_eq!(infos.attr("FirstSeen"), Some("1772582400")); // 2026-03-04 UTC
                                                             // Poi: grid anchor, hot cues in seconds with ARGB colour, memory cue, loops.
    assert_eq!(poi(a, "Type", "beatgrid").attr("Pos"), Some("0.250000"));
    let c1 = poi(a, "Num", "1");
    assert_eq!(
        (
            c1.attr("Pos"),
            c1.attr("Name"),
            c1.attr("Color"),
            c1.attr("Type")
        ),
        (
            Some("10.000000"),
            Some("Drop"),
            Some("4294901760"),
            None
        )
    );
    let l3 = poi(a, "Num", "3");
    assert_eq!(
        (
            l3.attr("Type"),
            l3.attr("Pos"),
            l3.attr("Size"),
            l3.attr("Slot"),
            l3.attr("Color")
        ),
        (
            Some("loop"),
            Some("20.000000"),
            Some("4.000000"),
            Some("3"),
            Some("4278255360")
        )
    );
    let mem = poi(a, "Pos", "5.000000");
    assert_eq!(
        (mem.attr("Num"), mem.attr("Type"), mem.attr("Color")),
        (Some("-1"), None, None)
    );
    let mloop = poi(a, "Pos", "30.000000");
    assert_eq!(
        (
            mloop.attr("Type"),
            mloop.attr("Size"),
            mloop.attr("Slot"),
            mloop.attr("Num")
        ),
        (Some("loop"), Some("4.000000"), Some("1"), Some("-1"))
    );
    assert_eq!(pois(a).len(), 5);
    // A track without analysis: tags and tempo only.
    let b = &songs[1];
    assert!(pois(b).is_empty());
    assert_eq!(child(b, "Scan").attr("Phase"), None);
    assert_eq!(child(b, "Tags").attr("Bpm"), Some("0.500000"));

    // Lists: folder as folder, one file per list, members in order with the database's paths.
    let lists = stick.join("VirtualDJ/MyLists");
    let warm = list_songs(&lists.join("Sets/Warm up.vdjfolder"));
    let paths: Vec<_> = songs
        .iter()
        .map(|s| s.attr("FilePath").unwrap().to_owned())
        .collect();
    assert_eq!(
        warm,
        vec![
            (paths[0].clone(), "0".into()),
            (paths[1].clone(), "1".into())
        ]
    );
    assert_eq!(
        list_songs(&lists.join("Root_List.vdjfolder")),
        vec![(paths[1].clone(), "0".into())]
    );
    assert!(!stick.join("VirtualDJ/Folders").exists());

    let info = Plugin::device_probe(&stick.0, &json!({})).unwrap();
    assert_eq!(info.libraries, vec![LIBRARY.to_owned()]);
    assert!(info.can_write && info.modified.is_some());
}

#[test]
fn second_export_refreshes_in_place_and_keeps_what_virtualdj_added() {
    let (src, stick) = (Dir::new("u-src"), Dir::new("u-stick"));
    let s = snapshot(&src);
    export(&stick.0, &s);
    let contents_before = file_count(&stick.join("Contents"));

    // VirtualDJ plays the song and adds an automix point, a play count and another song of its own.
    let db = stick.join(DATABASE);
    let text = std::fs::read_to_string(&db).unwrap();
    let text = text
        .replacen("<Infos ", "<Infos PlayCount=\"7\" ", 1)
        .replacen(
            "</Song>",
            "<Poi Pos=\"1.5\" Type=\"automix\" Point=\"cutStart\"/>\n </Song>",
            1,
        );
    let foreign = " <Song FilePath=\"E:\\Music\\other.mp3\" FileSize=\"5\" Flag=\"2\">\n  <Tags Author=\"X\" Foo=\"bar\"/>\n  <CustomMix>keep</CustomMix>\n </Song>";
    let text = text.replace(
        "</VirtualDJ_Database>",
        &format!("{foreign}\n</VirtualDJ_Database>"),
    );
    std::fs::write(&db, text).unwrap();

    // The host pushes one track with one moved cue and no playlists.
    let mut one = Snapshot {
        tracks: vec![s.tracks[0].clone()],
        ..Snapshot::default()
    };
    one.tracks[0].1 = Some(analysis(vec![cue(1, 12_000, None, None, None)]));
    one.tracks[0].0.rating = 5;
    let r = export(&stick.0, &one);
    assert_eq!((r.added, r.updated, r.playlists), (0, 1, 0));

    let text = std::fs::read_to_string(&db).unwrap();
    assert!(text.contains(foreign), "foreign song changed:\n{text}");
    let songs = songs(&stick.0);
    assert_eq!(songs.len(), 3);
    let a = &songs[0];
    assert_eq!(
        pois(a)
            .iter()
            .filter(|p| p.attr("Num") == Some("1"))
            .count(),
        1
    );
    assert_eq!(poi(a, "Num", "1").attr("Pos"), Some("12.000000"));
    assert!(
        pois(a).iter().all(|p| p.attr("Num") != Some("3")),
        "old loop is gone"
    );
    assert_eq!(poi(a, "Type", "automix").attr("Point"), Some("cutStart"));
    assert_eq!(child(a, "Infos").attr("PlayCount"), Some("7"));
    assert_eq!(child(a, "Tags").attr("Stars"), Some("5"));
    assert_eq!(
        file_count(&stick.join("Contents")),
        contents_before,
        "audio copied again"
    );
    // The lists of the first export are still there.
    assert_eq!(
        list_songs(&stick.join("VirtualDJ/MyLists/Sets/Warm up.vdjfolder")).len(),
        2
    );
}

fn file_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| if p.is_dir() { file_count(&p) } else { 1 })
        .sum()
}

#[test]
fn a_database_from_virtualdj_keeps_its_text_and_its_drive_style() {
    let (src, stick) = (Dir::new("f-src"), Dir::new("f-stick"));
    let s = Snapshot {
        tracks: vec![(track(&src, "a.wav", "New", 300), None)],
        ..Snapshot::default()
    };
    // Where the audio of the track will be: the stick holds the song already under another drive letter and case.
    let probe = Dir::new("f-probe");
    export(&probe.0, &s);
    let rel = identity(songs(&probe.0)[0].attr("FilePath").unwrap());
    let old_path = format!("G:{}", rel.replace('/', "\\").to_uppercase());
    let foreign_song = " <Song FilePath=\"G:\\Music\\a &amp; b.mp3\" FileSize=\"9\" Flag=\"1\">\n  <Tags Author=\"A &amp; B\" Unknown=\"1\"/>\n  <Infos PlayCount=\"3\"/>\n  <Poi Pos=\"1\" Type=\"remix\"/>\n </Song>";
    let ours = format!(
        " <Song FilePath=\"{old_path}\" FileSize=\"1\">\n  <Infos PlayCount=\"9\"/>\n </Song>"
    );
    let text = format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<!-- mine -->\r\n<VirtualDJ_Database Version=\"8.5\">\r\n <FavoriteFolder Path=\"G:\\Music\"/>\r\n{foreign_song}\r\n{ours}\r\n</VirtualDJ_Database>\r\n"
    );
    std::fs::create_dir_all(stick.join("VirtualDJ")).unwrap();
    std::fs::write(stick.join(DATABASE), &text).unwrap();
    // The audio is copied now; its song is found by the path without the drive letter, whatever the case.
    let r = export(&stick.0, &s);
    assert_eq!(
        (r.added, r.updated),
        (0, 1),
        "same file under another letter is one song"
    );

    let out = std::fs::read_to_string(stick.join(DATABASE)).unwrap();
    assert!(out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<!-- mine -->\r\n<VirtualDJ_Database Version=\"8.5\">\r\n <FavoriteFolder Path=\"G:\\Music\"/>\r\n"), "{out:?}");
    assert!(out.contains(foreign_song));
    let songs = songs(&stick.0);
    assert_eq!(songs.len(), 2);
    assert_eq!(
        songs[1].attr("FilePath"),
        Some(old_path.as_str()),
        "the vendor's FilePath is kept"
    );
    assert_eq!(child(&songs[1], "Infos").attr("PlayCount"), Some("9"));
    assert_eq!(child(&songs[1], "Tags").attr("Title"), Some("New"));

    // A track the database does not know takes the drive letter of the database.
    let s2 = Snapshot {
        tracks: vec![(track(&src, "c.wav", "Other", 333), None)],
        ..Snapshot::default()
    };
    export(&stick.0, &s2);
    let fp = songs_last(&stick.0);
    assert!(fp.starts_with("G:\\Contents\\"), "{fp}");
}

fn songs_last(root: &Path) -> String {
    songs(root)
        .last()
        .unwrap()
        .attr("FilePath")
        .unwrap()
        .to_owned()
}

#[test]
fn older_folders_layout_and_macos_volumes() {
    let (src, stick) = (Dir::new("o-src"), Dir::new("o-stick"));
    std::fs::create_dir_all(stick.join("VirtualDJ/Folders")).unwrap();
    std::fs::create_dir_all(stick.join("VirtualDJ")).unwrap();
    std::fs::write(
        stick.join(DATABASE),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<VirtualDJ_Database Version=\"8.5\">\n <Song FilePath=\"/Volumes/KEY/Music/x.mp3\" FileSize=\"1\"/>\n</VirtualDJ_Database>\n",
    )
    .unwrap();
    let mut s = snapshot(&src);
    s.tracks.truncate(1);
    s.members = vec![vec![], vec![0], vec![0]];
    export(&stick.0, &s);
    let fp = songs_last(&stick.0);
    assert!(
        fp.starts_with("/Volumes/KEY/Contents/") && !fp.contains('\\'),
        "{fp}"
    );
    assert!(stick
        .join("VirtualDJ/Folders/Sets/Warm up.vdjfolder")
        .is_file());
    assert!(!stick.join("VirtualDJ/MyLists").exists());
    // Identity ignores the drive name, the separator and the case.
    assert_eq!(
        identity("E:\\Contents\\A.mp3"),
        identity("/Volumes/KEY/contents/a.mp3")
    );
    assert_ne!(
        identity("E:\\Contents\\A.mp3"),
        identity("E:\\Contents\\B.mp3")
    );
    assert_eq!(
        Style::of("/Volumes/KEY/x")
            .unwrap()
            .path("Contents/a b.mp3"),
        "/Volumes/KEY/Contents/a b.mp3"
    );
    assert_eq!(Style::of("e:/x").unwrap().path("a/b"), "e:/a/b");
}

#[test]
fn failures_leave_the_stick_as_it_was() {
    let (src, stick) = (Dir::new("e-src"), Dir::new("e-stick"));
    let s = snapshot(&src);
    // Not enough room: nothing written, nothing copied.
    let err = Plugin::export(
        &stick.0,
        &s,
        &ExportOptions {
            free: Some(100),
            ..Default::default()
        },
        &json!({}),
    )
    .unwrap_err();
    assert!(err.contains("Not enough space"), "{err}");
    assert_eq!(std::fs::read_dir(&stick.0).unwrap().count(), 0);
    // A database that is not XML is never overwritten.
    std::fs::create_dir_all(stick.join("VirtualDJ")).unwrap();
    std::fs::write(
        stick.join(DATABASE),
        "<VirtualDJ_Database><Song FilePath=\"x\"",
    )
    .unwrap();
    let err = Plugin::export(&stick.0, &s, &ExportOptions::default(), &json!({})).unwrap_err();
    assert!(err.contains("database.xml"), "{err}");
    assert_eq!(
        std::fs::read_to_string(stick.join(DATABASE)).unwrap(),
        "<VirtualDJ_Database><Song FilePath=\"x\""
    );
    assert!(!stick.join("Contents").exists());
    // Wrong root element.
    std::fs::write(stick.join(DATABASE), "<DJ_PLAYLISTS/>").unwrap();
    assert!(
        Plugin::export(&stick.0, &s, &ExportOptions::default(), &json!({}))
            .unwrap_err()
            .contains("not a VirtualDJ database")
    );
}

#[test]
fn missing_audio_is_skipped_and_a_loop_without_tempo_becomes_a_cue() {
    let (src, stick) = (Dir::new("m-src"), Dir::new("m-stick"));
    let mut t = track(&src, "a.wav", "T", 300);
    t.bpm_x100 = 0;
    let mut gone = track(&src, "gone.wav", "Gone", 301);
    gone.file = Some(src.join("nope.wav"));
    let mut a = analysis(vec![cue(2, 4_000, Some(6_000), None, None)]);
    a.beats.clear();
    let s = Snapshot {
        tracks: vec![(t, Some(a)), (gone, None)],
        playlists: vec![playlist("L", 0, false)],
        members: vec![vec![0, 1]],
        ..Snapshot::default()
    };
    let r = export(&stick.0, &s);
    assert_eq!((r.tracks, r.warnings.len()), (1, 2), "{:?}", r.warnings);
    assert!(
        r.warnings.iter().any(|w| w.contains("skipped"))
            && r.warnings.iter().any(|w| w.contains("loop"))
    );
    let songs = songs(&stick.0);
    let c = poi(&songs[0], "Num", "2");
    assert_eq!((c.attr("Type"), c.attr("Size")), (None, None));
    assert_eq!(child(&songs[0], "Tags").attr("Bpm"), None);
    assert_eq!(
        list_songs(&stick.join("VirtualDJ/MyLists/L.vdjfolder")).len(),
        1
    );
}

#[test]
fn keys_and_xml_edges() {
    assert_eq!(
        ["1A", "8A", "12A", "1B", "8B", "12B", "Am", "", "13A"].map(key_text),
        ["G#m", "Am", "C#m", "B", "C", "E", "Am", "", "13A"]
    );
    let e = xml::parse("<?xml version='1.0'?><!-- c --><A x='1 &amp; &#65;&#x42;' y=\"&unknown; \"><![CDATA[<raw>]]><B/>t &lt; u</A>").unwrap();
    assert_eq!(
        (e.attr("x"), e.attr("y")),
        (Some("1 & AB"), Some("&unknown; "))
    );
    assert_eq!(e.text(), "<raw>t < u");
    assert!(xml::parse("<A><B></A>").is_err());
    assert!(xml::parse("<A x=1/>").is_err());
    assert!(xml::split("<R><A></R>").is_err());
    assert!(xml::split("<R>stray text</R>").is_err());
    let d = xml::split("<R a=\"1\"/>").unwrap();
    assert_eq!((d.head.as_str(), d.items.len()), ("<R a=\"1\">", 0));
    // Control characters are dropped, never written.
    assert_eq!(xml::escape("a\u{1}b\tc\r", true), "ab&#9;c&#13;");
}
