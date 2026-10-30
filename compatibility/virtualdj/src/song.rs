//! One `<Song>` of `database.xml`: built from a track and its analysis, or refreshed in place. Elements and attributes of an
//! existing song that this plugin does not write (play counts, colours, automix points, remix points, a newer VirtualDJ's
//! additions) are kept. A hot cue or memory cue `<Poi>` has no `Type` (the default of VirtualDJ's POI editor).
//!
//! Sources: the element and attribute names are VDJPedia's "VDJ_database" (https://virtualdj.com/wiki/VDJ_database.html);
//! `Bpm` is the time between two beats in seconds (same page); a hot cue is `<Poi Name Pos Num>` with `Pos` in seconds and
//! `Num` from 1, the beat grid anchor `<Poi Pos Type="beatgrid"/>`, a saved loop `<Poi Type="loop" Size Slot/>` (forum
//! threads 207692 and 214082 on virtualdj.com); `Color` is ARGB as a decimal number (observed: `Color="4294901760"` is
//! opaque red in forum thread 228599; the same in github.com/epistemex/vdj-data `vdj.color.js`); `Scan Phase` is the grid
//! anchor of the newest VirtualDJ builds (github.com/DeedjayNo1/beatgrid_fix). Inferred: `Size` of a loop counts beats (the
//! stored values of old databases are round numbers such as `4.000000` whatever the tempo), a memory cue is a `Num="-1"`
//! cue, `Scan Flag="32768"` and `Tags Flag="1"` as a scanned song carries them (observed in every published example).

use crate::xml::{El, Node};
use openfader_plugin::library::{Analysis, Cue, Track};
use std::path::Path;

/// How the drive is named inside `FilePath`: `E:` and `\`, or `/Volumes/NAME` and `/`. VirtualDJ ignores the drive letter
/// (Windows) and the volume name (macOS) of a database that lies on the drive itself and puts the real one in
/// (forum threads 91468, 193485 and 242242 on virtualdj.com), so any name works; the style of the database found on the stick
/// is kept.
pub struct Style {
    prefix: String,
    sep: char,
}

impl Style {
    /// The style of an existing `FilePath`: `E:\x` or `/Volumes/NAME/x`.
    pub fn of(path: &str) -> Option<Self> {
        if let Some(rest) = path.strip_prefix("/Volumes/") {
            let name = rest.split('/').next()?;
            return Some(Self {
                prefix: format!("/Volumes/{name}"),
                sep: '/',
            });
        }
        let b = path.as_bytes();
        (b.len() > 2 && b[1] == b':' && b[0].is_ascii_alphabetic()).then(|| Self {
            prefix: path[..2].to_owned(),
            sep: if path.contains('\\') { '\\' } else { '/' },
        })
    }

    /// The style of the device mounted at `root` (a drive letter or a macOS volume), else `E:\`.
    pub fn of_root(root: &Path) -> Self {
        Self::of(&format!("{}/x", root.to_string_lossy())).unwrap_or(Self {
            prefix: "E:".into(),
            sep: '\\',
        })
    }

    /// `FilePath` of the file `rel` (relative to the root, `/` separated).
    pub fn path(&self, rel: &str) -> String {
        let sep = self.sep;
        let rel: String = rel
            .chars()
            .map(|c| if c == '/' { sep } else { c })
            .collect();
        format!("{}{sep}{rel}", self.prefix)
    }
}

/// `file_path` without its drive (`E:` or `/Volumes/NAME`), `/` separated and with the leading `/`; `None` when it has none.
pub fn drive_rest(file_path: &str) -> Option<String> {
    let p = file_path.replace('\\', "/");
    let s = Style::of(&p)?;
    Some(p[s.prefix.len()..].to_owned())
}

/// Identity of a file on the drive: the path without the drive name, `/` separated, lower case (FAT is case insensitive).
pub fn identity(file_path: &str) -> String {
    drive_rest(file_path)
        .unwrap_or_else(|| file_path.replace('\\', "/"))
        .to_lowercase()
}

/// Keys by Camelot number 1..=12: `MINOR[n - 1]` is `nA`, `MAJOR[n - 1]` is `nB`.
const MINOR: [&str; 12] = [
    "G#m", "D#m", "A#m", "Fm", "Cm", "Gm", "Dm", "Am", "Em", "Bm", "F#m", "C#m",
];
const MAJOR: [&str; 12] = [
    "B", "F#", "C#", "G#", "D#", "A#", "F", "C", "G", "D", "A", "E",
];

/// VirtualDJ's key notation (`Am`, `C#`, sharps only) of a Camelot key (`8A`); any other text is returned as it is.
pub fn key_text(key: &str) -> String {
    let k = key.trim();
    let Some((n, letter)) = k.split_at_checked(k.len().saturating_sub(1)) else {
        return k.to_owned();
    };
    match (n.parse::<usize>(), letter) {
        (Ok(n @ 1..=12), "A" | "a") => MINOR[n - 1].to_owned(),
        (Ok(n @ 1..=12), "B" | "b") => MAJOR[n - 1].to_owned(),
        _ => k.to_owned(),
    }
}

/// Camelot key (`8A`) of a VirtualDJ key (`Am`, `C#`, `Bbm`; a flat is read as its sharp); an existing Camelot key is kept
/// in upper case; `None` for any other text.
pub fn camelot(key: &str) -> Option<String> {
    const SHARP: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let k = key.trim();
    if let Some((n, l)) = k.split_at_checked(k.len().saturating_sub(1))
        && matches!(n.parse::<u8>(), Ok(1..=12))
        && matches!(l, "A" | "a" | "B" | "b")
    {
        return Some(k.to_uppercase());
    }
    let mut it = k.chars();
    let base = "C D EF G A B".find(it.next()?.to_ascii_uppercase())? as i32;
    let rest = it.as_str();
    let (shift, rest) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let minor = match rest {
        "" => false,
        "m" | "min" => true,
        _ => return None,
    };
    let name = format!(
        "{}{}",
        SHARP[(base + shift).rem_euclid(12) as usize],
        if minor { "m" } else { "" }
    );
    let table = if minor { &MINOR } else { &MAJOR };
    let n = table.iter().position(|k| *k == name)? + 1;
    Some(format!("{n}{}", if minor { 'A' } else { 'B' }))
}

/// Seconds between two beats at `bpm_x100`.
fn period(bpm_x100: i64) -> Option<f64> {
    (bpm_x100 > 0).then(|| 6000.0 / bpm_x100 as f64)
}

/// `FirstSeen` (Unix seconds) from a `YYYY-MM-DD…` date.
fn first_seen(date: &str) -> Option<i64> {
    let d = chrono::NaiveDate::parse_from_str(date.get(..10)?, "%Y-%m-%d").ok()?;
    Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
}

fn color(rgb: [u8; 3]) -> String {
    (0xFF00_0000u32 | u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2]))
        .to_string()
}

fn secs(ms: u32) -> String {
    format!("{:.6}", f64::from(ms) / 1000.0)
}

/// The tempo of the grid (`bpm_x100`) and the anchor (first downbeat, seconds) of an analysis.
fn grid(a: &Analysis, t: &Track) -> (i64, Option<u32>) {
    let anchor = a.beats.iter().find(|b| b.bar_pos == 1).or(a.beats.first());
    let bpm = anchor.map_or(0, |b| i64::from(b.bpm_x100));
    (
        if bpm > 0 { bpm } else { t.bpm_x100 },
        anchor.map(|b| b.time_ms),
    )
}

/// The `<Poi>` elements of an analysis: the grid anchor, the hot cues, the memory cues and loops (warnings for loops that
/// cannot be written).
fn pois(
    a: &Analysis,
    bpm_x100: i64,
    anchor: Option<u32>,
    title: &str,
    warnings: &mut Vec<String>,
) -> Vec<El> {
    let mut out = Vec::new();
    if let Some(ms) = anchor {
        let mut p = El::new("Poi");
        p.set("Pos", secs(ms));
        p.set("Type", "beatgrid");
        out.push(p);
    }
    let mut cues: Vec<&Cue> = a.cues.iter().filter(|c| c.hot_cue <= 8).collect();
    cues.sort_by_key(|c| (c.hot_cue == 0, c.hot_cue, c.time_ms));
    let mut slots: Vec<u16> = cues
        .iter()
        .filter(|c| c.loop_end_ms.is_some())
        .map(|c| c.hot_cue)
        .collect();
    for c in cues {
        let mut p = El::new("Poi");
        let num = if c.hot_cue == 0 {
            -1
        } else {
            i32::from(c.hot_cue)
        };
        let name = match (&c.comment, c.hot_cue) {
            (Some(s), _) if !s.is_empty() => s.clone(),
            (_, 0) => "Memory".into(),
            (_, n) => format!("Cue {n}"),
        };
        p.set("Name", name);
        p.set("Pos", secs(c.time_ms));
        p.set("Num", num.to_string());
        if let Some(rgb) = c.color {
            p.set("Color", color(rgb));
        }
        // No `Type`: VirtualDJ's POI editor makes a hot cue the default type and its own databases write none (VDJPedia, forum
        // thread 228599); `cue` is not a type it names.
        if let Some(end) = c.loop_end_ms.filter(|e| *e > c.time_ms) {
            match period(bpm_x100) {
                Some(period) => {
                    p.set("Type", "loop");
                    p.set(
                        "Size",
                        format!("{:.6}", f64::from(end - c.time_ms) / 1000.0 / period),
                    );
                    // A hot cue loop sits in the slot of its pad; a memory loop takes the first free slot of the eight.
                    let slot = if num > 0 {
                        Some(num as u16)
                    } else {
                        (1..=8).find(|s| !slots.contains(s))
                    };
                    if let Some(s) = slot {
                        slots.push(s);
                        p.set("Slot", s.to_string());
                    }
                }
                None => warnings.push(format!(
                    "{title}: the loop at {} s is written as a cue (no tempo to size it)",
                    secs(c.time_ms)
                )),
            }
        }
        out.push(p);
    }
    out
}

/// Types of `<Poi>` that an analysis replaces; others (automix, remix, load, action) stay.
fn ours(p: &El) -> bool {
    matches!(p.attr("Type"), None | Some("cue" | "loop" | "beatgrid"))
}

/// What a track written to the drive needs.
pub struct Entry<'a> {
    pub track: &'a Track,
    pub analysis: Option<&'a Analysis>,
    pub size: u64,
    /// Unix seconds, `FirstSeen` of a song that has none.
    pub now: i64,
}

/// Writes `e` into `song` (a new empty element, or the one the database holds): tags, infos, tempo and key, and with an
/// analysis its grid, cues and loops instead of the old ones. `path` is the `FilePath` of a new song.
pub fn fill(song: &mut El, path: &str, e: &Entry, warnings: &mut Vec<String>) {
    let t = e.track;
    song.set_new("FilePath", path);
    song.set("FileSize", e.size.to_string());
    let (bpm, anchor) = e.analysis.map_or((t.bpm_x100, None), |a| grid(a, t));
    let period = period(bpm).map(|p| format!("{p:.6}"));
    let key = e
        .analysis
        .and_then(|a| a.key.as_deref())
        .filter(|k| !k.is_empty())
        .or(Some(t.key.as_str()).filter(|k| !k.is_empty()))
        .map(key_text);

    let tags = song.child_mut("Tags");
    for (k, v) in [
        ("Author", &t.artist),
        ("Title", &t.title),
        ("Genre", &t.genre),
        ("Album", &t.album),
        ("Label", &t.label),
    ] {
        if !v.is_empty() {
            tags.set(k, v.as_str());
        }
    }
    if let Some(p) = &period {
        tags.set("Bpm", p.as_str());
    }
    if let Some(k) = &key {
        tags.set("Key", k.as_str());
    }
    if t.rating > 0 {
        tags.set("Stars", t.rating.min(5).to_string());
    }
    tags.set_new("Flag", "1");

    let infos = song.child_mut("Infos");
    if t.length_s > 0 {
        infos.set_new("SongLength", format!("{:.6}", t.length_s as f64));
    }
    infos.set_new(
        "FirstSeen",
        first_seen(&t.date_added).unwrap_or(e.now).to_string(),
    );
    if t.bitrate > 0 {
        infos.set_new("Bitrate", t.bitrate.to_string());
    }

    if period.is_some() || key.is_some() {
        let scan = song.child_mut("Scan");
        scan.set_new("Version", "801");
        if let Some(p) = &period {
            scan.set("Bpm", p.as_str());
        }
        if let Some(k) = &key {
            scan.set("Key", k.as_str());
        }
        if let Some(ms) = anchor {
            scan.set("Phase", secs(ms));
        }
        scan.set_new("Flag", "32768");
    }

    if let Some(a) = e.analysis {
        song.kids
            .retain(|n| !matches!(n, Node::El(p) if p.name == "Poi" && ours(p)));
        song.kids.extend(
            pois(a, bpm, anchor, &t.title, warnings)
                .into_iter()
                .map(Node::El),
        );
    }

    if !t.comment.is_empty() {
        song.child_mut("Comment").set_text(&t.comment);
    }
}
