//! The five `PerformanceData` blobs, from plain values to bytes and back. Layouts: libdjinterop `src/djinterop/engine/v2/*_blob.cpp`
//! (<https://github.com/xsco/libdjinterop>), which `PerformanceData` of schema 3.0.0 shares with the older `Track` columns.
//!
//! `trackData`, `overviewWaveFormData`, `beatData` and `quickCues` are a zlib stream behind a 4-byte big-endian length of the
//! uncompressed bytes; `loops` is stored raw. Numbers are big-endian except in `loops` and in the beat markers (little-endian).
//! The `parse_*` functions read what the encoders write and answer `None` for a blob that is cut short or not a zlib stream.

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use std::io::{Read, Write};

/// Hot cue and loop slots of a track.
pub const SLOTS: usize = 8;

/// Pad colour as `[a, r, g, b]`, and `[0; 4]` for an empty slot.
pub type Argb = [u8; 4];

/// One beat marker: the beats between two markers are evenly spaced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    pub offset: f64,
    pub number: i64,
}

pub struct QuickCue {
    pub label: String,
    /// Sample offset, -1 when the slot is empty.
    pub offset: f64,
    pub color: Argb,
}

pub struct Loop {
    pub label: String,
    pub start: f64,
    pub end: f64,
    pub color: Argb,
}

/// The overview waveform: points as `[low, mid, high]`, the samples each stands for, and the per-band maximum.
pub struct Overview {
    pub points: Vec<[u8; 3]>,
    pub samples_per_point: f64,
}

fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut z = ZlibEncoder::new(
        (raw.len() as u32).to_be_bytes().to_vec(),
        Compression::default(),
    );
    // Writing to a Vec cannot fail.
    z.write_all(raw)
        .and_then(|()| z.finish())
        .unwrap_or_default()
}

/// `trackData`: sample rate, sample count, key id and the three average loudnesses (0: the player does not rescale).
pub fn track_data(rate: f64, samples: i64, key: i32) -> Vec<u8> {
    let mut b = rate.to_be_bytes().to_vec();
    b.extend(samples.to_be_bytes());
    b.extend(key.to_be_bytes());
    b.extend([0u8; 24]);
    zlib(&b)
}

/// `overviewWaveFormData`.
pub fn overview(o: &Overview) -> Vec<u8> {
    let n = o.points.len() as i64;
    let mut b = [
        n.to_be_bytes(),
        n.to_be_bytes(),
        o.samples_per_point.to_be_bytes(),
    ]
    .concat();
    b.extend(o.points.iter().flatten());
    b.extend((0..3).map(|c| o.points.iter().map(|p| p[c]).max().unwrap_or(0)));
    zlib(&b)
}

/// `beatData`: the default and the adjusted grid are the same list. libdjinterop notes 9 zero bytes that real data ends with.
pub fn beat_data(rate: f64, samples: f64, grid: &[Marker]) -> Vec<u8> {
    let mut b = [rate.to_be_bytes(), samples.to_be_bytes()].concat();
    b.push(u8::from(!grid.is_empty()));
    for _ in 0..2 {
        b.extend((grid.len() as i64).to_be_bytes());
        for (i, m) in grid.iter().enumerate() {
            let beats = grid
                .get(i + 1)
                .map_or(0, |next| (next.number - m.number) as i32);
            b.extend(m.offset.to_le_bytes());
            b.extend(m.number.to_le_bytes());
            b.extend(beats.to_le_bytes());
            b.extend(0i32.to_le_bytes());
        }
    }
    b.extend([0u8; 9]);
    zlib(&b)
}

/// A label as the blobs store it: a length byte and at most 255 bytes of UTF-8, cut at a character boundary.
fn label(b: &mut Vec<u8>, s: &str) {
    let mut n = s.len().min(255);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    b.push(n as u8);
    b.extend(&s.as_bytes()[..n]);
}

/// `quickCues`: exactly [`SLOTS`] cues, then the main cue (adjusted, flag, default; 0 when there is none).
pub fn quick_cues(cues: &[QuickCue], main: f64) -> Vec<u8> {
    let mut b = (cues.len() as i64).to_be_bytes().to_vec();
    for c in cues {
        label(&mut b, &c.label);
        b.extend(c.offset.to_be_bytes());
        b.extend(c.color);
    }
    b.extend(main.to_be_bytes());
    b.push(1);
    b.extend(main.to_be_bytes());
    zlib(&b)
}

/// `loops`, uncompressed: exactly [`SLOTS`] entries, `None` for an empty one.
pub fn loops(loops: &[Option<Loop>]) -> Vec<u8> {
    let mut b = (loops.len() as i64).to_le_bytes().to_vec();
    for l in loops {
        let (text, start, end, set, color) = match l {
            Some(l) => (l.label.as_str(), l.start, l.end, 1, l.color),
            None => ("", -1.0, -1.0, 0, [0; 4]),
        };
        label(&mut b, text);
        b.extend(start.to_le_bytes());
        b.extend(end.to_le_bytes());
        b.extend([set, set]);
        b.extend(color);
    }
    b
}

/// Largest uncompressed blob read: a real one is a few hundred KB, the overview waveform being the biggest.
const MAX_BLOB: usize = 64 << 20;

/// A cursor over blob bytes; every read answers `None` past the end.
struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|b| b[0])
    }
    fn be_f64(&mut self) -> Option<f64> {
        self.take().map(f64::from_be_bytes)
    }
    fn be_i64(&mut self) -> Option<i64> {
        self.take().map(i64::from_be_bytes)
    }
    fn le_f64(&mut self) -> Option<f64> {
        self.take().map(f64::from_le_bytes)
    }
    fn le_i64(&mut self) -> Option<i64> {
        self.take().map(i64::from_le_bytes)
    }
    fn le_i32(&mut self) -> Option<i32> {
        self.take().map(i32::from_le_bytes)
    }
    /// A [`label`]: a length byte and that many bytes of UTF-8 (invalid bytes become U+FFFD).
    fn label(&mut self) -> Option<String> {
        let n = usize::from(self.u8()?);
        let (s, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(String::from_utf8_lossy(s).into_owned())
    }
}

/// The bytes behind a [`zlib`] blob: the length prefix says how many, and no more than that are inflated.
fn inflate(b: &[u8]) -> Option<Vec<u8>> {
    let (len, z) = b.split_first_chunk::<4>()?;
    let len = u32::from_be_bytes(*len) as usize;
    if len > MAX_BLOB {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    ZlibDecoder::new(z)
        .take(len as u64)
        .read_to_end(&mut out)
        .ok()?;
    (out.len() == len).then_some(out)
}

/// `(sample rate, sample count)` of a `trackData` blob; the key and the loudnesses are not read (the key is `Track.key`).
pub fn parse_track_data(b: &[u8]) -> Option<(f64, i64)> {
    let raw = inflate(b)?;
    let mut r = Rd(&raw);
    Some((r.be_f64()?, r.be_i64()?))
}

/// `(sample rate, sample count, markers)` of a `beatData` blob. The markers are the adjusted grid (what the players use), the
/// default grid when the adjusted one is empty.
pub fn parse_beat_data(b: &[u8]) -> Option<(f64, f64, Vec<Marker>)> {
    let raw = inflate(b)?;
    let mut r = Rd(&raw);
    let (rate, samples, _is_set) = (r.be_f64()?, r.be_f64()?, r.u8()?);
    let mut grid = || {
        let n = r.be_i64()?;
        // A marker is 24 bytes: a count the rest of the blob cannot hold is garbage.
        if usize::try_from(n).ok()? > r.0.len() / 24 {
            return None;
        }
        (0..n)
            .map(|_| {
                let m = Marker {
                    offset: r.le_f64()?,
                    number: r.le_i64()?,
                };
                // Beats to the next marker, and the unused word.
                r.le_i32()?;
                r.le_i32()?;
                Some(m)
            })
            .collect::<Option<Vec<_>>>()
    };
    let default = grid()?;
    let adjusted = grid().unwrap_or_default();
    Some((
        rate,
        samples,
        if adjusted.is_empty() {
            default
        } else {
            adjusted
        },
    ))
}

/// `(slots, adjusted main cue)` of a `quickCues` blob; an empty slot has a negative offset.
pub fn parse_quick_cues(b: &[u8]) -> Option<(Vec<QuickCue>, f64)> {
    let raw = inflate(b)?;
    let mut r = Rd(&raw);
    let n = r.be_i64()?;
    // A slot is at least 13 bytes.
    if usize::try_from(n).ok()? > r.0.len() / 13 {
        return None;
    }
    let cues = (0..n)
        .map(|_| {
            Some(QuickCue {
                label: r.label()?,
                offset: r.be_f64()?,
                color: r.take()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((cues, r.be_f64()?))
}

/// The loops of a raw `loops` blob; a slot that is not set (or has no start and end) is `None`.
pub fn parse_loops(b: &[u8]) -> Option<Vec<Option<Loop>>> {
    let mut r = Rd(b);
    let n = r.le_i64()?;
    // A loop is at least 23 bytes.
    if usize::try_from(n).ok()? > r.0.len() / 23 {
        return None;
    }
    (0..n)
        .map(|_| {
            let (label, start, end) = (r.label()?, r.le_f64()?, r.le_f64()?);
            let (start_set, end_set) = (r.u8()?, r.u8()?);
            let color = r.take()?;
            Some(
                (start_set != 0 && end_set != 0 && start >= 0.0 && end > start).then_some(Loop {
                    label,
                    start,
                    end,
                    color,
                }),
            )
        })
        .collect()
}
