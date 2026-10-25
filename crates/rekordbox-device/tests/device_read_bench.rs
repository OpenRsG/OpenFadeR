//! A regression bench for the device read path (`PIONEER/rekordbox/{export.pdb,exportLibrary.db}` → tracks +
//! playlists): how long opening a device library and listing everything on it takes, alone and when two openers race
//! it the way a real mount does (the browser's own read, `app/browser/sources.rs`, and the device's automatic
//! cue-sync job, `Auto::Link` in `app/device/scan.rs`, both start within the mount's first moment). The first opener pays
//! the SQLCipher key derivation of a OneLibrary once (PBKDF2-HMAC-SHA512, 256000 rounds: about 70 ms on a Ryzen 9700X, the
//! floor of a first read); every later connection to the same file uses the derived raw key (`read/onelibrary.rs`).
//! Before that, the open, the playlists and the tracks each derived it: 234 ms for the owner's 24-track stick, 251 ms for
//! the 5k fixture; after: 75 ms and 84 ms, and the second opener under 1 ms and 11 ms.
//!
//! Run with `cargo test -p rekordbox-device --release -- --ignored --nocapture bench_device_read`. Point
//! `OPENFADER_USB` (the convention `tests/common` already uses) at a real device root to bench that library instead
//! of the synthetic one this test builds on the fly, e.g. a read-only copy of the owner's stick at
//! `.ofwork/db-stick`. Unset, it writes a 5000-track device of its own through the plugin's real exporter (same code
//! path a real rekordbox export takes, so the parse/decrypt side is benched against real on-disk bytes).

mod common;

use openfader_plugin::library::{Analysis, Beat, ExportOptions, Snapshot, Track};
use std::path::{Path, PathBuf};
use std::time::Instant;

const TRACKS: usize = 5000;
const PLAYLISTS: usize = 20;

/// A silent 0.2 s 44.1 kHz stereo WAV: its bytes are never read back, only its path matters to the exporter, and every
/// synthetic track points at this one file (shortcut: a real stick never reuses one file for 5000 tracks, but the
/// device-read path under test never opens the audio either, so it cannot tell; upgrade to per-track files if a future
/// bench needs to measure the export/copy side instead).
fn silence(path: &Path) {
    let data = vec![0u8; 4 * (44100 / 5)];
    let mut w = [
        b"RIFF".as_slice(),
        &(36 + data.len() as u32).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
    ]
    .concat();
    w.extend_from_slice(&[1, 0, 2, 0]);
    w.extend_from_slice(&44100u32.to_le_bytes());
    w.extend_from_slice(&(44100u32 * 4).to_le_bytes());
    w.extend_from_slice(&[4, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    std::fs::write(path, w).expect("write the fixture's silent wav");
}

/// A `tracks`-track device root under `dir`, with [`PLAYLISTS`] playlists of 50 tracks each plus an "All" playlist,
/// written by [`rekordbox_export::export`] (the plugin's own writer), so both `export.pdb` and `exportLibrary.db` end
/// up on it exactly as a real rekordbox export would leave them.
fn build_fixture(dir: &Path, tracks: usize) -> PathBuf {
    let stick = dir.join("stick");
    let _ = std::fs::remove_dir_all(&stick);
    let src = dir.join("source.wav");
    silence(&src);
    let mut snap = Snapshot::default();
    for i in 0..tracks {
        let beats: Vec<Beat> = (0..8)
            .map(|b| Beat {
                bar_pos: (b % 4) as u16 + 1,
                bpm_x100: 12000,
                time_ms: 250 + b * 500,
            })
            .collect();
        snap.tracks.push((
            Track {
                title: format!("Track {i:05}"),
                artist: format!("Artist {}", i % 200),
                album: format!("Album {}", i % 500),
                key: "8A".into(),
                genre: "House".into(),
                bpm_x100: 12000 + (i % 40) as i64 * 100,
                length_s: 180 + (i % 120) as i64,
                file: Some(src.clone()),
                date_added: "2026-01-02".into(),
                bitrate: 320,
                sample_rate: 44100,
                ..Track::default()
            },
            Some(Analysis {
                beats,
                key: Some("8A".into()),
                ..Analysis::default()
            }),
        ));
    }
    snap.playlists.push(openfader_plugin::library::Playlist {
        id: "all".into(),
        name: "All".into(),
        depth: 0,
        is_folder: false,
    });
    snap.members.push((0..tracks).collect());
    for p in 0..PLAYLISTS {
        snap.playlists.push(openfader_plugin::library::Playlist {
            id: format!("p{p}"),
            name: format!("Set {p}"),
            depth: 0,
            is_folder: false,
        });
        snap.members
            .push((p * 50..(p * 50 + 50).min(tracks)).collect());
    }
    rekordbox_export::export(
        &stick,
        &snap,
        &ExportOptions::default(),
        &serde_json::json!({}),
    )
    .expect("export the fixture");
    stick
}

/// Open the device library at `root` and list everything on it, the way `app/browser/sources.rs::read` does.
fn read_once(root: &Path) -> (usize, usize) {
    let t = Instant::now();
    let lib = common::open(root, None);
    let open = t.elapsed();
    let playlists = lib.playlists().expect("playlists").len();
    let lists = t.elapsed() - open;
    let tracks = lib.tracks(None).expect("tracks").len();
    println!("  open {open:?}, playlists {lists:?}, tracks {:?}", t.elapsed() - open - lists);
    (tracks, playlists)
}

#[test]
#[ignore = "bench: run with --nocapture; builds or reuses a 5k-track device fixture"]
fn bench_device_read() {
    let (root, source) = match common::usb_root() {
        Some(r) => (r, "OPENFADER_USB"),
        None => {
            let dir =
                std::env::temp_dir().join(format!("rekordbox-device-bench-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            (build_fixture(&dir, TRACKS), "synthetic 5k fixture")
        }
    };
    println!("device: {} ({source})", root.display());

    let start = Instant::now();
    let solo = read_once(&root);
    let cold = start.elapsed();
    println!(
        "device read, one opener: {} tracks, {} playlists in {cold:?}",
        solo.0, solo.1
    );

    // The real app's second opener on the same mount, moments later: the browser's own read and the automatic
    // cue-sync job both open the same library independently when a device appears.
    let start = Instant::now();
    let r2 = root.clone();
    let other = std::thread::spawn(move || read_once(&r2));
    let here = read_once(&root);
    let there = other.join().unwrap();
    let concurrent = start.elapsed();
    assert_eq!(here, there, "both openers see the same library");
    println!(
        "device read again, two concurrent openers (browser + cue-sync): {concurrent:?} total (first read alone: {cold:?})"
    );
}
