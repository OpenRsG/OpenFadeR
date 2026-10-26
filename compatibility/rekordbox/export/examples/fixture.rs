//! A small synthetic stick for the independent readers of CI (`.github/check_stick.py`, `rekordcrate`): five generated WAV files
//! with unicode, emoji and long names, cues and loops, playlists in a folder, exported with the plugin's own writer.
//!
//! `cargo run --release -p compat-rekordbox-export --example fixture -- <folder>` writes `<folder>/stick` (the device) and
//! `<folder>/manifest.json`: what every reader must find on it (titles, cue counts, playlists and their tracks).

use openfader_plugin::library::{
    Analysis, Beat, Cue, ExportOptions, PREVIEW_ENTRIES, Playlist, Snapshot, Track,
};
use serde_json::json;
use std::path::{Path, PathBuf};

/// A `secs`-long 44.1 kHz stereo 16-bit WAV of a 220 Hz tone with a click on every beat of 120 BPM.
fn wav(path: &Path, secs: f64) {
    const RATE: usize = 44100;
    let n = (secs * RATE as f64) as usize;
    let mut data = Vec::with_capacity(n * 4);
    for i in 0..n {
        let tone = (std::f32::consts::TAU * 220.0 * i as f32 / RATE as f32).sin() * 0.2;
        let click = if i % (RATE / 2) < 60 { 0.7 } else { 0.0 };
        let v = ((tone + click) * 32767.0) as i16;
        data.extend(v.to_le_bytes());
        data.extend(v.to_le_bytes());
    }
    let mut w = [
        b"RIFF".as_slice(),
        &(36 + data.len() as u32).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
    ]
    .concat();
    w.extend_from_slice(&[1, 0, 2, 0]);
    w.extend_from_slice(&(RATE as u32).to_le_bytes());
    w.extend_from_slice(&(RATE as u32 * 4).to_le_bytes());
    w.extend_from_slice(&[4, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    std::fs::write(path, w).expect("write wav");
}

fn cue(hot_cue: u16, time_ms: u32, loop_end_ms: Option<u32>, comment: Option<&str>) -> Cue {
    Cue {
        hot_cue,
        time_ms,
        loop_end_ms,
        color: Some([40, 226, 20]),
        comment: comment.map(str::to_owned),
    }
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: fixture <folder>"));
    let (src, stick) = (out.join("source"), out.join("stick"));
    for d in [&src, &stick] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::fs::create_dir_all(&src).unwrap();
    let long = "Long name ".repeat(15);
    // (title, artist, album, file, seconds, cues)
    let rows: Vec<(String, String, String, String, f64, Vec<Cue>)> = vec![
        (
            "Ünïcødé Intro".into(),
            "Beyoncé".into(),
            "Nuit blanche".into(),
            "01 Ünïcødé Intro.wav".into(),
            4.0,
            vec![
                cue(1, 500, None, Some("drop")),
                cue(2, 1500, None, None),
                cue(0, 1000, None, None),
            ],
        ),
        (
            "日本語のトラック".into(),
            "坂本".into(),
            "音楽".into(),
            "日本語.wav".into(),
            3.0,
            vec![
                cue(1, 750, None, None),
                cue(0, 1250, Some(2250), Some("loop")),
            ],
        ),
        (
            "Emoji 🎧 Track".into(),
            "DJ 🎛 Crew".into(),
            "Vol.1: Test?".into(),
            "emoji 🎧.wav".into(),
            3.5,
            vec![cue(1, 1000, None, None)],
        ),
        (
            "Plain".into(),
            "Plain Artist".into(),
            String::new(),
            "plain.wav".into(),
            2.5,
            vec![],
        ),
        (
            long.trim().to_owned(),
            long.trim().to_owned(),
            long.trim().to_owned(),
            "long.wav".into(),
            2.0,
            vec![
                cue(3, 500, None, None),
                cue(0, 900, None, None),
                cue(0, 1300, None, None),
                cue(4, 1700, None, None),
            ],
        ),
    ];
    let mut tracks = Vec::new();
    for (title, artist, album, file, secs, cues) in &rows {
        let f = src.join(file);
        wav(&f, *secs);
        let beats: Vec<Beat> = (0..(*secs * 2.0) as u32 - 1)
            .map(|i| Beat {
                bar_pos: (i % 4) as u16 + 1,
                bpm_x100: 12000,
                time_ms: 250 + i * 500,
            })
            .collect();
        let band = |n: usize| {
            (0..n)
                .map(|i| [(i % 100) as u8, 40, 20])
                .collect::<Vec<_>>()
        };
        let analysis = Analysis {
            beats,
            cues: cues.clone(),
            three_band_detail: band(300),
            three_band_preview: band(PREVIEW_ENTRIES),
            key: Some("8A".into()),
            ..Analysis::default()
        };
        tracks.push((
            Track {
                title: title.clone(),
                artist: artist.clone(),
                album: album.clone(),
                key: "8A".into(),
                genre: "House".into(),
                bpm_x100: 12000,
                length_s: *secs as i64,
                file: Some(f),
                date_added: "2026-01-02".into(),
                bitrate: 1411,
                sample_rate: 44100,
                ..Track::default()
            },
            Some(analysis),
        ));
    }
    // "Warm Up" [0, 1]; folder "Sets" holding "Peak 🔥" [2, 3, 4]; "All" [0..5].
    let lists = [
        ("warm", "Warm Up", 0, false, vec![0, 1]),
        ("sets", "Sets", 0, true, vec![]),
        ("peak", "Peak 🔥", 1, false, vec![2, 3, 4]),
        ("all", "All", 0, false, vec![0, 1, 2, 3, 4]),
    ];
    let snap = Snapshot {
        tracks,
        playlists: lists
            .iter()
            .map(|l| Playlist {
                id: l.0.into(),
                name: l.1.into(),
                depth: l.2,
                is_folder: l.3,
            })
            .collect(),
        members: lists.iter().map(|l| l.4.clone()).collect(),
        ..Snapshot::default()
    };
    let report =
        compat_rekordbox_export::export(&stick, &snap, &ExportOptions::default(), &json!({}))
            .expect("export the fixture");
    assert_eq!(report.added, rows.len(), "{:?}", report.warnings);
    assert_eq!(report.verified, Some(true));

    let manifest = json!({
        "tracks": rows.iter().map(|r| json!({
            "title": r.0, "artist": r.1, "album": r.2,
            // 0 is a memory cue, 1.. the hot cue A..: the .DAT holds hot cues A to C and the memory cues, the .EXT all of them
            "cue_slots": r.5.iter().map(|c| c.hot_cue).collect::<Vec<_>>(), "beats": (r.4 * 2.0) as u32 - 1,
        })).collect::<Vec<_>>(),
        "playlists": lists.iter().map(|l| json!({
            "name": l.1,
            "folder": l.3,
            "parent": if l.2 == 1 { json!("Sets") } else { json!(null) },
            "tracks": l.4.iter().map(|&i| rows[i].0.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    println!("{}", out.display());
}
