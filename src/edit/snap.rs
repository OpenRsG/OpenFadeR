//! The edited beat grid and snapping. The source's beat grid ([`Beat`], `bar_pos == 1` = downbeat) is carried
//! through the arrangement: a beat survives where its frame is still played, at its new position, and
//! silence has none. So after a cut the grid shown (and snapped to) is the grid the edited track plays.

use super::{Edl, Seg};
use crate::analysis::Beat;
use std::ops::Range;

/// A beat in arrangement frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub frame: usize,
    /// Downbeat (bar line).
    pub bar: bool,
}

/// What a position snaps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Off,
    Beat,
    Bar,
}

/// Source frame of a beat (rounded).
pub fn beat_frame(b: &Beat, rate: u32) -> usize {
    (b.time_ms as f64 * rate as f64 / 1000.0).round() as usize
}

/// The beats the arrangement plays, in order.
pub fn lines(edl: &Edl, beats: &[Beat], rate: u32) -> Vec<Line> {
    let frames: Vec<usize> = beats.iter().map(|b| beat_frame(b, rate)).collect();
    let mut out = Vec::new();
    let mut at = 0;
    for s in edl.segs() {
        if let Seg::Src { start, len } = *s {
            let i = frames.partition_point(|&f| f < start);
            for (b, &f) in beats[i..].iter().zip(&frames[i..]).take_while(|(_, f)| **f < start + len) {
                out.push(Line { frame: at + f - start, bar: b.bar_pos == 1 });
            }
        }
        at += s.len();
    }
    out
}

/// The line of `mode` nearest to `pos`; `pos` itself for `Off` or when the grid has none.
pub fn snap(lines: &[Line], mode: Snap, pos: usize) -> usize {
    let ok = |l: &&Line| mode == Snap::Beat || (mode == Snap::Bar && l.bar);
    if mode == Snap::Off {
        return pos;
    }
    let after = lines.iter().filter(ok).find(|l| l.frame >= pos).map(|l| l.frame);
    let before = lines.iter().rev().filter(ok).find(|l| l.frame <= pos).map(|l| l.frame);
    match (before, after) {
        (Some(b), Some(a)) => if pos - b <= a - pos { b } else { a },
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => pos,
    }
}

/// The selection between two raw positions (in either order): both ends snapped, at most `len`; a start at 0 and an end
/// at or past `len` stay on the track's ends (the first and last beat are rarely exactly there). Two ends that snap
/// to the same line select up to the next line of `mode` (or the end), so a drag always selects something.
pub fn range(lines: &[Line], mode: Snap, a: usize, b: usize, len: usize) -> Range<usize> {
    let (a, b) = (a.min(b), a.max(b));
    let s = if a == 0 { 0 } else { snap(lines, mode, a).min(len) };
    let mut e = if b >= len { len } else { snap(lines, mode, b) };
    if e <= s && mode != Snap::Off {
        e = lines.iter().find(|l| l.frame > s && (mode == Snap::Beat || l.bar)).map_or(len, |l| l.frame);
    }
    s..e.max(s)
}
