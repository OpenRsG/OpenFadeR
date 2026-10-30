//! Fixtures shared by the export and read tests.
#![allow(dead_code)]

use openfader_plugin::library::host::{self, Host, HostRequest};
use openfader_plugin::library::{Analysis, Beat, Buf, Cue, Playlist, Snapshot, Track};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct Dir(pub PathBuf);

impl Dir {
    pub fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("virtualdj-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    pub fn join(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A mono 8-bit WAV of `n` samples: the smallest real audio file; `n` makes files differ in size.
pub fn wav(n: u32) -> Vec<u8> {
    let mut b = b"RIFF".to_vec();
    b.extend((36 + n).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend([1, 0, 1, 0]);
    b.extend(8000u32.to_le_bytes());
    b.extend(8000u32.to_le_bytes());
    b.extend([1, 0, 8, 0]);
    b.extend(b"data");
    b.extend(n.to_le_bytes());
    b.extend(std::iter::repeat_n(128u8, n as usize));
    b
}

pub fn track(src: &Dir, file: &str, title: &str, n: u32) -> Track {
    std::fs::write(src.join(file), wav(n)).unwrap();
    Track {
        title: title.into(),
        artist: "Art & <Ist>".into(),
        album: "Album".into(),
        genre: "House".into(),
        label: "Lab\"el".into(),
        key: "8A".into(),
        bpm_x100: 12000,
        length_s: 90,
        file: Some(src.join(file)),
        rating: 4,
        comment: "line one\nline two & <more>".into(),
        date_added: "2026-03-04".into(),
        bitrate: 320,
        ..Track::default()
    }
}

pub fn cue(
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

/// 120 BPM grid, first downbeat at 250 ms (a half beat of lead-in before it).
pub fn analysis(cues: Vec<Cue>) -> Analysis {
    let beats = (0..8)
        .map(|i| Beat {
            bar_pos: (i % 4 + 1) as u16,
            bpm_x100: 12000,
            time_ms: 250 + i * 500,
        })
        .collect();
    Analysis {
        beats,
        cues,
        key: Some("8A".into()),
        ..Analysis::default()
    }
}

pub fn playlist(name: &str, depth: usize, is_folder: bool) -> Playlist {
    Playlist {
        id: name.into(),
        name: name.into(),
        depth,
        is_folder,
    }
}

/// Two tracks (the first with a grid, hot and memory cues and loops), a folder with a list, a top-level list.
pub fn snapshot(src: &Dir) -> Snapshot {
    let cues = vec![
        cue(3, 20_000, Some(22_000), Some([0, 255, 0]), None),
        cue(1, 10_000, None, Some([255, 0, 0]), Some("Drop")),
        cue(0, 5_000, None, None, None),
        cue(0, 30_000, Some(32_000), None, None),
    ];
    Snapshot {
        tracks: vec![
            (
                track(src, "a.wav", "Title \"A\" & <1> 'x'\nb", 400),
                Some(analysis(cues)),
            ),
            (track(src, "b.wav", "Plain", 700), None),
        ],
        playlists: vec![
            playlist("Sets", 0, true),
            playlist("Warm up", 1, false),
            playlist("Root/List", 0, false),
        ],
        members: vec![vec![], vec![0, 1], vec![1]],
        ..Snapshot::default()
    }
}

thread_local! {
    static CANCEL_AT: std::cell::RefCell<Option<(String, usize)>> = const { std::cell::RefCell::new(None) };
    static SEEN: std::cell::RefCell<Vec<(String, u64, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

unsafe extern "C" fn call(req: *const u8, len: usize) -> Buf {
    let req: HostRequest =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(req, len) }).unwrap();
    let v: Value = match req {
        HostRequest::Analysis { .. } => json!({ "analysis": null }),
        HostRequest::AudioInfo { .. } => json!({ "sample_rate": 44100 }),
        HostRequest::Progress {
            phase,
            bytes_done,
            bytes_total,
            ..
        } => {
            let cancel = CANCEL_AT.with(|c| {
                let mut c = c.borrow_mut();
                match c.as_mut() {
                    Some((p, skip)) if *p == phase => {
                        if *skip == 0 {
                            true
                        } else {
                            *skip -= 1;
                            false
                        }
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
        ptr: Box::into_raw(out) as *mut u8,
        len,
    }
}

unsafe extern "C" fn free(buf: Buf) {
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.ptr, buf.len)) });
}

/// Runs `f` with a fake host table on this thread: no analysis, progress reports recorded ([`reports`]), cancel at
/// [`cancel_at`].
pub fn with_host<R>(f: impl FnOnce() -> R) -> R {
    CANCEL_AT.with(|c| *c.borrow_mut() = None);
    SEEN.with(|s| s.borrow_mut().clear());
    host::serving(&Host { call, free }, f)
}

/// The user cancels the job at the `n`-th (from 0) progress report of `phase`.
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
