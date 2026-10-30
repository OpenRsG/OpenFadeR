//! What Serato keeps in the audio file: `Serato Markers2` (track colour, hot cues, saved loops, beat grid lock) and `Serato BeatGrid`.
//! Formats: <https://github.com/Holzhaus/serato-tags/blob/main/docs/serato_markers2.md> and `serato_beatgrid.md`, colours from
//! `colors.md`, container layouts from `fileformats.md`; Mixxx's `src/track/serato/markers2.cpp` agrees on the entry layouts.
//!
//! `Serato Markers2` GEOB data = `01 01` + base64 (no padding, `\n` after every 72 characters) of `01 01` + entries + `00`; an
//! entry is `name\0`, a big-endian `u32` length, the data. Entries written: `COLOR` (`00` RGB), `CUE` (`00` index `u32` ms `00`
//! RGB `00 00` name), `LOOP` (`00` index start end `ff ff ff ff` `00`+RGB `00` locked name), `BPMLOCK`. Entries of other names
//! (`FLIP`, …) and the old `BPMLOCK` stay. `Serato BeatGrid` = `01 00`, a `u32` marker count, markers (`f32` seconds + `f32` BPM for the last,
//! `u32` beats to the next one for the others), one footer byte.
//!
//! Choices: hot cues 1..=8 become `CUE` entries; every loop (memory or hot cue) becomes a `LOOP`, numbered in time order (Serato
//! has 8 loop slots); memory cues have no Serato equivalent and are dropped (counted by [`Tags::new`]). A cue colour is
//! stored as is.

use openfader_plugin::library::{Analysis, Beat, Cue};

pub const MARKERS2: &str = "Serato Markers2";
pub const BEATGRID: &str = "Serato BeatGrid";
const MIME: &[u8] = b"application/octet-stream";
const NO_COLOR: [u8; 3] = [0xff; 3];
const LOOP_COLOR: [u8; 3] = [0x27, 0xaa, 0xe1];
/// Serato's own palette for hot cues 1 to 8 (colors.md, "Saved").
const CUE_COLORS: [[u8; 3]; 8] = [
    [0xcc, 0, 0],
    [0xcc, 0x88, 0],
    [0, 0, 0xcc],
    [0xcc, 0xcc, 0],
    [0, 0xcc, 0],
    [0xcc, 0, 0xcc],
    [0, 0xcc, 0xcc],
    [0x88, 0, 0xcc],
];
/// Serato's track colours for the host's 1..=8 (pink, red, orange, yellow, green, aqua, blue, purple).
const TRACK_COLORS: [[u8; 3]; 8] = [
    [0xff, 0x99, 0xff],
    [0xff, 0x99, 0x99],
    [0xff, 0xbb, 0x99],
    [0xff, 0xff, 0x99],
    [0x99, 0xff, 0x99],
    [0x99, 0xff, 0xff],
    [0x99, 0x99, 0xff],
    [0xbb, 0x99, 0xff],
];
/// Beats of tempo difference (in 1/100 BPM) that start a new beat grid marker.
// ponytail: one marker per tempo change of more than 0.1 BPM, a marker per beat if grids with drift ever need it.
const TEMPO_STEP: u16 = 10;

/// A cue (`end` 0) or a loop.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub index: u8,
    pub start: u32,
    pub end: u32,
    pub color: [u8; 3],
    pub name: String,
}

/// What to put into the tags of one track.
#[derive(Clone, Debug, Default)]
pub struct Tags {
    pub cues: Vec<Mark>,
    pub loops: Vec<Mark>,
    pub color: Option<[u8; 3]>,
    /// `Serato BeatGrid` data.
    pub grid: Option<Vec<u8>>,
    /// The track has an analysis or a colour: its tags are ours to refresh (an empty cue list clears the file's cues).
    pub touch: bool,
}

impl Tags {
    /// The tags for an analysis and a host track colour (1..=8); also how many memory cues Serato cannot hold.
    pub fn new(a: Option<&Analysis>, color: Option<u8>) -> (Tags, usize) {
        let mut t = Tags {
            color: color.and_then(|c| TRACK_COLORS.get(usize::from(c).wrapping_sub(1)).copied()),
            touch: a.is_some() || color.is_some(),
            ..Tags::default()
        };
        let mut dropped = 0;
        for c in a.map_or(&[][..], |a| &a.cues) {
            let name = c.comment.clone().unwrap_or_default();
            let slot = usize::from(c.hot_cue).wrapping_sub(1);
            if let Some(&def) = CUE_COLORS.get(slot) {
                t.cues.push(Mark {
                    index: slot as u8,
                    start: c.time_ms,
                    end: 0,
                    color: c.color.unwrap_or(def),
                    name: name.clone(),
                });
            }
            if let Some(end) = c.loop_end_ms {
                t.loops.push(Mark {
                    index: 0,
                    start: c.time_ms,
                    end,
                    color: c.color.unwrap_or(LOOP_COLOR),
                    name,
                });
            } else if slot >= CUE_COLORS.len() {
                dropped += 1;
            }
        }
        t.cues.sort_by_key(|m| m.index);
        t.loops.sort_by_key(|m| m.start);
        t.loops.truncate(8);
        for (i, l) in t.loops.iter_mut().enumerate() {
            l.index = i as u8;
        }
        t.grid = a.and_then(|a| beatgrid(&a.beats));
        (t, dropped)
    }

    /// There is nothing to write into a file that has no tags yet.
    pub fn blank(&self) -> bool {
        self.markers2(None).is_none() && self.grid.is_none()
    }

    /// The `Serato Markers2` GEOB data after applying these tags to `old` (the file's current data); `None`: leave it as it is.
    pub fn markers2(&self, old: Option<&[u8]>) -> Option<Vec<u8>> {
        let had = old.map(entries).unwrap_or_default();
        let kept = |name: &str| had.iter().find(|(n, _)| n == name).map(|(_, d)| d.clone());
        if !self.touch
            || (self.cues.is_empty()
                && self.loops.is_empty()
                && self.color.is_none()
                && old.is_none())
        {
            return None;
        }
        let rgb = self
            .color
            .or_else(|| kept("COLOR")?.get(1..4)?.try_into().ok());
        let mut out = vec![(
            "COLOR".to_string(),
            [&[0][..], &rgb.unwrap_or(NO_COLOR)].concat(),
        )];
        for m in &self.cues {
            let mut d = vec![0, m.index];
            d.extend(m.start.to_be_bytes());
            d.push(0);
            d.extend(m.color);
            d.extend([0, 0]);
            d.extend(m.name.as_bytes());
            d.push(0);
            out.push(("CUE".into(), d));
        }
        for m in &self.loops {
            let mut d = vec![0, m.index];
            d.extend(m.start.to_be_bytes());
            d.extend(m.end.to_be_bytes());
            d.extend([0xff; 4]);
            d.extend([0, m.color[0], m.color[1], m.color[2], 0, 0]);
            d.extend(m.name.as_bytes());
            d.push(0);
            out.push(("LOOP".into(), d));
        }
        out.push(("BPMLOCK".into(), kept("BPMLOCK").unwrap_or(vec![0])));
        out.extend(
            had.into_iter()
                .filter(|(n, _)| !["COLOR", "CUE", "LOOP", "BPMLOCK"].contains(&n.as_str())),
        );
        let mut inner = vec![1, 1];
        for (n, d) in out {
            inner.extend(n.as_bytes());
            inner.push(0);
            inner.extend((d.len() as u32).to_be_bytes());
            inner.extend(d);
        }
        inner.push(0);
        Some([&[1, 1][..], wrap(&b64(&inner), 72).as_bytes()].concat())
    }
}

/// The entries `(name, data)` of `Serato Markers2` GEOB data.
pub fn entries(geob: &[u8]) -> Vec<(String, Vec<u8>)> {
    let inner = unb64(geob.get(2..).unwrap_or_default());
    let mut pos = 2.min(inner.len());
    let mut out = Vec::new();
    while let Some(n) = inner[pos..].iter().position(|&c| c == 0) {
        if n == 0 {
            break;
        }
        let name = String::from_utf8_lossy(&inner[pos..pos + n]).into_owned();
        pos += n + 1;
        let Some(len) = inner
            .get(pos..pos + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        else {
            break;
        };
        let Some(d) = inner.get(pos + 4..pos + 4 + len) else {
            break;
        };
        out.push((name, d.to_vec()));
        pos += 4 + len;
    }
    out
}

/// The cues and loops of `Serato Markers2` GEOB data, in file order: `CUE` entries become hot cues 1..=8 (index + 1), `LOOP`
/// entries become memory loops (`hot_cue` 0 with a loop end; Serato's loop slots are not hot cue slots). A colour is kept as
/// stored, an empty name is no comment. Layouts as written by [`Tags::markers2`].
pub fn cues(markers2: &[u8]) -> Vec<Cue> {
    let u32_at = |d: &[u8], i: usize| {
        d.get(i..i + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let name = |d: &[u8], from: usize| {
        let n = d.get(from..).unwrap_or_default();
        let n = &n[..n.iter().position(|&c| c == 0).unwrap_or(n.len())];
        Some(String::from_utf8_lossy(n).into_owned()).filter(|s| !s.is_empty())
    };
    let mut out = Vec::new();
    for (n, d) in entries(markers2) {
        match n.as_str() {
            "CUE" if d.len() >= 12 => out.push(Cue {
                hot_cue: u16::from(d[1]) + 1,
                time_ms: u32_at(&d, 2).unwrap_or(0),
                loop_end_ms: None,
                color: Some([d[7], d[8], d[9]]),
                comment: name(&d, 12),
            }),
            "LOOP" if d.len() >= 20 => out.push(Cue {
                hot_cue: 0,
                time_ms: u32_at(&d, 2).unwrap_or(0),
                loop_end_ms: u32_at(&d, 6),
                color: Some([d[15], d[16], d[17]]),
                comment: name(&d, 20),
            }),
            _ => {}
        }
    }
    out
}

/// Most beats a grid is expanded to (about 10 hours at 170 BPM): a damaged marker must not fill the memory.
const MAX_BEATS: usize = 100_000;

/// The beats of `Serato BeatGrid` data up to `end_ms` (the track length; 0: only the last marker's own beat). Each marker
/// starts a run of beats: for all but the last the run is the marker's beat count up to the next marker (tempo from the two
/// times), for the last it runs at the stored BPM. The first marker is beat 1 of its bar and nothing is extrapolated before
/// it (inferred: Serato's first marker is the grid's first downbeat).
pub fn grid_beats(grid: &[u8], end_ms: u32) -> Vec<Beat> {
    let word = |i: usize| grid.get(i..i + 4).map(|b| [b[0], b[1], b[2], b[3]]);
    let Some(count) = word(2).map(u32::from_be_bytes) else {
        return vec![];
    };
    let marks: Vec<(f64, [u8; 4])> = (0..count as usize)
        .map_while(|k| {
            Some((
                f64::from(f32::from_be_bytes(word(6 + 8 * k)?)),
                word(10 + 8 * k)?,
            ))
        })
        .collect();
    let mut out: Vec<Beat> = Vec::new();
    for (k, &(t, tail)) in marks.iter().enumerate() {
        let (n, bpm) = match marks.get(k + 1) {
            Some(&(next, _)) => {
                let n = u32::from_be_bytes(tail);
                if n == 0 || next <= t {
                    continue;
                }
                (n as usize, 60.0 * f64::from(n) / (next - t))
            }
            None => {
                let bpm = f64::from(f32::from_be_bytes(tail));
                if !(bpm > 0.0 && bpm.is_finite()) {
                    break;
                }
                let end = if end_ms > 0 {
                    f64::from(end_ms)
                } else {
                    t * 1000.0
                };
                // A beat counts when it starts at most half a millisecond after `end` (the times are rounded to ms).
                (
                    ((end - t * 1000.0 + 0.5) * bpm / 60_000.0).floor().max(0.0) as usize + 1,
                    bpm,
                )
            }
        };
        let x100 = (bpm * 100.0).round().clamp(0.0, 65535.0) as u16;
        for i in 0..n.min(MAX_BEATS - out.len().min(MAX_BEATS)) {
            out.push(Beat {
                bar_pos: (out.len() % 4) as u16 + 1,
                bpm_x100: x100,
                time_ms: ((t + i as f64 * 60.0 / bpm) * 1000.0).round().max(0.0) as u32,
            });
        }
    }
    out
}

/// `Serato BeatGrid` data for a beat list: a marker at the first downbeat and at every tempo change.
pub fn beatgrid(beats: &[Beat]) -> Option<Vec<u8>> {
    let b = &beats[beats.iter().position(|b| b.bar_pos == 1).unwrap_or(0)..];
    b.first()?;
    let mut starts = vec![0];
    for (i, x) in b.iter().enumerate() {
        if x.bpm_x100.abs_diff(b[*starts.last()?].bpm_x100) > TEMPO_STEP {
            starts.push(i);
        }
    }
    let mut d = vec![1, 0];
    d.extend((starts.len() as u32).to_be_bytes());
    for (k, &s) in starts.iter().enumerate() {
        d.extend((b[s].time_ms as f32 / 1000.0).to_be_bytes());
        if let Some(&next) = starts.get(k + 1) {
            d.extend(((next - s) as u32).to_be_bytes());
        } else {
            let (last, n) = (b.last()?, b.len() - 1 - s);
            let span = last.time_ms.saturating_sub(b[s].time_ms);
            let bpm = if n > 0 && span > 0 {
                60_000.0 * n as f64 / f64::from(span)
            } else {
                f64::from(b[s].bpm_x100) / 100.0
            };
            d.extend((bpm as f32).to_be_bytes());
        }
    }
    d.push(0);
    Some(d)
}

/// A tag field of a container without GEOB frames (FLAC, MP4): base64 of `mime\0` `\0` `name\0` data, `\n` after every 72
/// characters when `lines`.
pub fn field(name: &str, data: &[u8], lines: bool) -> String {
    let raw = [MIME, b"\0\0", name.as_bytes(), b"\0", data].concat();
    let s = b64(&raw);
    if lines {
        wrap(&s, 72)
    } else {
        s
    }
}

/// The GEOB data inside a [`field`].
pub fn field_data(s: &str) -> Option<Vec<u8>> {
    let raw = unb64(s.as_bytes());
    let mut pos = 0;
    for _ in 0..3 {
        pos += raw.get(pos..)?.iter().position(|&c| c == 0)? + 1;
    }
    Some(raw[pos..].to_vec())
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 without padding.
fn b64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = c.iter().fold(0u32, |a, &b| a << 8 | u32::from(b)) << (8 * (3 - c.len()));
        for i in 0..=c.len() {
            s.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

/// Base64 decoder that skips line feeds and `=`, stops at the first NUL (Serato pads with NULs) and drops a dangling sixth
/// of a byte (Serato sometimes writes one character too many).
fn unb64(s: &[u8]) -> Vec<u8> {
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for &c in s.iter().take_while(|&&c| c != 0) {
        let Some(v) = ALPHABET.iter().position(|&a| a == c) else {
            continue;
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

fn wrap(s: &str, n: usize) -> String {
    s.as_bytes()
        .chunks(n)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}
