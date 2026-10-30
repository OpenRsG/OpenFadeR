//! Fixtures: tiny audio files with the metadata layouts the plugin edits, a snapshot, and independent decoders of what it writes.
#![allow(dead_code)]

use openfader_plugin::library::{
    host::{self, Host, HostRequest},
    Analysis, Beat, Buf, Cue, ExportOptions, ExportReport, LibraryPlugin, Playlist, Snapshot,
    Track,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("serato-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("lib")).unwrap();
    std::fs::create_dir_all(d.join("usb")).unwrap();
    d
}

/// Deterministic "audio" bytes.
pub fn audio(n: usize) -> Vec<u8> {
    let mut x = 0x2545_F491u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

pub fn id3_frame(id: &str, body: &[u8]) -> Vec<u8> {
    let mut v = id.as_bytes().to_vec();
    v.extend((body.len() as u32).to_be_bytes());
    v.extend([0, 0]);
    v.extend(body);
    v
}

/// An ID3v2.3 tag of `total` bytes (frames, then zero padding): a title and an artist.
pub fn id3v23(total: usize) -> Vec<u8> {
    let mut body = id3_frame("TIT2", b"\0Song");
    body.extend(id3_frame("TPE1", b"\0Artist"));
    let mut v = vec![b'I', b'D', b'3', 3, 0, 0];
    let n = total - 10;
    v.extend([
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]);
    v.extend(body);
    v.resize(total, 0);
    v
}

/// The size an [`id3v23`] needs for its frames and no padding.
pub const ID3_TIGHT: usize = 10 + 15 + 17;

pub fn mp3(tag_total: usize) -> Vec<u8> {
    [id3v23(tag_total), audio(6000)].concat()
}

fn flac_block(ty: u8, last: bool, body: &[u8]) -> Vec<u8> {
    let mut v = vec![ty | if last { 0x80 } else { 0 }];
    v.extend(&(body.len() as u32).to_be_bytes()[1..]);
    v.extend(body);
    v
}

/// STREAMINFO, VORBIS_COMMENT (title, artist) and, if `pad`, a PADDING block of that many bytes.
pub fn flac(pad: Option<usize>) -> Vec<u8> {
    let mut vorbis = 4u32.to_le_bytes().to_vec();
    vorbis.extend(b"test");
    vorbis.extend(2u32.to_le_bytes());
    for c in ["TITLE=Song", "ARTIST=Artist"] {
        vorbis.extend((c.len() as u32).to_le_bytes());
        vorbis.extend(c.as_bytes());
    }
    let mut v = b"fLaC".to_vec();
    v.extend(flac_block(0, false, &[7; 34]));
    v.extend(flac_block(4, pad.is_none(), &vorbis));
    if let Some(p) = pad {
        v.extend(flac_block(1, true, &vec![0; p]));
    }
    [v, audio(5000)].concat()
}

pub fn atom(ty: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut v = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    v.extend(ty);
    v.extend(payload);
    v
}

/// Payload of the audio data of [`m4a`].
pub fn m4a_audio() -> Vec<u8> {
    audio(3000)
}

/// `ftyp`, `moov` (one track with an `stco`, a title in `ilst`, a `free` atom of `free` bytes in `meta` when given), `mdat`.
pub fn m4a(free: Option<usize>) -> Vec<u8> {
    let ftyp = atom(b"ftyp", b"M4A \0\0\0\0M4A mp42isom");
    let build = |offset: u32| {
        let stco = atom(
            b"stco",
            &[&[0; 4][..], &1u32.to_be_bytes(), &offset.to_be_bytes()].concat(),
        );
        let trak = atom(
            b"trak",
            &atom(b"mdia", &atom(b"minf", &atom(b"stbl", &stco))),
        );
        let name = atom(
            b"\xa9nam",
            &atom(b"data", &[&[0, 0, 0, 1, 0, 0, 0, 0][..], b"Song"].concat()),
        );
        let hdlr = atom(b"hdlr", &[&[0; 8][..], b"mdirappl", &[0; 9]].concat());
        let mut meta = [vec![0; 4], hdlr, atom(b"ilst", &name)].concat();
        if let Some(f) = free {
            meta.extend(atom(b"free", &vec![0; f - 8]));
        }
        atom(
            b"moov",
            &[
                atom(b"mvhd", &[0; 100]),
                trak,
                atom(b"udta", &atom(b"meta", &meta)),
            ]
            .concat(),
        )
    };
    let moov_len = build(0).len();
    let offset = (ftyp.len() + moov_len + 8) as u32;
    [ftyp, build(offset), atom(b"mdat", &m4a_audio())].concat()
}

/// A RIFF/WAVE file without an `id3 ` chunk.
pub fn wav() -> Vec<u8> {
    let data = audio(2000);
    let fmt = [1, 0, 1, 0, 0x44, 0xac, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0];
    let mut body = b"WAVE".to_vec();
    body.extend(b"fmt ");
    body.extend((fmt.len() as u32).to_le_bytes());
    body.extend(fmt);
    body.extend(b"data");
    body.extend((data.len() as u32).to_le_bytes());
    body.extend(data);
    [
        b"RIFF".to_vec(),
        (body.len() as u32).to_le_bytes().to_vec(),
        body,
    ]
    .concat()
}

pub fn track(file: &Path, title: &str) -> Track {
    Track {
        title: title.into(),
        artist: "Artist".into(),
        album: "Album".into(),
        genre: "House".into(),
        key: "Am".into(),
        bpm_x100: 12000,
        length_s: 200,
        bitrate: 320,
        sample_rate: 44100,
        date_added: "2024-05-06".into(),
        file: Some(file.to_path_buf()),
        ..Track::default()
    }
}

/// A hot cue with colour and name, a hot-cue loop, a memory cue; 16 beats at 120 BPM from 250 ms.
pub fn analysis(cue_ms: u32) -> Analysis {
    let cue = |hot_cue, time_ms, loop_end_ms, color, comment: Option<&str>| Cue {
        hot_cue,
        time_ms,
        loop_end_ms,
        color,
        comment: comment.map(str::to_string),
    };
    Analysis {
        cues: vec![
            cue(1, cue_ms, None, Some([1, 2, 3]), Some("Drop")),
            cue(2, 30000, Some(32000), None, None),
            cue(0, 500, None, None, None),
        ],
        beats: (0..16)
            .map(|i| Beat {
                bar_pos: i % 4 + 1,
                bpm_x100: 12000,
                time_ms: 250 + 500 * u32::from(i),
            })
            .collect(),
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

/// One track in the playlist "Mix".
pub fn snapshot(t: Track, a: Analysis) -> Snapshot {
    Snapshot {
        tracks: vec![(t, Some(a))],
        playlists: vec![playlist("Mix", 0, false)],
        members: vec![vec![0]],
        colors: vec![(0, 5)],
        ..Snapshot::default()
    }
}

// ---- Independent decoders of what the plugin writes -------------------------------------------------------------

fn unb64(s: &[u8]) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64: {c:#x}"),
    };
    let mut out = Vec::new();
    let clean: Vec<u8> = s.iter().copied().filter(|&c| c != b'\n').collect();
    for ch in clean.chunks(4) {
        let n = ch.iter().fold(0u32, |a, &c| a << 6 | u32::from(val(c))) << (6 * (4 - ch.len()));
        for i in 0..(ch.len() * 6 / 8) {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    out
}

pub fn b64(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = c.iter().fold(0u32, |a, &b| a << 8 | u32::from(b)) << (8 * (3 - c.len()));
        for i in 0..=c.len() {
            s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

/// `(id, body)` of the frames of the ID3v2.3/2.4 tag at the start of `b`.
pub fn id3_frames(b: &[u8]) -> Vec<(String, Vec<u8>)> {
    assert_eq!(&b[..3], b"ID3");
    let v4 = b[3] == 4;
    let end = 10 + b[6..10].iter().fold(0usize, |a, &x| a << 7 | x as usize);
    let (mut pos, mut out) = (10, Vec::new());
    while pos + 10 <= end && b[pos] != 0 {
        let s = &b[pos + 4..pos + 8];
        let size = if v4 {
            s.iter().fold(0usize, |a, &x| a << 7 | x as usize)
        } else {
            u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize
        };
        out.push((
            String::from_utf8(b[pos..pos + 4].to_vec()).unwrap(),
            b[pos + 10..pos + 10 + size].to_vec(),
        ));
        pos += 10 + size;
    }
    out
}

/// Size of the ID3 tag at the start of `b`.
pub fn id3_size(b: &[u8]) -> usize {
    10 + b[6..10].iter().fold(0usize, |a, &x| a << 7 | x as usize)
}

/// The data of the GEOB frame `desc` of an ID3 tag.
pub fn geob(frames: &[(String, Vec<u8>)], desc: &str) -> Option<Vec<u8>> {
    frames
        .iter()
        .filter(|(id, _)| id == "GEOB")
        .find_map(|(_, b)| {
            assert_eq!(b[0], 0);
            let mut parts = b[1..].splitn(4, |&c| c == 0);
            let (mime, file, d) = (parts.next()?, parts.next()?, parts.next()?);
            assert_eq!(mime, b"application/octet-stream");
            assert!(file.is_empty());
            (d == desc.as_bytes()).then(|| parts.next().unwrap().to_vec())
        })
}

/// The entries of `Serato Markers2` GEOB data, checking the framing: header, line length, terminator.
pub fn markers2(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    assert_eq!(&data[..2], &[1, 1]);
    let text = &data[2..];
    assert!(text
        .split(|&c| c == b'\n')
        .all(|l| l.len() <= 72 && !l.is_empty()));
    let inner = unb64(text);
    assert_eq!(&inner[..2], &[1, 1]);
    let (mut pos, mut out) = (2, Vec::new());
    while inner[pos] != 0 {
        let n = inner[pos..].iter().position(|&c| c == 0).unwrap();
        let name = String::from_utf8(inner[pos..pos + n].to_vec()).unwrap();
        let len = u32::from_be_bytes(inner[pos + n + 1..pos + n + 5].try_into().unwrap()) as usize;
        out.push((name, inner[pos + n + 5..pos + n + 5 + len].to_vec()));
        pos += n + 5 + len;
    }
    assert_eq!(
        pos,
        inner.len() - 1,
        "only the terminator follows the entries"
    );
    out
}

/// `(index, ms, rgb, name)` of a `CUE` entry.
pub fn cue(d: &[u8]) -> (u8, u32, [u8; 3], String) {
    assert_eq!(
        (d[0], d[6], &d[10..12], d[d.len() - 1]),
        (0, 0, &[0u8, 0][..], 0)
    );
    (
        d[1],
        u32::from_be_bytes(d[2..6].try_into().unwrap()),
        d[7..10].try_into().unwrap(),
        String::from_utf8(d[12..d.len() - 1].to_vec()).unwrap(),
    )
}

/// `(index, start, end, rgb, locked, name)` of a `LOOP` entry.
pub fn lp(d: &[u8]) -> (u8, u32, u32, [u8; 3], bool, String) {
    assert_eq!(
        (d[0], &d[10..14], d[14], d[18], d[d.len() - 1]),
        (0, &[0xffu8; 4][..], 0, 0, 0)
    );
    (
        d[1],
        u32::from_be_bytes(d[2..6].try_into().unwrap()),
        u32::from_be_bytes(d[6..10].try_into().unwrap()),
        d[15..18].try_into().unwrap(),
        d[19] != 0,
        String::from_utf8(d[20..d.len() - 1].to_vec()).unwrap(),
    )
}

/// `(seconds, beats to the next marker or BPM)` of each `Serato BeatGrid` marker: the last one carries the BPM.
pub fn beatgrid(d: &[u8]) -> Vec<(f32, f32)> {
    assert_eq!(&d[..2], &[1, 0]);
    let n = u32::from_be_bytes(d[2..6].try_into().unwrap()) as usize;
    assert_eq!(d.len(), 6 + 8 * n + 1);
    (0..n)
        .map(|i| {
            let m = &d[6 + 8 * i..14 + 8 * i];
            let pos = f32::from_be_bytes(m[..4].try_into().unwrap());
            let v = if i + 1 < n {
                u32::from_be_bytes(m[4..].try_into().unwrap()) as f32
            } else {
                f32::from_be_bytes(m[4..].try_into().unwrap())
            };
            (pos, v)
        })
        .collect()
}

/// The decoded value of a `field` (FLAC comment value, MP4 item value): the GEOB data after `mime\0\0name\0`.
pub fn field(text: &str, name: &str) -> Vec<u8> {
    let raw = unb64(text.as_bytes());
    let head = [
        b"application/octet-stream\0\0".as_slice(),
        name.as_bytes(),
        b"\0",
    ]
    .concat();
    assert_eq!(&raw[..head.len()], &head[..]);
    raw[head.len()..].to_vec()
}

/// Every check the standard test analysis must decode to, on the entries and the grid.
pub fn assert_standard(markers: &[(String, Vec<u8>)], grid: &[u8], cue_ms: u32) {
    let get = |n: &str| {
        markers
            .iter()
            .filter(|(k, _)| k == n)
            .map(|(_, d)| d.as_slice())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        get("COLOR"),
        vec![&[0, 0x99, 0xff, 0x99][..]],
        "host colour 5 is green"
    );
    let cues: Vec<_> = get("CUE").into_iter().map(cue).collect();
    assert_eq!(
        cues,
        vec![
            (0, cue_ms, [1, 2, 3], "Drop".to_string()),
            (1, 30000, [0xcc, 0x88, 0], String::new())
        ]
    );
    let loops: Vec<_> = get("LOOP").into_iter().map(lp).collect();
    assert_eq!(
        loops,
        vec![(0, 30000, 32000, [0x27, 0xaa, 0xe1], false, String::new())]
    );
    assert_eq!(get("BPMLOCK"), vec![&[0][..]]);
    let g = beatgrid(grid);
    assert_eq!(g.len(), 1);
    assert!(
        (g[0].0 - 0.25).abs() < 1e-6 && (g[0].1 - 120.0).abs() < 1e-3,
        "{g:?}"
    );
}

// ---- A fake host: progress reports, cancels, and what the user would read ------------------------------------------------

thread_local! {
    /// Cancel once a progress report names this phase with at least this many items done.
    static CANCEL_AT: RefCell<Option<(String, usize)>> = const { RefCell::new(None) };
    static SEEN: RefCell<Vec<(String, usize, u64, u64)>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" fn call(req: *const u8, len: usize) -> Buf {
    // SAFETY: `req`/`len` are the plugin's request buffer (see `host::ask`).
    let req: HostRequest =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(req, len) }).unwrap();
    let v: Value = match req {
        HostRequest::Analysis { .. } => json!({ "analysis": null }),
        HostRequest::AudioInfo { .. } => json!({ "sample_rate": 44100 }),
        HostRequest::Progress {
            phase,
            done,
            bytes_done,
            bytes_total,
            ..
        } => {
            let cancel = CANCEL_AT.with(|c| {
                c.borrow()
                    .as_ref()
                    .is_some_and(|(p, n)| *p == phase && done >= *n)
            });
            SEEN.with(|s| s.borrow_mut().push((phase, done, bytes_done, bytes_total)));
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

/// Runs `f` as a job served by the fake host: no cancel until [`cancel_at`], every progress report is recorded.
pub fn with_host<R>(f: impl FnOnce() -> R) -> R {
    CANCEL_AT.with(|c| *c.borrow_mut() = None);
    SEEN.with(|s| s.borrow_mut().clear());
    host::serving(&Host { call, free }, f)
}

/// The user cancels the job at the first report of `phase`.
pub fn cancel_at(phase: &str) {
    cancel_at_item(phase, 0);
}

/// The user cancels the job at the report of `phase` that says `done` items are finished.
pub fn cancel_at_item(phase: &str, done: usize) {
    CANCEL_AT.with(|c| *c.borrow_mut() = Some((phase.to_owned(), done)));
}

/// `(phase, done, bytes_done, bytes_total)` of every progress report of the last job on this thread.
pub fn reports() -> Vec<(String, usize, u64, u64)> {
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

/// An export under the fake host; every text it answers the user with must be translated.
pub fn export_host(
    usb: &Path,
    snap: &Snapshot,
    opts: &ExportOptions,
) -> Result<ExportReport, String> {
    let out = with_host(|| compat_serato::Plugin::export(usb, snap, opts, &Value::Null));
    assert_answer_translated(&out);
    out
}

/// Every text of an export answer is translated.
pub fn assert_answer_translated(out: &Result<ExportReport, String>) {
    match out {
        Ok(r) => r
            .warnings
            .iter()
            .chain(&r.notes)
            .chain(r.plan.iter().flat_map(|p| &p.blocked))
            .for_each(|t| assert_translated(t)),
        Err(e) => {
            if e != "cancelled" && !e.contains("injected") {
                assert_translated(e)
            }
        }
    }
}

// ---- The tree of a device --------------------------------------------------------------------------------------------------

/// Every file and folder below `root` (folders end in `/`) with the bytes and the time of change of the files.
pub fn tree(root: &Path) -> BTreeMap<String, (Vec<u8>, Option<SystemTime>)> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, Option<SystemTime>)>) {
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
                out.insert(format!("{rel}/"), (Vec::new(), None));
                walk(base, &p, out);
            } else {
                let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                out.insert(rel, (std::fs::read(&p).unwrap(), t));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A copy of the tree at `from` (files keep their bytes, not their times).
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
