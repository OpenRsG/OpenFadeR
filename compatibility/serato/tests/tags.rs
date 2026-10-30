mod common;

use common::*;
use openfader_plugin::library::{Beat, ExportOptions, LibraryPlugin};
use compat_serato::tag::markers::{beatgrid, entries, Tags};
use compat_serato::Plugin;
use std::fs;

#[test]
fn reads_the_markers2_serato_wrote_in_the_reverse_engineering_notes() {
    // `Serato Markers2.octet-stream` of an analysed file from Holzhaus/serato-tags docs/writeup.md: padded with NULs to 470 bytes.
    let mut geob = b"\x01\x01AQFDT0xPUgAAAAAEAP///0JQTUxPQ0sAAAAAAQAA".to_vec();
    geob.resize(470, 0);
    let e = entries(&geob);
    assert_eq!(
        e,
        vec![
            ("COLOR".to_string(), vec![0, 255, 255, 255]),
            ("BPMLOCK".to_string(), vec![0])
        ]
    );
    // A string one character too long (Serato's quirk) still decodes.
    geob.splice(
        2..,
        b"AQFDT0xPUgAAAAAEAP///0JQTUxPQ0sAAAAAAQAAA".iter().copied(),
    );
    assert_eq!(entries(&geob).len(), 2);
}

#[test]
fn many_cues_wrap_at_72_characters_and_round_trip() {
    let mut a = analysis(100);
    a.cues = (1..=8)
        .map(|i| openfader_plugin::library::Cue {
            hot_cue: i,
            time_ms: 1000 * u32::from(i),
            loop_end_ms: None,
            color: None,
            comment: Some(format!("cue number {i} with a name")),
        })
        .collect();
    let (tags, memory) = Tags::new(Some(&a), None);
    assert_eq!(memory, 0);
    let data = tags.markers2(None).unwrap();
    let m = markers2(&data);
    assert!(data.contains(&b'\n'));
    let cues: Vec<_> = m
        .iter()
        .filter(|(k, _)| k == "CUE")
        .map(|(_, d)| cue(d))
        .collect();
    assert_eq!(cues.len(), 8);
    assert_eq!(cues[7].0, 7);
    assert_eq!(cues[7].3, "cue number 8 with a name");
    // Without cues, colour or an existing tag there is nothing to write.
    assert!(
        Tags::new(Some(&openfader_plugin::library::Analysis::default()), None)
            .0
            .markers2(None)
            .is_none()
    );
}

#[test]
fn a_tempo_change_starts_a_new_beat_grid_marker() {
    let beats: Vec<Beat> = (0..16u32)
        .map(|i| {
            if i < 8 {
                Beat {
                    bar_pos: (i % 4 + 1) as u16,
                    bpm_x100: 12000,
                    time_ms: 1000 + 500 * i,
                }
            } else {
                Beat {
                    bar_pos: (i % 4 + 1) as u16,
                    bpm_x100: 12800,
                    time_ms: 5000 + 469 * (i - 8),
                }
            }
        })
        .collect();
    let g = common::beatgrid(&beatgrid(&beats).unwrap());
    assert_eq!(g.len(), 2);
    assert_eq!(g[0], (1.0, 8.0), "8 beats up to the next marker");
    assert!((g[1].0 - 5.0).abs() < 1e-6);
    assert!(
        (g[1].1 - 60000.0 / 469.0).abs() < 0.01,
        "BPM from the real beat spacing: {}",
        g[1].1
    );
    // A grid that does not start on a downbeat starts at the first one.
    let mut late = beats.clone();
    late[0].bar_pos = 2;
    late[1].bar_pos = 3;
    late[2].bar_pos = 4;
    let g = common::beatgrid(&beatgrid(&late).unwrap());
    assert_eq!(
        g[0],
        (3.0, 4.0),
        "starts at the first downbeat (beat 4), 4 beats to the tempo change"
    );
    assert!(beatgrid(&[]).is_none());
}

#[test]
fn an_unsynchronised_tag_is_decoded_and_written_back_plain() {
    // Frame bodies with 0xFF 0xE0 are stored as FF 00 E0 when the tag carries the unsynchronisation flag.
    let d = tmp("unsync");
    let plain_body = [&[0][..], &[0xff, 0xe0, 0xff, 0x00, b'x']].concat();
    let frame = id3_frame("TIT2", &plain_body);
    let mut stored = Vec::new();
    for (i, &c) in frame.iter().enumerate() {
        stored.push(c);
        if c == 0xff && frame.get(i + 1).is_none_or(|&n| n >= 0xe0 || n == 0) {
            stored.push(0);
        }
    }
    let total = 10 + 600;
    let mut tag = vec![
        b'I',
        b'D',
        b'3',
        3,
        0,
        0x80,
        0,
        0,
        (600 >> 7) as u8,
        (600 & 0x7f) as u8,
    ];
    tag.extend(&stored);
    tag.resize(total, 0);
    let file = [tag, audio(1000)].concat();
    let src = d.join("lib/a.mp3");
    fs::write(&src, &file).unwrap();
    let usb = d.join("usb");
    let snap = snapshot(track(&src, "Song"), analysis(1500));
    Plugin::export(
        &usb,
        &snap,
        &ExportOptions::default(),
        &serde_json::Value::Null,
    )
    .unwrap();
    let out = fs::read(usb.join("Contents/Artist/Album/a.mp3")).unwrap();
    assert_eq!(out.len(), file.len());
    assert_eq!(out[5], 0, "unsynchronisation flag cleared");
    let frames = id3_frames(&out);
    assert_eq!(frames[0], ("TIT2".to_string(), plain_body));
    assert!(geob(&frames, "Serato Markers2").is_some());
    assert_eq!(out[total..], file[total..]);
}
