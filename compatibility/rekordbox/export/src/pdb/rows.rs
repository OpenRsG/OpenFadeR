//! Row encoders of `export.pdb` / `exportExt.pdb`. Layouts: pinned crate-digger `rekordbox_pdb.ksy` (spec) plus what a real
//! rekordbox 7 export contains (observed, see `.local/re/export/notes.md`). Every row is padded to a multiple of 4 bytes; the
//! `index_shift` of tracks/artists/albums (u2 at offset 2) is filled in by the page builder.

use super::{w16, w32};
use crate::column_label;
use crate::settings::browse::COLUMNS;

/// Longest string we write (characters); keeps a track row far below one page.
const MAX_CHARS: usize = 255;

/// `s` cut to the characters [`string`] writes (what a read-back of it gives).
pub fn clip(s: &str) -> &str {
    &s[..s.char_indices().nth(MAX_CHARS).map_or(s.len(), |(i, _)| i)]
}

/// DeviceSQL string: short ASCII (`(len+1)<<1|1`), long ASCII `0x40`, anything else UTF-16LE `0x90`.
pub fn string(out: &mut Vec<u8>, s: &str) {
    let s = clip(s);
    if s.is_ascii() && s.len() < 127 {
        out.push(((s.len() + 1) << 1 | 1) as u8);
        out.extend(s.as_bytes());
        return;
    }
    let (flag, body) = if s.is_ascii() {
        (0x40, s.as_bytes().to_vec())
    } else {
        (0x90, s.encode_utf16().flat_map(u16::to_le_bytes).collect())
    };
    out.push(flag);
    out.extend(((body.len() + 4) as u16).to_le_bytes());
    out.push(0);
    out.extend(body);
}

fn pad4(r: &mut Vec<u8>) {
    r.resize(r.len().next_multiple_of(4), 0);
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Everything a track row stores (ids reference the other tables; 0 = none).
pub struct TrackRow<'a> {
    pub id: u32,
    pub title: &'a str,
    pub artist_id: u32,
    pub album_id: u32,
    pub genre_id: u32,
    pub key_id: u32,
    pub artwork_id: u32,
    /// Colors table id 1..=8, 0 = none.
    pub color_id: u8,
    pub rating: u8,
    pub bpm_x100: u32,
    pub duration_s: u16,
    pub sample_rate: u32,
    pub bitrate: u32,
    pub file_size: u32,
    /// 1 mp3, 4 m4a, 5 flac, 0x0b wav, 0x0c aiff (observed: 1 and 4; rest rekordcrate).
    pub file_type: u16,
    pub comment: &'a str,
    pub date_added: &'a str,
    pub analyze_date: &'a str,
    pub analyze_path: &'a str,
    pub filename: &'a str,
    pub file_path: &'a str,
}

/// Track row: 0x5e fixed bytes, 21 u2 string offsets, the strings in index order (spec `track_row`).
pub fn track(t: &TrackRow) -> Vec<u8> {
    let mut r = vec![0u8; 0x5e + 42];
    w16(&mut r, 0, 0x24);
    w32(&mut r, 0x04, 0x000c_0700); // observed in every row
    w32(&mut r, 0x08, t.sample_rate);
    w32(&mut r, 0x10, t.file_size);
    w16(&mut r, 0x18, 0xf3e2); // observed constants (the spec's 19048/30967 differ)
    w16(&mut r, 0x1a, 0xb0b1);
    w32(&mut r, 0x1c, t.artwork_id);
    w32(&mut r, 0x20, t.key_id);
    w32(&mut r, 0x30, t.bitrate);
    w32(&mut r, 0x38, t.bpm_x100);
    w32(&mut r, 0x3c, t.genre_id);
    w32(&mut r, 0x40, t.album_id);
    w32(&mut r, 0x44, t.artist_id);
    w32(&mut r, 0x48, t.id);
    w16(&mut r, 0x52, 16); // sample depth
    w16(&mut r, 0x54, t.duration_s);
    w16(&mut r, 0x56, 41); // "always 41" (spec)
    r[0x58] = t.color_id;
    r[0x59] = t.rating.min(5);
    w16(&mut r, 0x5a, t.file_type);
    w16(&mut r, 0x5c, 3);
    // Index of each string: kuvo_public/autoload_hot_cues "ON" and "2"/"3" in #2/#3 are what rekordbox writes (observed).
    let strings: [&str; 21] = [
        "",
        "",
        "2",
        "3",
        "",
        "",
        "ON",
        "ON",
        "",
        "",
        t.date_added,
        "",
        "",
        "",
        t.analyze_path,
        t.analyze_date,
        t.comment,
        t.title,
        "",
        t.filename,
        t.file_path,
    ];
    for (i, s) in strings.iter().enumerate() {
        let at = r.len() as u16;
        w16(&mut r, 0x5e + 2 * i, at);
        string(&mut r, s);
    }
    pad4(&mut r);
    r
}

pub fn artist(id: u32, name: &str) -> Vec<u8> {
    let mut r = u32s(&[0x60, id]);
    r.extend([0x03, 0x0a]);
    string(&mut r, name);
    pad4(&mut r);
    r
}

pub fn album(id: u32, artist_id: u32, name: &str) -> Vec<u8> {
    let mut r = u32s(&[0x80, 0, artist_id, id, 0]);
    r.extend([0x03, 0x16]);
    string(&mut r, name);
    pad4(&mut r);
    r
}

/// Row of the tables that hold only a name: genre, label, artwork path and history playlist (spec `history_playlist_row`): `id u4, name`.
pub fn named(id: u32, name: &str) -> Vec<u8> {
    let mut r = u32s(&[id]);
    string(&mut r, name);
    pad4(&mut r);
    r
}

pub fn key(id: u32, name: &str) -> Vec<u8> {
    let mut r = u32s(&[id, id]);
    string(&mut r, name);
    pad4(&mut r);
    r
}

pub fn color(id: u8, name: &str) -> Vec<u8> {
    let mut r = vec![0, 0, 0, 0, id, id, 0, 0];
    string(&mut r, name);
    pad4(&mut r);
    r
}

pub fn playlist_node(
    parent: u32,
    sort_order: u32,
    id: u32,
    is_folder: bool,
    name: &str,
) -> Vec<u8> {
    let mut r = u32s(&[parent, 0, sort_order, id, u32::from(is_folder)]);
    string(&mut r, name);
    pad4(&mut r);
    r
}

pub fn playlist_entry(entry_index: u32, track_id: u32, playlist_id: u32) -> Vec<u8> {
    u32s(&[entry_index, track_id, playlist_id])
}

/// History entry row (spec `history_entry_row`): `track_id u4, playlist_id u4, entry_index u4`.
pub fn history_entry(entry_index: u32, track_id: u32, playlist_id: u32) -> Vec<u8> {
    u32s(&[track_id, playlist_id, entry_index])
}

/// The one row of table 19 (observed in every rekordbox export.pdb; the twin of OneLibrary's `property`): `0x0280 u2,
/// index_shift u2, track count u4, 0 u4`, the creation date `YYYY-MM-DD`, then the near offsets of the two strings after it:
/// the database version "1000" and "".
pub fn library_info(tracks: u32, date: &str) -> Vec<u8> {
    let mut r = u32s(&[0x0280, tracks, 0]);
    string(&mut r, date);
    let version = r.len() + 2;
    r.extend([version as u8, (version + 5) as u8]); // "1000" takes 5 bytes
    string(&mut r, "1000");
    string(&mut r, "");
    pad4(&mut r);
    r
}

pub fn columns() -> Vec<Vec<u8>> {
    COLUMNS
        .iter()
        .map(|&(id, field, label)| {
            let mut r = [id.to_le_bytes(), field.to_le_bytes()].concat();
            // Always the UTF-16 form: the marker characters are not ASCII.
            string(&mut r, &column_label(label));
            pad4(&mut r);
            r
        })
        .collect()
}

/// Rows of tables 17 and 18 (browse category / sort lists): four u2 per row, from `DeviceConfig::table17` / `table18`.
pub fn u16_rows(rows: &[[u16; 4]]) -> Vec<Vec<u8>> {
    rows.iter()
        .map(|r| r.iter().flat_map(|v| v.to_le_bytes()).collect())
        .collect()
}

/// `exportExt.pdb` tag row (spec `tag_row`; observed bytes): a category (`category == 0`) or a tag of `category`.
pub fn tag(category: u32, position: u32, id: u32, is_category: bool, name: &str) -> Vec<u8> {
    let mut r = vec![0u8; 0x1c];
    w16(&mut r, 0, 0x0680);
    w32(&mut r, 12, category);
    w32(&mut r, 16, position);
    w32(&mut r, 20, id);
    if is_category {
        r[27] = 1; // observed: 00 00 00 01
    }
    r.extend([0x03, 0x1f, 0]);
    string(&mut r, name);
    let empty_at = r.len() as u8;
    r[0x1e] = empty_at;
    string(&mut r, "");
    pad4(&mut r);
    r.resize(r.len() + 8, 0); // observed: every tag row ends with 8 zero bytes after the alignment
    r
}

/// `exportExt.pdb` tag_track row: track `track_id` carries tag `tag_id`.
pub fn tag_track(track_id: u32, tag_id: u32) -> Vec<u8> {
    u32s(&[0, track_id, tag_id, 3])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Vec<u8> {
        let mut o = Vec::new();
        string(&mut o, v);
        o
    }

    #[test]
    fn string_encodings() {
        assert_eq!(s(""), [0x03]);
        assert_eq!(s("abc"), [0x09, b'a', b'b', b'c']);
        let long = "x".repeat(200);
        assert_eq!(&s(&long)[..4], [0x40, 204, 0, 0]);
        assert_eq!(s("é"), [0x90, 6, 0, 0, 0xe9, 0]);
        assert_eq!(s(&"é".repeat(300)).len(), 4 + 2 * MAX_CHARS); // truncated
    }

    #[test]
    fn rows_are_aligned() {
        assert_eq!(artist(1, "Ab").len() % 4, 0);
        assert_eq!(color(1, "Pink").len(), 16); // observed row length
        assert_eq!(columns()[0].len(), 24); // observed
        assert_eq!(tag(0, 0, 1, true, "Genre").len(), 48); // observed
    }

    /// Byte for byte the row of a real rekordbox 7 export (24 tracks), index shift aside.
    #[test]
    fn library_info_is_the_observed_row() {
        let mut want = vec![0x80, 0x02, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0, 0x17];
        want.extend(b"2026-09-13");
        want.extend([0x19, 0x1e, 0x0b]);
        want.extend(b"1000");
        want.extend([0x03, 0]);
        assert_eq!(library_info(24, "2026-09-13"), want);
    }
}
