//! Fixtures shared by the export and read tests: temp dirs, synthetic tracks and a snapshot with cues, a tempo change and lists.
#![allow(dead_code)]

use openfader_plugin::library::{
    host::{self, Host, HostRequest},
    Analysis, Beat, Buf, Cue, ExportOptions, LibraryPlugin, Playlist, Snapshot, Track,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::PathBuf;

pub const RATE: u32 = 44100;

/// A temp dir that is gone again when the test ends.
pub struct Dirs(PathBuf);

impl Dirs {
    pub fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("engine-dj-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("stick")).unwrap();
        Self(d)
    }
    pub fn src(&self) -> PathBuf {
        self.0.join("src")
    }
    pub fn stick(&self) -> PathBuf {
        self.0.join("stick")
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 8-bit mono silence of `secs` seconds: a valid WAV, small enough to copy often.
pub fn wav(secs: u32) -> Vec<u8> {
    let data = vec![128u8; (RATE * secs) as usize];
    let mut b = [
        b"RIFF".as_slice(),
        &(36 + data.len() as u32).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
    ]
    .concat();
    b.extend([1, 0, 1, 0]);
    b.extend(RATE.to_le_bytes());
    b.extend(RATE.to_le_bytes());
    b.extend([1, 0, 8, 0]);
    b.extend(b"data");
    b.extend((data.len() as u32).to_le_bytes());
    b.extend(data);
    b
}

pub fn track(d: &Dirs, file: &str, title: &str, artist: &str, secs: i64) -> Track {
    let f = d.src().join(file);
    std::fs::write(&f, wav(secs as u32)).unwrap();
    Track {
        title: title.into(),
        artist: artist.into(),
        album: "Album".into(),
        genre: "House".into(),
        label: "Label".into(),
        key: "8A".into(),
        bpm_x100: 12000,
        length_s: secs,
        file: Some(f),
        rating: 4,
        comment: "nice".into(),
        date_added: "2026-01-02".into(),
        bitrate: 1411,
        sample_rate: RATE,
        ..Default::default()
    }
}

pub fn cue(
    hot: u16,
    ms: u32,
    end: Option<u32>,
    color: Option<[u8; 3]>,
    comment: Option<&str>,
) -> Cue {
    Cue {
        hot_cue: hot,
        time_ms: ms,
        loop_end_ms: end,
        color,
        comment: comment.map(Into::into),
    }
}

/// 120 BPM for 16 beats (0 to 8 s), then 150 BPM for 8 beats (to 11.2 s).
pub fn beats() -> Vec<Beat> {
    (0..=24u32)
        .map(|k| {
            let (time_ms, bpm_x100) = if k <= 16 {
                (500 * k, 12000)
            } else {
                (8000 + 400 * (k - 16), 15000)
            };
            Beat {
                bar_pos: (k % 4) as u16 + 1,
                bpm_x100,
                time_ms,
            }
        })
        .collect()
}

pub fn analysis() -> Analysis {
    Analysis {
        beats: beats(),
        cues: vec![
            cue(1, 750, None, Some([0xff, 0x00, 0x17]), Some("Drop é")),
            cue(3, 1500, None, None, None),
            cue(0, 1250, None, None, None),
            cue(0, 3250, Some(5250), None, Some("Loop")),
            cue(2, 6000, Some(7000), None, None),
            cue(0, 9000, None, None, None),
            cue(9, 9500, None, None, None),
        ],
        three_band_preview: (0..1200)
            .map(|i| [(i % 256) as u8, (i * 2 % 256) as u8, 7])
            .collect(),
        key: Some("8A".into()),
        ..Default::default()
    }
}

pub fn list(name: &str, depth: usize, folder: bool) -> Playlist {
    Playlist {
        id: name.into(),
        name: name.into(),
        depth,
        is_folder: folder,
    }
}

/// A (analysed), B (no analysis, key from the track only), C (audio file missing); Sets/Warm = [A, B], Top = [B, A, B].
pub fn setup(d: &Dirs) -> Snapshot {
    let a = track(d, "a.wav", "Alpha é", "Art/ist: X", 12);
    let mut b = track(d, "b.wav", "Beta", "Other", 8);
    b.key = "11B".into();
    b.sample_rate = 0;
    let mut c = track(d, "c.wav", "Gamma", "Other", 1);
    c.file = Some(d.src().join("missing.wav"));
    Snapshot {
        tracks: vec![(a, Some(analysis())), (b, None), (c, None)],
        playlists: vec![
            list("Sets", 0, true),
            list("Warm", 1, false),
            list("Top", 0, false),
        ],
        members: vec![vec![], vec![0, 1], vec![1, 0, 1]],
        ..Default::default()
    }
}

pub fn export(
    d: &Dirs,
    snap: &Snapshot,
) -> Result<openfader_plugin::library::ExportReport, String> {
    with_host(|| {
        compat_engine_dj::Plugin::export(&d.stick(), snap, &ExportOptions::default(), &Value::Null)
    })
}

pub fn export_schema(
    d: &Dirs,
    snap: &Snapshot,
    schema: &str,
) -> Result<openfader_plugin::library::ExportReport, String> {
    let mut options = ExportOptions::default();
    options
        .config
        .insert(compat_engine_dj::SCHEMA_OPTION.into(), schema.into());
    with_host(|| compat_engine_dj::Plugin::export(&d.stick(), snap, &options, &Value::Null))
}

thread_local! {
    static CANCEL_AT: RefCell<Option<(String, usize)>> = const { RefCell::new(None) };
    static SEEN: RefCell<Vec<(String, u64, u64)>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" fn call(req: *const u8, len: usize) -> Buf {
    // SAFETY: `req`/`len` are the plugin's request buffer (see `host::ask`).
    let req: HostRequest =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(req, len) }).unwrap();
    let v: Value = match req {
        HostRequest::Analysis { .. } => json!({ "analysis": null }),
        HostRequest::AudioInfo { .. } => json!({ "sample_rate": RATE }),
        HostRequest::Progress {
            phase,
            bytes_done,
            bytes_total,
            ..
        } => {
            let cancel = CANCEL_AT.with(|c| {
                let mut c = c.borrow_mut();
                match c.as_mut() {
                    Some((p, n)) if *p == phase => {
                        *n -= 1;
                        *n == 0
                    }
                    _ => false,
                }
            });
            SEEN.with(|s| s.borrow_mut().push((phase, bytes_done, bytes_total)));
            json!({ "cancel": cancel })
        }
    };
    let out = serde_json::to_vec(&v).unwrap().into_boxed_slice();
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

/// Runs `f` with the fake host: it analyses nothing, answers 44.1 kHz and records every progress report.
pub fn with_host<R>(f: impl FnOnce() -> R) -> R {
    CANCEL_AT.with(|c| *c.borrow_mut() = None);
    SEEN.with(|s| s.borrow_mut().clear());
    host::serving(&Host { call, free }, f)
}

/// The user cancels the job at the `n`-th report of `phase` (1: the first).
pub fn cancel_at(phase: &str, n: usize) {
    CANCEL_AT.with(|c| *c.borrow_mut() = Some((phase.to_owned(), n)));
}

/// `(phase, bytes_done, bytes_total)` of every progress report of the last job on this thread.
pub fn reports() -> Vec<(String, u64, u64)> {
    SEEN.with(|s| s.borrow().clone())
}

/// Panics unless the host would show `text` in every language of the plugin's `locales/` (the rule of the host's `locale.rs`: the
/// text itself is a key, or matches a key whose `{}` stand for values of at least one character).
pub fn assert_translated(text: &str) {
    fn matches(key: &str, text: &str) -> bool {
        let parts: Vec<&str> = key.split("{}").collect();
        let (first, rest) = parts.split_first().unwrap();
        let Some((last, middle)) = rest.split_last() else {
            return false;
        };
        let Some(mut s) = text.strip_prefix(first).and_then(|t| t.strip_suffix(last)) else {
            return false;
        };
        for part in middle {
            let start = s.char_indices().nth(1).map_or(s.len(), |(i, _)| i);
            match s.get(start..).and_then(|r| r.find(part)) {
                Some(i) => s = &s[start + i + part.len()..],
                None => return false,
            }
        }
        !s.is_empty()
    }
    for lang in ["de", "es", "fr", "it", "nl", "pt"] {
        let file = format!("{}/locales/{lang}.json", env!("CARGO_MANIFEST_DIR"));
        let map: std::collections::HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let found = map.get(text).is_some_and(|t| !t.is_empty())
            || map
                .iter()
                .any(|(k, v)| !v.is_empty() && k.contains("{}") && matches(k, text));
        assert!(found, "{lang} has no translation of: {text}");
    }
}

/// Every file and folder below `root` (folders end in `/`) with the bytes of the files.
pub fn tree(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(
        base: &std::path::Path,
        dir: &std::path::Path,
        out: &mut std::collections::BTreeMap<String, Vec<u8>>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

pub fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

/// A library of the plugin's own reader: `(tracks, lists that are no folders, cues)`.
pub fn read_counts(stick: &std::path::Path) -> (usize, usize, usize) {
    let src = compat_engine_dj::reader::open(stick).unwrap();
    let tracks = src.tracks(None).unwrap();
    let lists = src
        .playlists()
        .unwrap()
        .iter()
        .filter(|p| !p.is_folder)
        .count();
    let cues = tracks
        .iter()
        .map(|t| src.analysis(t).map_or(0, |a| a.cues.len()))
        .sum();
    (tracks.len(), lists, cues)
}
