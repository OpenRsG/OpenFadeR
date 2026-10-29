//! One track as an NML `ENTRY`: location, tag info, tempo, key and the CUE_V2 markers.
//!
//! Evidence (no official schema exists; all of it is the format as Traktor writes and other tools read it):
//! * `LOCATION`: `DIR` is the folder path with every separator written `/:` (also a leading and a trailing one), `VOLUME`
//!   the drive letter on Windows (`G:`) and the volume name elsewhere; the playlist key of a track is `VOLUME + DIR + FILE`
//!   (Mixxx `src/library/traktor/traktorfeature.cpp`, `fromTraktorSeparators`; a real file: polamjag/traktor-nml
//!   `spec/traktor/traktor-nml-demo.nml`).
//! * `CUE_V2`: `TYPE` 0 cue, 1 fade-in, 2 fade-out, 3 load, 4 grid, 5 loop; `START` and `LEN` in milliseconds (float, `LEN`
//!   is the loop length, 0 otherwise); `REPEATS` -1; `HOTCUE` 0..=7 is hot cue A..H and -1 a cue that sits on no pad
//!   (hellricer.github.io "Decoding the TRAKTOR4 field", the same record in the MP3 tag; digital-dj-tools/dj-data-converter
//!   `src/converter/traktor/cue.clj`).
//! * Grid: a `TYPE` 4 marker at the first downbeat, `HOTCUE` -1, named "Beat Marker"; the tempo is the entry's `TEMPO`.
//!   Traktor itself names its own `AutoGrid` and keeps it on pad 1, which would collide with hot cue A. Lexicon writes
//!   "Beat Marker" / -1 markers into Traktor collections (discuss.lexicondj.com topic 2511). `LOCK="1"` on the entry is
//!   Traktor's grid lock, written so that Traktor does not re-grid the track on load (observed on Lexicon-written entries
//!   and in traktor-nml-utils `tests/fixtures/collection.nml`).
//! * `MUSICAL_KEY VALUE`: 0..=11 major C..B, 12..=23 minor C..B (Mixxx reads it with `keyFromNumericValue`; checked against
//!   the `KEY="10m"` / `VALUE="12"` pair of the traktor-nml-utils fixture and `12d` / 5 of the demo). `INFO KEY` is the
//!   Open Key text of the same key.
//! * `RANKING` 0..=255 = stars x 51 (Mixxx divides by 51). `FILESIZE` is in KiB, `BITRATE` in bit/s (fixture: 189720 and
//!   5040 for a 193 s MP3), dates are `Y/M/D` without padding.
//! * `INFO COLOR` 1..=7: red, orange, yellow, green, blue, purple, pink (Segolene-Albouy/Traktor-NML-to-Rekordbox-XML
//!   `consts.py`; inferred, Traktor documents only "one of six colors"). The aqua of the host has no Traktor colour and is
//!   written blue. Traktor has no cue colours, so none are written.
//! * No `AUDIO_ID` (an analysis fingerprint nobody else can compute): Traktor reads the tags and analyses the audio itself.

use chrono::{Datelike, NaiveDateTime, Timelike};
use openfader_plugin::library::{Analysis, Track};

use crate::xml::El;

/// Traktor colour by the host's track colour 1..=8 (pink, red, orange, yellow, green, aqua, blue, purple).
const COLORS: [u8; 8] = [7, 1, 2, 3, 4, 5, 5, 6];
/// Hot cue pads of a Traktor deck.
const PADS: u16 = 8;

/// `Contents/A/B/f.mp3` → (`/:Contents/:A/:B/:`, `f.mp3`).
pub fn dir_file(rel: &str) -> (String, String) {
    let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
    let mut d = String::from("/:");
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        d.push_str(part);
        d.push_str("/:");
    }
    (d, file.into())
}

/// `MUSICAL_KEY VALUE` of a Camelot ("8A"), Open Key ("10m", "3d": what Traktor's own `INFO KEY` and so a Traktor source's
/// `Track::key` hold) or note ("Am", "F#m", "Bb") key; `None` for anything else.
pub fn musical_key(s: &str) -> Option<u8> {
    let s = s.trim();
    for (suffix, minor) in [('m', true), ('d', false)] {
        if let Some(n) = s
            .strip_suffix(suffix)
            .and_then(|n| n.parse::<i32>().ok())
            .filter(|n| (1..=12).contains(n))
        {
            // Open Key 1d is C major and 1m A minor; every step is a fifth.
            let pc = ((7 * (n - 1)).rem_euclid(12) + if minor { 9 } else { 0 }) % 12;
            return Some(pc as u8 + if minor { 12 } else { 0 });
        }
    }
    for (suffix, minor) in [(['A', 'a'], true), (['B', 'b'], false)] {
        if let Some(n) = s
            .strip_suffix(suffix)
            .and_then(|n| n.parse::<i32>().ok())
            .filter(|n| (1..=12).contains(n))
        {
            // Camelot 8A is A minor and 8B C major; every step is a fifth (7 semitones).
            let pc = (if minor { 9 } else { 0 } + 7 * (n - 8)).rem_euclid(12);
            return Some(pc as u8 + if minor { 12 } else { 0 });
        }
    }
    let (note, minor) = s.strip_suffix('m').map_or((s, false), |n| (n, true));
    let mut chars = note.chars();
    let base = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let pc: i32 = match chars.as_str() {
        "" => base,
        "#" | "\u{266f}" => base + 1,
        "b" | "\u{266d}" => base - 1,
        _ => return None,
    };
    Some(pc.rem_euclid(12) as u8 + if minor { 12 } else { 0 })
}

/// Open Key text ("10m") of a `MUSICAL_KEY` value.
pub fn open_key(v: u8) -> String {
    let (minor, pc) = (v >= 12, i32::from(v % 12));
    // A minor is 1m and C major 1d; a minor key counts as its relative major, three semitones up.
    let major = if minor { (pc + 3) % 12 } else { pc };
    format!("{}{}", major * 7 % 12 + 1, if minor { 'm' } else { 'd' })
}

/// `2024-03-05` (or with `/`, a time after it) → `2024/3/5`.
fn ymd(s: &str) -> Option<String> {
    let mut p = s.trim().split(['-', '/', ' ', 'T']);
    let mut n = || p.next()?.parse::<u32>().ok();
    Some(format!("{}/{}/{}", n()?, n()?, n()?))
}

fn cue(name: &str, kind: u8, start: f64, len: f64, hotcue: i32) -> El {
    El::new("CUE_V2")
        .attr("NAME", name)
        .attr("DISPL_ORDER", 0)
        .attr("TYPE", kind)
        .attr("START", format!("{start:.6}"))
        .attr("LEN", format!("{len:.6}"))
        .attr("REPEATS", -1)
        .attr("HOTCUE", hotcue)
}

/// The grid marker, hot cues A..H (`HOTCUE` 0..=7), then memory cues and memory loops (`HOTCUE` -1); a loop is `TYPE` 5.
fn markers(a: &Analysis, warnings: &mut Vec<String>, title: &str) -> Vec<El> {
    let mut out = Vec::new();
    if let Some(b) = a.beats.iter().find(|b| b.bar_pos == 1).or(a.beats.first()) {
        out.push(cue("Beat Marker", 4, f64::from(b.time_ms), 0.0, -1));
    }
    let mut cues: Vec<_> = a.cues.iter().collect();
    cues.sort_by_key(|c| (c.hot_cue == 0, c.hot_cue, c.time_ms));
    let mut seen = [false; PADS as usize];
    for c in cues {
        let hot = match c.hot_cue {
            0 => -1,
            n if n <= PADS && !std::mem::replace(&mut seen[usize::from(n) - 1], true) => {
                i32::from(n) - 1
            }
            n => {
                warnings.push(format!(
                    "{title}: hot cue {n} left out (Traktor has pads 1 to {PADS}, one cue each)"
                ));
                continue;
            }
        };
        let name = c
            .comment
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("n.n.");
        let start = f64::from(c.time_ms);
        out.push(match c.loop_end_ms {
            Some(end) => cue(
                name,
                5,
                start,
                f64::from(end.saturating_sub(c.time_ms)),
                hot,
            ),
            None => cue(name, 0, start, 0.0, hot),
        });
    }
    out
}

/// The `ENTRY` of `t`, stored at `rel` (relative to the device root, `/`-separated) on the volume `vol`; `size` is the audio
/// file's byte count and `color` the host's track colour. Cues that cannot be written are named in `warnings`.
pub fn entry(
    vol: &str,
    rel: &str,
    size: u64,
    t: &Track,
    a: Option<&Analysis>,
    color: Option<u8>,
    now: NaiveDateTime,
    warnings: &mut Vec<String>,
) -> El {
    let (dir, file) = dir_file(rel);
    let title = if t.title.is_empty() {
        file.rsplit_once('.').map_or(file.as_str(), |s| s.0)
    } else {
        &t.title
    };
    let today = format!("{}/{}/{}", now.year(), now.month(), now.day());
    let markers = a.map(|a| markers(a, warnings, title)).unwrap_or_default();
    let grid = markers.iter().any(|m| m.get("TYPE") == Some("4"));
    let mut e = El::new("ENTRY")
        .attr("MODIFIED_DATE", &today)
        .attr("MODIFIED_TIME", now.num_seconds_from_midnight());
    if grid {
        e.set("LOCK", 1);
        e.set("LOCK_MODIFICATION_TIME", now.format("%Y-%m-%dT%H:%M:%S"));
    }
    e.set("TITLE", title);
    e.set("ARTIST", &t.artist);
    e.kids.push(
        El::new("LOCATION")
            .attr("DIR", dir)
            .attr("FILE", file)
            .attr("VOLUME", vol)
            .attr("VOLUMEID", vol),
    );
    if !t.album.is_empty() {
        e.kids.push(El::new("ALBUM").attr("TITLE", &t.album));
    }
    e.kids
        .push(El::new("MODIFICATION_INFO").attr("AUTHOR_TYPE", "user"));

    let key = a
        .and_then(|a| a.key.as_deref())
        .and_then(musical_key)
        .or_else(|| musical_key(&t.key));
    let mut info = El::new("INFO");
    for (k, v) in [
        ("GENRE", &t.genre),
        ("LABEL", &t.label),
        ("COMMENT", &t.comment),
    ] {
        if !v.is_empty() {
            info.set(k, v);
        }
    }
    if t.bitrate > 0 {
        info.set("BITRATE", u64::from(t.bitrate) * 1000);
    }
    if let Some(k) = key {
        info.set("KEY", open_key(k));
    }
    if t.length_s > 0 {
        info.set("PLAYTIME", t.length_s);
        info.set("PLAYTIME_FLOAT", format!("{:.6}", t.length_s as f64));
    }
    if t.rating > 0 {
        info.set("RANKING", u32::from(t.rating.min(5)) * 51);
    }
    info.set("IMPORT_DATE", ymd(&t.date_added).unwrap_or(today));
    info.set("FILESIZE", size.div_ceil(1024));
    if let Some(c) = color.and_then(|c| COLORS.get(usize::from(c).wrapping_sub(1))) {
        info.set("COLOR", c);
    }
    e.kids.push(info);

    let bpm = Some(t.bpm_x100)
        .filter(|b| *b > 0)
        .or_else(|| {
            a.and_then(|a| a.beats.first())
                .map(|b| i64::from(b.bpm_x100))
        })
        .filter(|b| *b > 0);
    if let Some(b) = bpm {
        e.kids.push(
            El::new("TEMPO")
                .attr("BPM", format!("{:.6}", b as f64 / 100.0))
                .attr("BPM_QUALITY", "100.000000"),
        );
    }
    if let Some(k) = key {
        e.kids.push(El::new("MUSICAL_KEY").attr("VALUE", k));
    }
    e.kids.extend(markers);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_the_real_pairs_of_traktor_files() {
        // Open Key 10m = C minor = 12, 12d = F major = 5 (fixtures), Camelot 8A = A minor = 21.
        assert_eq!(open_key(12), "10m");
        assert_eq!(open_key(5), "12d");
        assert_eq!(open_key(21), "1m");
        assert_eq!(musical_key("8A"), Some(21));
        assert_eq!(musical_key("5A"), Some(12));
        assert_eq!(musical_key("8B"), Some(0));
        assert_eq!(musical_key("7B"), Some(5));
        assert_eq!(musical_key("1A"), Some(20)); // G#m
        assert_eq!(musical_key("12B"), Some(4)); // E
        assert_eq!(musical_key("F#m"), Some(18));
        assert_eq!(musical_key("Bb"), Some(10));
        assert_eq!(musical_key("Abm"), Some(20));
        assert_eq!(musical_key("13A"), None);
        // Open Key (a Traktor source's own key text) names the same keys as `open_key` writes.
        assert_eq!(musical_key("10m"), Some(12));
        assert_eq!(musical_key("12d"), Some(5));
        assert!((0..24).all(|v| musical_key(&open_key(v)) == Some(v)));
        assert_eq!(musical_key(""), None);
        // Every Camelot key maps to its own value.
        let all: std::collections::BTreeSet<_> = (1..=12)
            .flat_map(|n| [format!("{n}A"), format!("{n}B")])
            .filter_map(|k| musical_key(&k))
            .collect();
        assert_eq!(all.len(), 24);
    }

    #[test]
    fn a_traktor_source_key_is_written_back_as_its_musical_key() {
        let t = Track {
            key: "10m".into(),
            ..Track::default()
        };
        let now = chrono::NaiveDate::from_ymd_opt(2024, 3, 5)
            .unwrap()
            .and_hms_opt(1, 2, 3)
            .unwrap();
        let e = entry("V", "Contents/a.wav", 1, &t, None, None, now, &mut Vec::new());
        assert_eq!(e.child("MUSICAL_KEY").unwrap().get("VALUE"), Some("12"));
        assert_eq!(e.child("INFO").unwrap().get("KEY"), Some("10m"));
    }

    #[test]
    fn locations_use_the_colon_separators() {
        assert_eq!(
            dir_file("Contents/A/B/f.mp3"),
            ("/:Contents/:A/:B/:".into(), "f.mp3".into())
        );
        assert_eq!(dir_file("f.mp3"), ("/:".into(), "f.mp3".into()));
    }

    #[test]
    fn dates_lose_their_padding() {
        assert_eq!(ymd("2024-03-05"), Some("2024/3/5".into()));
        assert_eq!(ymd("2024-03-05 10:11:12"), Some("2024/3/5".into()));
        assert_eq!(ymd(""), None);
    }
}
