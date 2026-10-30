mod common;

use common::*;
use openfader_plugin::library::{ExportOptions, ExportReport, LibraryPlugin, Snapshot};
use compat_serato::{crate_file, database, Plugin};
use std::fs;
use std::path::{Path, PathBuf};

const SHARED: &str = "Contents/Artist/Album/";

fn export(usb: &Path, snap: &Snapshot) -> ExportReport {
    Plugin::export(
        usb,
        snap,
        &ExportOptions::default(),
        &serde_json::Value::Null,
    )
    .unwrap()
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join("lib").join(name);
    fs::write(&p, bytes).unwrap();
    p
}

fn db(usb: &Path) -> Vec<u8> {
    fs::read(usb.join(database::FILE)).unwrap()
}

fn crate_paths(usb: &Path, name: &str) -> Vec<String> {
    crate_file::paths(&fs::read(usb.join(format!("{}/{name}.crate", crate_file::DIR))).unwrap())
        .unwrap()
}

fn files_under(dir: &Path) -> usize {
    fs::read_dir(dir).map_or(0, |d| {
        d.flatten()
            .map(|e| {
                if e.path().is_dir() {
                    files_under(&e.path())
                } else {
                    1
                }
            })
            .sum()
    })
}

/// The tags of an MP3's tag, decoded.
fn mp3_tags(bytes: &[u8]) -> (Vec<(String, Vec<u8>)>, Vec<u8>) {
    let f = id3_frames(bytes);
    let m = markers2(&geob(&f, "Serato Markers2").expect("Markers2 frame"));
    let g = geob(&f, "Serato BeatGrid").expect("BeatGrid frame");
    (m, g)
}

#[test]
fn mp3_with_padding_is_tagged_in_place() {
    let d = tmp("mp3_pad");
    let (usb, src) = (d.join("usb"), put(&d, "a.mp3", &mp3(4096)));
    let rep = export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    assert_eq!(
        (rep.tracks, rep.added, rep.updated, rep.playlists),
        (1, 1, 0, 1),
        "{rep:?}"
    );
    assert!(
        rep.notes.iter().any(|n| n.contains("1 memory cues")),
        "{:?}",
        rep.notes
    );
    assert!(!rep.notes.iter().any(|n| n.contains("private")));

    let (before, after) = (
        fs::read(&src).unwrap(),
        fs::read(usb.join(SHARED).join("a.mp3")).unwrap(),
    );
    assert_eq!(before.len(), after.len(), "size unchanged");
    assert_eq!(id3_size(&after), 4096);
    assert_eq!(before[4096..], after[4096..], "audio untouched");
    let (m, g) = mp3_tags(&after);
    assert_standard(&m, &g, 1500);
    let frames = id3_frames(&after);
    assert_eq!(geob(&frames, "Serato Markers_"), None);
    assert!(
        frames.iter().any(|(id, b)| id == "TIT2" && b == b"\0Song"),
        "existing frames kept"
    );
    assert!(!usb.join("_Serato_/Audio").exists());

    assert_eq!(
        database::paths(&db(&usb)).unwrap(),
        vec![format!("{SHARED}a.mp3")]
    );
    assert_eq!(crate_paths(&usb, "Mix"), vec![format!("{SHARED}a.mp3")]);
}

#[test]
fn mp3_without_room_gets_a_private_copy_and_later_exports_update_it() {
    let d = tmp("mp3_tight");
    let (usb, src) = (d.join("usb"), put(&d, "a.mp3", &mp3(ID3_TIGHT)));
    let rep = export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    assert!(
        rep.notes
            .iter()
            .any(|n| n.contains("1 tracks got a private copy")),
        "{:?}",
        rep.notes
    );

    let shared = usb.join(SHARED).join("a.mp3");
    assert_eq!(
        fs::read(&shared).unwrap(),
        fs::read(&src).unwrap(),
        "shared copy byte-identical"
    );
    let private_rel = "_Serato_/Audio/Artist/Album/a.mp3";
    let private = fs::read(usb.join(private_rel)).unwrap();
    let (m, g) = mp3_tags(&private);
    assert_standard(&m, &g, 1500);
    assert!(id3_frames(&private)
        .iter()
        .any(|(id, b)| id == "TIT2" && b == b"\0Song"));
    let tag = id3_size(&private);
    assert_eq!(
        private[tag..],
        fs::read(&src).unwrap()[ID3_TIGHT..],
        "audio untouched"
    );
    assert_eq!(
        database::paths(&db(&usb)).unwrap(),
        vec![private_rel.to_string()]
    );
    assert_eq!(crate_paths(&usb, "Mix"), vec![private_rel.to_string()]);

    // Second export: only a changed cue, no playlists.
    let mut snap = snapshot(track(&src, "Song"), analysis(2500));
    (snap.playlists, snap.members) = (vec![], vec![]);
    let rep = export(&usb, &snap);
    assert_eq!((rep.added, rep.updated), (0, 1), "{rep:?}");
    assert!(
        rep.notes.iter().all(|n| !n.contains("private")),
        "no second copy: {:?}",
        rep.notes
    );
    let (m, g) = mp3_tags(&fs::read(usb.join(private_rel)).unwrap());
    assert_standard(&m, &g, 2500);
    assert_eq!(fs::read(&shared).unwrap(), fs::read(&src).unwrap());
    assert_eq!(files_under(&usb.join("Contents")), 1);
    assert_eq!(files_under(&usb.join("_Serato_/Audio")), 1);
    assert_eq!(
        database::paths(&db(&usb)).unwrap(),
        vec![private_rel.to_string()]
    );
    assert_eq!(
        crate_paths(&usb, "Mix"),
        vec![private_rel.to_string()],
        "playlist of the first export kept"
    );
}

#[test]
fn a_track_that_went_private_replaces_the_shared_record() {
    // First export without analysis: the shared copy is listed. The second one needs tags that do not fit.
    let d = tmp("mp3_switch");
    let (usb, src) = (d.join("usb"), put(&d, "a.mp3", &mp3(ID3_TIGHT)));
    let mut plain = snapshot(track(&src, "Song"), analysis(0));
    plain.tracks[0].1 = None;
    plain.colors.clear();
    export(&usb, &plain);
    assert_eq!(
        database::paths(&db(&usb)).unwrap(),
        vec![format!("{SHARED}a.mp3")]
    );
    export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    assert_eq!(
        database::paths(&db(&usb)).unwrap(),
        vec!["_Serato_/Audio/Artist/Album/a.mp3".to_string()]
    );
}

#[test]
fn existing_cues_of_the_file_are_refreshed_and_unknown_entries_kept() {
    let d = tmp("mp3_merge");
    let usb = d.join("usb");
    // A tag that already holds a Markers2 with a FLIP entry, a locked grid and an old cue, plus the older Markers_ frame.
    let mut inner = vec![1, 1];
    for (n, data) in [
        ("COLOR", vec![0, 9, 9, 9]),
        ("CUE", vec![0, 5, 0, 0, 0, 7, 0, 1, 1, 1, 0, 0, 0]),
        ("BPMLOCK", vec![1]),
        ("FLIP", vec![0, 0, 0]),
    ] {
        inner.extend(n.bytes());
        inner.push(0);
        inner.extend((data.len() as u32).to_be_bytes());
        inner.extend(data);
    }
    inner.push(0);
    let geob_body = |desc: &str, data: &[u8]| {
        [
            &[0][..],
            b"application/octet-stream\0\0",
            desc.as_bytes(),
            b"\0",
            data,
        ]
        .concat()
    };
    let mut tag_body = id3_frame("TIT2", b"\0Song");
    tag_body.extend(id3_frame(
        "GEOB",
        &geob_body(
            "Serato Markers2",
            &[&[1, 1][..], b64(&inner).as_bytes()].concat(),
        ),
    ));
    tag_body.extend(id3_frame("GEOB", &geob_body("Serato Markers_", &[1, 1, 7])));
    let mut file = vec![b'I', b'D', b'3', 3, 0, 0, 0, 0, 0x20, 0];
    file.extend(&tag_body);
    file.resize(10 + 0x1000, 0);
    file.extend(audio(3000));
    let src = put(&d, "a.mp3", &file);

    let mut a = analysis(1500);
    a.cues.truncate(1);
    let mut snap = snapshot(track(&src, "Song"), a);
    snap.colors.clear();
    let rep = export(&usb, &snap);
    assert!(rep.notes.is_empty(), "{rep:?}");
    let out = fs::read(usb.join(SHARED).join("a.mp3")).unwrap();
    assert_eq!(out.len(), file.len());
    let frames = id3_frames(&out);
    assert_eq!(
        geob(&frames, "Serato Markers_"),
        None,
        "Markers_ would override Markers2"
    );
    let m = markers2(&geob(&frames, "Serato Markers2").unwrap());
    let names: Vec<&str> = m.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["COLOR", "CUE", "BPMLOCK", "FLIP"]);
    assert_eq!(
        m[0].1,
        [0, 9, 9, 9],
        "no host colour: the file's colour stays"
    );
    assert_eq!(
        cue(&m[1].1),
        (0, 1500, [1, 2, 3], "Drop".to_string()),
        "old cue replaced"
    );
    assert_eq!(m[2].1, [1], "grid lock stays");
    assert_eq!(m[3].1, [0, 0, 0]);
}

#[test]
fn flac_with_padding_keeps_its_size() {
    let d = tmp("flac_pad");
    let (usb, src) = (d.join("usb"), put(&d, "a.flac", &flac(Some(4096))));
    let rep = export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    assert_eq!(rep.tracks, 1);
    let (before, after) = (
        fs::read(&src).unwrap(),
        fs::read(usb.join(SHARED).join("a.flac")).unwrap(),
    );
    assert_eq!(before.len(), after.len());
    // Walk the blocks: audio starts at the same offset, exactly the last block is flagged, tags decode.
    let (mut pos, mut types, mut comments) = (4, vec![], vec![]);
    loop {
        let (flag, len) = (
            after[pos],
            u32::from_be_bytes([0, after[pos + 1], after[pos + 2], after[pos + 3]]) as usize,
        );
        types.push(flag & 0x7f);
        if flag & 0x7f == 4 {
            let b = &after[pos + 4..pos + 4 + len];
            let mut at = 4 + u32::from_le_bytes(b[..4].try_into().unwrap()) as usize + 4;
            while at < b.len() {
                let n = u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
                comments.push(String::from_utf8(b[at + 4..at + 4 + n].to_vec()).unwrap());
                at += 4 + n;
            }
        }
        pos += 4 + len;
        if flag & 0x80 != 0 {
            break;
        }
    }
    assert_eq!(types, [0, 4, 1], "last block is the padding");
    assert_eq!(before[pos..], after[pos..], "audio untouched");
    let value = |k: &str| {
        comments
            .iter()
            .find_map(|c| c.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!("{k} in {comments:?}"))
    };
    assert_eq!(value("TITLE"), "Song");
    let m = markers2(&field(value("SERATO_MARKERS_V2"), "Serato Markers2"));
    assert_standard(
        &m,
        &field(value("SERATO_BEATGRID"), "Serato BeatGrid"),
        1500,
    );
    assert!(!usb.join("_Serato_/Audio").exists());
}

#[test]
fn flac_without_padding_goes_private_and_stays_valid() {
    let d = tmp("flac_tight");
    let (usb, src) = (d.join("usb"), put(&d, "a.flac", &flac(None)));
    export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    let before = fs::read(&src).unwrap();
    assert_eq!(fs::read(usb.join(SHARED).join("a.flac")).unwrap(), before);
    let private = fs::read(usb.join("_Serato_/Audio/Artist/Album/a.flac")).unwrap();
    assert!(private.len() > before.len());
    let audio_len = 5000;
    assert_eq!(
        private[private.len() - audio_len..],
        before[before.len() - audio_len..]
    );
    assert_eq!(&private[..4], b"fLaC");
    // Metadata blocks parse and end with the flag on the padding that was added.
    let mut pos = 4;
    let last_type = loop {
        let (flag, len) = (
            private[pos],
            u32::from_be_bytes([0, private[pos + 1], private[pos + 2], private[pos + 3]]) as usize,
        );
        pos += 4 + len;
        if flag & 0x80 != 0 {
            break flag & 0x7f;
        }
    };
    assert_eq!(last_type, 1);
    assert_eq!(pos, private.len() - audio_len);
}

#[test]
fn wav_gets_an_id3_chunk_in_a_private_copy() {
    let d = tmp("wav");
    let (usb, src) = (d.join("usb"), put(&d, "a.wav", &wav()));
    export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    let before = fs::read(&src).unwrap();
    assert_eq!(fs::read(usb.join(SHARED).join("a.wav")).unwrap(), before);
    let out = fs::read(usb.join("_Serato_/Audio/Artist/Album/a.wav")).unwrap();
    let riff = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
    assert_eq!(riff + 8, out.len(), "container size follows the new chunk");
    assert_eq!(
        out[..before.len()][8..],
        before[8..],
        "existing chunks untouched"
    );
    let at = before.len();
    assert_eq!(&out[at..at + 4], b"id3 ");
    let n = u32::from_le_bytes(out[at + 4..at + 8].try_into().unwrap()) as usize;
    assert_eq!(out.len(), at + 8 + n + (n & 1));
    let (m, g) = mp3_tags(&out[at + 8..at + 8 + n]);
    assert_standard(&m, &g, 1500);

    // The chunk is rewritten in place on the next export (it has padding), even a longer cue list.
    let mut a = analysis(2500);
    a.cues.push(openfader_plugin::library::Cue {
        hot_cue: 3,
        time_ms: 9000,
        loop_end_ms: None,
        color: None,
        comment: Some("Later".into()),
    });
    export(&usb, &snapshot(track(&src, "Song"), a));
    let again = fs::read(usb.join("_Serato_/Audio/Artist/Album/a.wav")).unwrap();
    assert_eq!(again.len(), out.len());
    let (m, _) = mp3_tags(&again[at + 8..at + 8 + n]);
    assert_eq!(m.iter().filter(|(k, _)| k == "CUE").count(), 3);
}

fn stco_target(bytes: &[u8]) -> usize {
    let at = bytes.windows(4).position(|w| w == b"stco").unwrap();
    u32::from_be_bytes(bytes[at + 12..at + 16].try_into().unwrap()) as usize
}

fn m4a_items(bytes: &[u8]) -> Vec<(String, String)> {
    // (name, value) of every `----` item: scan for the atoms inside ilst by their names.
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = bytes[i..].windows(4).position(|w| w == b"----") {
        let start = i + p - 4;
        let size = u32::from_be_bytes(bytes[start..start + 4].try_into().unwrap()) as usize;
        let item = &bytes[start + 8..start + size];
        let (mean, rest) = (
            u32::from_be_bytes(item[..4].try_into().unwrap()) as usize,
            &item[..],
        );
        assert_eq!(&rest[4..8], b"mean");
        assert_eq!(&rest[12..mean], b"com.serato.dj");
        let name_at = mean;
        let nlen = u32::from_be_bytes(rest[name_at..name_at + 4].try_into().unwrap()) as usize;
        let name = String::from_utf8(rest[name_at + 12..name_at + nlen].to_vec()).unwrap();
        let data_at = name_at + nlen;
        let dlen = u32::from_be_bytes(rest[data_at..data_at + 4].try_into().unwrap()) as usize;
        assert_eq!(&rest[data_at + 4..data_at + 8], b"data");
        out.push((
            name,
            String::from_utf8(rest[data_at + 16..data_at + dlen].to_vec()).unwrap(),
        ));
        i = start + size;
    }
    out
}

fn assert_m4a_tags(bytes: &[u8]) {
    let items = m4a_items(bytes);
    let get = |n: &str| {
        items
            .iter()
            .find(|(k, _)| k == n)
            .unwrap_or_else(|| panic!("{n} in {items:?}"))
            .1
            .clone()
    };
    assert_standard(
        &markers2(&field(&get("markersv2"), "Serato Markers2")),
        &field(&get("beatgrid"), "Serato BeatGrid"),
        1500,
    );
    assert!(
        !get("beatgrid").contains('\n'),
        "beatgrid has no line feeds"
    );
}

#[test]
fn m4a_with_a_free_atom_keeps_its_size() {
    let d = tmp("m4a_free");
    let (usb, src) = (d.join("usb"), put(&d, "a.m4a", &m4a(Some(8192))));
    export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    let (before, after) = (
        fs::read(&src).unwrap(),
        fs::read(usb.join(SHARED).join("a.m4a")).unwrap(),
    );
    assert_eq!(before.len(), after.len());
    assert_eq!(stco_target(&before), stco_target(&after));
    assert_eq!(
        after[stco_target(&after)..stco_target(&after) + 3000],
        m4a_audio()[..]
    );
    assert_m4a_tags(&after);
    assert!(after.windows(4).any(|w| w == b"\xa9nam"), "title item kept");
    assert!(!usb.join("_Serato_/Audio").exists());
}

#[test]
fn m4a_without_room_gets_a_private_copy_with_shifted_chunk_offsets() {
    let d = tmp("m4a_tight");
    let (usb, src) = (d.join("usb"), put(&d, "a.m4a", &m4a(None)));
    export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    let before = fs::read(&src).unwrap();
    assert_eq!(fs::read(usb.join(SHARED).join("a.m4a")).unwrap(), before);
    let out = fs::read(usb.join("_Serato_/Audio/Artist/Album/a.m4a")).unwrap();
    assert!(out.len() > before.len());
    assert_eq!(
        stco_target(&out) - stco_target(&before),
        out.len() - before.len(),
        "moov precedes mdat"
    );
    assert_eq!(
        out[stco_target(&out)..stco_target(&out) + 3000],
        m4a_audio()[..],
        "the offset still reaches the audio"
    );
    assert_m4a_tags(&out);
    // Next export fits in place thanks to the padding left behind.
    export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    assert_eq!(
        fs::read(usb.join("_Serato_/Audio/Artist/Album/a.m4a"))
            .unwrap()
            .len(),
        out.len()
    );
}

#[test]
fn unsupported_containers_stay_untouched_with_a_warning() {
    let d = tmp("ogg");
    let (usb, src) = (d.join("usb"), put(&d, "a.ogg", &audio(500)));
    let rep = export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    assert_eq!(rep.tracks, 1);
    assert!(
        rep.warnings.iter().any(|w| w.contains("not written")),
        "{:?}",
        rep.warnings
    );
    assert_eq!(
        fs::read(usb.join(SHARED).join("a.ogg")).unwrap(),
        fs::read(&src).unwrap()
    );
    assert_eq!(
        database::paths(&db(&usb)).unwrap(),
        vec![format!("{SHARED}a.ogg")]
    );
}

#[test]
fn database_merge_keeps_foreign_entries_and_replaces_ours() {
    let d = tmp("db_merge");
    let (usb, src) = (d.join("usb"), put(&d, "a.mp3", &mp3(4096)));
    let ours = format!("{SHARED}a.mp3");
    let tlv = |tag: &[u8; 4], data: &[u8]| {
        [tag.as_slice(), &(data.len() as u32).to_be_bytes(), data].concat()
    };
    let text = |tag: &[u8; 4], s: &str| {
        tlv(
            tag,
            &s.encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>(),
        )
    };
    let otrk = |path: &str, title: &str| {
        tlv(
            b"otrk",
            &[text(b"pfil", path), text(b"tsng", title)].concat(),
        )
    };
    let old = [
        text(b"vrsn", "2.0/Serato Scratch LIVE Database"),
        otrk("Foreign/x.mp3", "Foreign"),
        otrk(&ours, "Old title"),
        tlv(b"ovct", &text(b"tvcn", "song")),
    ]
    .concat();
    fs::create_dir_all(usb.join("_Serato_/Subcrates")).unwrap();
    fs::write(usb.join(database::FILE), &old).unwrap();
    let foreign_crate = crate_file::build(&["Foreign/x.mp3"]);
    fs::write(usb.join("_Serato_/Subcrates/Other.crate"), &foreign_crate).unwrap();
    fs::write(
        usb.join("_Serato_/Subcrates/Mix.crate"),
        crate_file::build(&["Foreign/x.mp3"]),
    )
    .unwrap();

    let rep = export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    assert_eq!((rep.added, rep.updated), (0, 1), "{rep:?}");
    let new = db(&usb);
    assert_eq!(
        database::paths(&new).unwrap(),
        vec!["Foreign/x.mp3".to_string(), ours.clone()],
        "order kept, no duplicate"
    );
    assert_eq!(
        database::fields(&new, "Foreign/x.mp3").unwrap().unwrap()["tsng"],
        "Foreign"
    );
    let f = database::fields(&new, &ours).unwrap().unwrap();
    assert_eq!(
        (
            f["tsng"].as_str(),
            f["tart"].as_str(),
            f["talb"].as_str(),
            f["tgen"].as_str()
        ),
        ("Song", "Artist", "Album", "House")
    );
    assert_eq!(
        (
            f["tbpm"].as_str(),
            f["tkey"].as_str(),
            f["ttyp"].as_str(),
            f["tlen"].as_str()
        ),
        ("120.00", "Am", "mp3", "03:20.00")
    );
    assert!(
        new.windows(4).any(|w| w == b"ovct"),
        "other top-level records kept"
    );
    assert_eq!(
        fs::read(usb.join("_Serato_/Subcrates/Other.crate")).unwrap(),
        foreign_crate
    );
    assert_eq!(
        crate_paths(&usb, "Mix"),
        vec![ours],
        "same-named crate replaced"
    );
}

#[test]
fn crates_follow_the_playlist_tree() {
    let d = tmp("crates");
    let (usb, src) = (d.join("usb"), put(&d, "a.mp3", &mp3(4096)));
    let mut snap = snapshot(track(&src, "Song"), analysis(1500));
    snap.playlists = vec![
        playlist("Sets", 0, true),
        playlist("Warm/up", 1, false),
        playlist("Peak", 0, false),
    ];
    snap.members = vec![vec![], vec![0], vec![0, 0]];
    export(&usb, &snap);
    let dir = usb.join("_Serato_/Subcrates");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["Peak.crate", "Sets%%Warm_up.crate", "Sets.crate"]);
    assert_eq!(
        crate_paths(&usb, "Sets%%Warm_up"),
        vec![format!("{SHARED}a.mp3")]
    );
    assert_eq!(crate_paths(&usb, "Peak").len(), 2);
    assert!(crate_paths(&usb, "Sets").is_empty());
    // A folder never empties a crate the user filled.
    fs::write(dir.join("Sets.crate"), crate_file::build(&["x.mp3"])).unwrap();
    export(&usb, &snap);
    assert_eq!(crate_paths(&usb, "Sets"), vec!["x.mp3".to_string()]);
}

#[test]
fn unreadable_tracks_are_skipped_and_too_little_space_fails_up_front() {
    let d = tmp("errors");
    let (usb, src) = (d.join("usb"), put(&d, "a.mp3", &mp3(4096)));
    let mut snap = snapshot(track(&src, "Song"), analysis(1500));
    snap.tracks
        .push((track(&d.join("lib/missing.mp3"), "Gone"), None));
    snap.members = vec![vec![0, 1]];
    let rep = export(&usb, &snap);
    assert_eq!(rep.tracks, 1);
    assert!(
        rep.warnings.iter().any(|w| w.starts_with("Gone:")),
        "{:?}",
        rep.warnings
    );
    assert_eq!(crate_paths(&usb, "Mix").len(), 1);

    let usb2 = d.join("usb2");
    fs::create_dir_all(&usb2).unwrap();
    let opts = ExportOptions {
        free: Some(10),
        ..ExportOptions::default()
    };
    let err = Plugin::export(
        &usb2,
        &snapshot(track(&src, "Song"), analysis(1500)),
        &opts,
        &serde_json::Value::Null,
    )
    .unwrap_err();
    assert!(err.contains("Not enough space"), "{err}");
    assert!(
        !usb2.join("_Serato_").exists() && !usb2.join("Contents").exists(),
        "nothing written"
    );
}

#[test]
fn device_probe_reports_the_library() {
    let d = tmp("probe");
    let usb = d.join("usb");
    let none = Plugin::device_probe(&usb, &serde_json::Value::Null).unwrap();
    assert!(none.libraries.is_empty() && none.can_write && none.modified.is_none());
    let src = put(&d, "a.mp3", &mp3(4096));
    export(&usb, &snapshot(track(&src, "Song"), analysis(1500)));
    let info = Plugin::device_probe(&usb, &serde_json::Value::Null).unwrap();
    assert_eq!(info.libraries, ["Serato library"]);
    assert!(info.modified.is_some_and(|t| t > 1_700_000_000));
}
