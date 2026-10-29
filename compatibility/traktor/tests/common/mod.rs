//! Shared by the integration tests: a fake host (progress, cancel), synthetic sticks in temp dirs (never a real stick), and the
//! checks every test makes of what the plugin answers.

#![allow(dead_code)] // each test binary uses a subset

use openfader_plugin::library::{
    host::{self, Host, HostRequest},
    Analysis, Beat, Buf, Cue, ExportOptions, ExportReport, LibraryPlugin, Playlist, Snapshot,
    Track,
};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use compat_traktor::{Plugin, FILE};

thread_local! {
    static CANCEL: Cell<bool> = const { Cell::new(false) };
    /// `(phase, nth)`: the user cancels at the `nth` (from 0) progress report of `phase`.
    static CANCEL_AT: RefCell<Option<(String, usize)>> = const { RefCell::new(None) };
    static SEEN: RefCell<Vec<(String, u64, u64)>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" fn call(req: *const u8, len: usize) -> Buf {
    // SAFETY: the plugin passes a buffer of `len` bytes that lives for the call.
    let req: HostRequest = serde_json::from_slice(unsafe { std::slice::from_raw_parts(req, len) })
        .expect("a host request");
    let v: Value = match req {
        HostRequest::Analysis { .. } => json!({ "analysis": null }),
        HostRequest::AudioInfo { .. } => json!({ "sample_rate": 44100 }),
        HostRequest::Progress {
            phase,
            bytes_done,
            bytes_total,
            ..
        } => {
            let nth = SEEN.with(|s| s.borrow().iter().filter(|r| r.0 == phase).count());
            let cancel = CANCEL.get()
                || CANCEL_AT.with(|c| {
                    c.borrow()
                        .as_ref()
                        .is_some_and(|(p, n)| *p == phase && *n == nth)
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

/// Runs `f` as a job of a host that answers every progress report with "go on".
pub fn with_host<R>(f: impl FnOnce() -> R) -> R {
    CANCEL.set(false);
    CANCEL_AT.with(|c| *c.borrow_mut() = None);
    SEEN.with(|s| s.borrow_mut().clear());
    host::serving(&Host { call, free }, f)
}

/// The user cancels the job at the `nth` (from 0) progress report of `phase`.
pub fn cancel_at(phase: &str, nth: usize) {
    CANCEL_AT.with(|c| *c.borrow_mut() = Some((phase.to_owned(), nth)));
}

/// `(phase, bytes_done, bytes_total)` of every progress report of the last job on this thread.
pub fn reports() -> Vec<(String, u64, u64)> {
    SEEN.with(|s| s.borrow().clone())
}

/// A raw export under the fake host.
pub fn export_raw(root: &Path, snap: &Snapshot, o: &ExportOptions) -> Result<ExportReport, String> {
    with_host(|| Plugin::export(root, snap, o, &Value::Null))
}

/// An export under the fake host; every text it answers the user with must be translated.
pub fn export(root: &Path, snap: &Snapshot, o: &ExportOptions) -> Result<ExportReport, String> {
    let out = export_raw(root, snap, o);
    match &out {
        Ok(r) => r
            .warnings
            .iter()
            .chain(&r.notes)
            .chain(r.plan.iter().flat_map(|p| &p.blocked))
            .for_each(|t| assert_translated(t)),
        Err(e) => assert_translated(e),
    }
    reports()
        .iter()
        .for_each(|(phase, ..)| assert_translated(phase));
    out
}

/// A fresh `<tmp>/traktor-it-<tag>-<pid>` with a `src` folder for the audio; `device()` is where the stick is mounted.
pub struct Root(pub PathBuf);

impl Root {
    pub fn new(tag: &str) -> Self {
        let r = std::env::temp_dir().join(format!("traktor-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        std::fs::create_dir_all(r.join("src")).unwrap();
        Self(r)
    }
    pub fn src(&self) -> PathBuf {
        self.0.join("src")
    }
    /// The stick; does not exist until something makes it.
    pub fn dev(&self) -> PathBuf {
        self.0.join("device")
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A mono 8 kHz 8-bit WAV of `len` data bytes.
pub fn wav(path: &Path, len: usize) {
    let data = vec![len as u8; len];
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

/// Track `i` (0..4): `a.wav` … `d.wav`, each of its own size, title `T<i>`.
pub fn track(r: &Root, i: usize) -> Track {
    let f = r.src().join(format!("{}.wav", ["a", "b", "c", "d"][i]));
    if !f.exists() {
        wav(&f, 800 + i * 400);
    }
    Track {
        title: format!("T{i}"),
        artist: "Art".into(),
        album: "Alb".into(),
        genre: "House".into(),
        bpm_x100: 12800,
        length_s: 1,
        bitrate: 320,
        date_added: "2024-03-05".into(),
        file: Some(f),
        ..Track::default()
    }
}

/// `n` hot cues (pads 1..=n) on a 128 BPM grid.
pub fn analysis(n: u16) -> Analysis {
    Analysis {
        beats: (0..8u32)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12800,
                time_ms: 100 + i * 469,
            })
            .collect(),
        cues: (1..=n)
            .map(|h| Cue {
                hot_cue: h,
                time_ms: u32::from(h) * 1000,
                loop_end_ms: None,
                color: None,
                comment: None,
            })
            .collect(),
        ..Analysis::default()
    }
}

/// The tracks `which` (indexes 0..4) and the lists P1 = first two of them, P2 = third, P3 = first and fourth, as far as the
/// tracks exist; track T0 carries `cues_of_a` cues, the others one.
pub fn snapshot(r: &Root, which: &[usize], cues_of_a: u16) -> Snapshot {
    let at = |t: usize| which.iter().position(|w| *w == t);
    let lists: Vec<(&str, Vec<usize>)> = vec![
        ("P1", [at(0), at(1)].into_iter().flatten().collect()),
        ("P2", [at(2)].into_iter().flatten().collect()),
        ("P3", [at(0), at(3)].into_iter().flatten().collect()),
    ];
    Snapshot {
        tracks: which
            .iter()
            .map(|&i| {
                (
                    track(r, i),
                    Some(analysis(if i == 0 { cues_of_a } else { 1 })),
                )
            })
            .collect(),
        playlists: lists
            .iter()
            .filter(|(_, m)| !m.is_empty())
            .map(|(n, _)| Playlist {
                id: (*n).into(),
                name: (*n).into(),
                depth: 0,
                is_folder: false,
            })
            .collect(),
        members: lists
            .into_iter()
            .map(|(_, m)| m)
            .filter(|m| !m.is_empty())
            .collect(),
        ..Snapshot::default()
    }
}

/// Every file and folder below `root` (folders end in `/`) with the bytes of the files; empty when `root` does not exist.
pub fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
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
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

pub fn copy_tree(from: &Path, to: &Path) {
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

/// What the plugin's own reader finds in the collection of the stick: `(tracks, playlists, cues of T0)`.
pub fn read_back(dev: &Path) -> (usize, usize, usize) {
    let src = Plugin::open(&dev.join(FILE), &Value::Null).unwrap();
    let all = src.tracks(None).unwrap();
    let lists = src.playlists().unwrap();
    // Every playlist can be read (a track left out of the stick is left out of its list).
    for l in lists.iter().filter(|l| !l.is_folder) {
        src.tracks(Some(&l.id)).unwrap();
    }
    let cues = all
        .iter()
        .find(|t| t.title == "T0")
        .and_then(|t| src.analysis(t))
        .map_or(0, |a| a.cues.len());
    (
        all.len(),
        lists.iter().filter(|l| !l.is_folder).count(),
        cues,
    )
}

/// The text the host would show in every language of the plugin's `locales/` (the rule of the host's `locale.rs`: the text
/// itself is a key, or matches a key whose `{}` stand for values of at least one character).
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
