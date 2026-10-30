//! `_Serato_/database V2`: `vrsn` (a UTF-16BE version text) followed by one `otrk` record per track, each holding its fields
//! (`pfil` path relative to the drive root without a leading slash, `ttyp` file type, `tsng` title, `tart` artist, `talb`
//! album, `tgen` genre, `tcom` comment, `tlbl` label, `tkey` key, `tbpm`, `tlen`, `tbit`, `tsmp`, `tsiz`, `tadd`/`uadd` date
//! added, `utme` file time, `bbgl` beat grid locked, `bmis` missing). Field names and types: Mixxx's reader (see [`crate::tlv`]);
//! the shapes of the text values (`03:21.00`, `320.0kbps`, `44.1k`, `7.31MB`) are observed in files Serato wrote.
//!
//! Export merges: the records of other paths (and every other top-level record) stay as they are, the record of a path we write
//! is replaced where it stands, new paths are appended.

use crate::tlv::{self, rec, text, Rec};
use openfader_plugin::library::Track;
use std::collections::HashMap;

/// Location of the file below the drive root.
pub const FILE: &str = "_Serato_/database V2";
const VERSION: &str = "2.0/Serato Scratch LIVE Database";

/// The `otrk` record of `t`, whose audio is at `path` (relative to the drive root), `size` bytes, last changed at `mtime`.
pub fn track(path: &str, t: &Track, size: u64, mtime: u32) -> Vec<u8> {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    let added = added_secs(&t.date_added).unwrap_or(mtime);
    let some = |on: bool, s: String| if on { s } else { String::new() };
    let fields: [(&[u8; 4], String); 15] = [
        (b"ttyp", ext),
        (b"pfil", path.to_string()),
        (b"tsng", t.title.clone()),
        (b"tart", t.artist.clone()),
        (b"talb", t.album.clone()),
        (b"tgen", t.genre.clone()),
        (b"tcom", t.comment.clone()),
        (b"tlbl", t.label.clone()),
        (b"tkey", t.key.clone()),
        (
            b"tlen",
            some(
                t.length_s > 0,
                format!("{:02}:{:02}.00", t.length_s / 60, t.length_s % 60),
            ),
        ),
        (
            b"tbit",
            some(t.bitrate > 0, format!("{:.1}kbps", f64::from(t.bitrate))),
        ),
        (
            b"tsmp",
            some(
                t.sample_rate > 0,
                format!("{:.1}k", f64::from(t.sample_rate) / 1000.0),
            ),
        ),
        (b"tbpm", some(t.bpm_x100 > 0, t.bpm_text())),
        (b"tsiz", format!("{:.2}MB", size as f64 / 1_048_576.0)),
        (b"tadd", added.to_string()),
    ];
    let mut v: Vec<u8> = fields
        .iter()
        .filter(|(_, s)| !s.is_empty())
        .flat_map(|(tag, s)| text(tag, s))
        .collect();
    v.extend(rec(b"uadd", &added.to_be_bytes()));
    v.extend(rec(b"utme", &mtime.to_be_bytes()));
    v.extend(rec(b"bbgl", &[0]));
    v.extend(rec(b"bmis", &[0]));
    rec(b"otrk", &v)
}

/// Unix seconds of a `YYYY-MM-DD…` date.
fn added_secs(s: &str) -> Option<u32> {
    let d = chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()?;
    u32::try_from(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp()).ok()
}

/// The `pfil` path of an `otrk` record's data.
fn path_of(data: &[u8]) -> Option<String> {
    let recs = tlv::parse(data).ok()?;
    tlv::find(&recs, b"pfil").map(tlv::from_utf16)
}

/// The paths of every track record in `buf`.
pub fn paths(buf: &[u8]) -> Result<Vec<String>, String> {
    Ok(tlv::parse(buf)?
        .iter()
        .filter(|r| &r.tag == b"otrk")
        .filter_map(|r| path_of(r.data))
        .collect())
}

/// [`paths`] of a `database V2` that starts with its `vrsn` record; `Err` for anything else (Serato and the reader take it for
/// no library at all).
pub fn open(buf: &[u8]) -> Result<Vec<String>, String> {
    if !buf.starts_with(b"vrsn") {
        return Err("does not start with a vrsn record".into());
    }
    paths(buf)
}

/// A path as `pfil` and `ptrk` hold it, normalised: forward slashes (inferred: a Windows library may hold backslashes) and no
/// leading slash (a macOS path is absolute without it; the reader finds the drive root it starts at).
pub fn norm(p: &str) -> String {
    let p = if p.contains('\\') && !p.contains('/') {
        p.replace('\\', "/")
    } else {
        p.to_string()
    };
    p.trim_start_matches('/').to_string()
}

/// The number at the start of `s` (`320.0kbps`, `44.1k`).
fn leading_f64(s: &str) -> Option<f64> {
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    s[..end].parse().ok()
}

/// Seconds of a `mm:ss.xx` (or `h:mm:ss`) length text.
fn seconds(s: &str) -> i64 {
    s.split(':')
        .try_fold(0.0, |a, p| {
            p.trim().parse::<f64>().ok().map(|x| a * 60.0 + x)
        })
        .map_or(0, |v: f64| v.round() as i64)
}

/// The track of one `otrk` record's fields and its normalised path; `file` stays `None` (the caller knows the drive root).
/// Text shapes as in [`track`]; `uadd` (seconds since 1970) wins over the `tadd` text. `None`: no `pfil`.
fn read_track(f: &[Rec]) -> Option<(String, Track)> {
    let text = |tag: &[u8; 4]| {
        tlv::find(f, tag)
            .map(|d| tlv::from_utf16(d).trim_matches('\0').to_string())
            .unwrap_or_default()
    };
    let path = norm(&text(b"pfil"));
    if path.is_empty() {
        return None;
    }
    let added = tlv::find(f, b"uadd")
        .and_then(|d| <[u8; 4]>::try_from(d).ok())
        .map(|b| u64::from(u32::from_be_bytes(b)))
        .or_else(|| text(b"tadd").parse().ok());
    let t = Track {
        title: text(b"tsng"),
        artist: text(b"tart"),
        album: text(b"talb"),
        genre: text(b"tgen"),
        label: text(b"tlbl"),
        key: text(b"tkey"),
        comment: text(b"tcom"),
        bpm_x100: leading_f64(&text(b"tbpm")).map_or(0, |v| (v * 100.0).round() as i64),
        length_s: seconds(&text(b"tlen")),
        bitrate: leading_f64(&text(b"tbit")).map_or(0, |v| v.round() as u32),
        sample_rate: leading_f64(&text(b"tsmp")).map_or(0, |v| {
            (if v < 1000.0 { v * 1000.0 } else { v }).round() as u32
        }),
        date_added: added
            .and_then(|s| chrono::DateTime::from_timestamp(i64::try_from(s).ok()?, 0))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        ..Track::default()
    };
    Some((path, t))
}

/// Every track record of the file as `(normalised path, track)`, in file order.
pub fn read(buf: &[u8]) -> Result<Vec<(String, Track)>, String> {
    Ok(tlv::parse(buf)?
        .iter()
        .filter(|r| &r.tag == b"otrk")
        .filter_map(|r| read_track(&tlv::parse(r.data).ok()?))
        .collect())
}

/// The text fields (tag → value) of the track record at `path`.
pub fn fields(buf: &[u8], path: &str) -> Result<Option<HashMap<String, String>>, String> {
    for r in tlv::parse(buf)?.iter().filter(|r| &r.tag == b"otrk") {
        if path_of(r.data).as_deref() == Some(path) {
            return Ok(Some(
                tlv::parse(r.data)?
                    .iter()
                    .filter(|f| matches!(f.tag[0], b't' | b'p'))
                    .map(|f| (String::from_utf8_lossy(&f.tag).into_owned(), f.text()))
                    .collect(),
            ));
        }
    }
    Ok(None)
}

/// `old` (the file's bytes, `None`: no library yet) with `new` (`(path, otrk record)`) merged in and the records of the paths in
/// `drop` removed. Returns the new file and how many of `new` replaced a record the file had.
pub fn merge(
    old: Option<&[u8]>,
    new: &[(String, Vec<u8>)],
    drop: &[String],
) -> Result<(Vec<u8>, usize), String> {
    let at: HashMap<&str, usize> = new
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.as_str(), i))
        .collect();
    let mut done = vec![false; new.len()];
    let (mut out, mut replaced, mut has_version) = (Vec::new(), 0, false);
    for r in tlv::parse(old.unwrap_or_default())? {
        if &r.tag == b"vrsn" {
            has_version = true;
        }
        let path = (&r.tag == b"otrk").then(|| path_of(r.data)).flatten();
        match path {
            Some(p) if drop.contains(&p) => {}
            Some(p) if at.contains_key(p.as_str()) => {
                let i = at[p.as_str()];
                if !std::mem::replace(&mut done[i], true) {
                    out.extend_from_slice(&new[i].1);
                    replaced += 1;
                }
            }
            _ => out.extend(rec(&r.tag, r.data)),
        }
    }
    if !has_version {
        out.splice(0..0, text(b"vrsn", VERSION));
    }
    for (i, (_, r)) in new.iter().enumerate() {
        if !done[i] {
            out.extend_from_slice(r);
        }
    }
    Ok((out, replaced))
}
