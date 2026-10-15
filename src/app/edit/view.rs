//! The timeline picture: ruler (time on top, bars and beats under it), the 3-band waveform of the arrangement,
//! the beat grid, the selection, and a lane of segments at the bottom (one block per run of the source,
//! silence in grey). Everything goes through one [`Axis`] (ms ↔ pixel column), like the deck waveforms; the
//! playhead is the only moving part and is drawn on a copy of the static [`base`].

use crate::app::render::{Axis, Canvas, Col, Layers, Rgb, Wave, playhead};
use openfader::edit::{Seg, snap::Line};
use std::ops::Range;

/// Everything the static layer shows.
pub struct Scene<'a> {
    pub axis: Axis,
    /// Physical size of the picture; `k` = physical pixels per logical pixel (text scale).
    pub w: usize,
    pub h: usize,
    pub k: usize,
    pub layers: &'a [Layers],
    /// Arrangement beats and segments, in frames at `rate`.
    pub lines: &'a [Line],
    pub segs: &'a [Seg],
    pub rate: u32,
    pub sel: Option<Range<usize>>,
    pub snap_on: bool,
}

/// Height of the ruler, and of the segment lane under the waveform.
fn band(k: usize) -> usize {
    14 * k + 8
}

/// Waveform entries per ms, as [`openfader::analysis::DETAIL_ENTRIES_PER_SEC`].
const PER_MS: f64 = openfader::analysis::DETAIL_ENTRIES_PER_SEC as f64 / 1000.0;

/// `layers` scaled to fill the room between the ruler and the lane (the wave is drawn centred on the picture).
fn wave(layers: &[Layers], h: usize, k: usize) -> Wave {
    let f = (h.saturating_sub(2 * band(k)) as f32 / h.max(1) as f32).max(0.0);
    let scaled = layers
        .iter()
        .map(|l| l.map(|c| Col { h: c.h * f, ..c }))
        .collect();
    Wave {
        layers: scaled,
        per_ms: PER_MS,
    }
}

/// "m:ss".
fn mmss(ms: f64) -> String {
    let ms = ms.max(0.0) as u64;
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

const BG: Rgb = [24, 24, 24];
const ORANGE: Rgb = [255, 153, 0];
/// Bar lines over the waveform (our choice of colour).
const BAR_LINE: Rgb = [235, 50, 50];
const BLOCKS: [Rgb; 2] = [[34, 64, 110], [30, 92, 92]];
const SILENCE: Rgb = [58, 58, 58];

fn ms(frame: usize, rate: u32) -> f64 {
    frame as f64 * 1000.0 / rate as f64
}

/// The picture without the playhead.
pub fn base(s: &Scene) -> Canvas {
    let (w, h, k, band) = (s.w, s.h, s.k, band(s.k));
    let mut c = Canvas::new(w, h);
    wave(s.layers, h, k).draw(&mut c, &s.axis, true);
    grid(&mut c, s, band);
    lane(&mut c, s, band);
    ruler(&mut c, s, band);
    if let Some(r) = &s.sel {
        let (a, b) = (
            s.axis.col(ms(r.start, s.rate)),
            s.axis.col(ms(r.end, s.rate)),
        );
        c.shade(a, b, 0..h, ORANGE, 0.25);
        for x in [a, b] {
            c.vline(x, 0..h, ORANGE);
        }
    }
    c
}

/// Pixels between two beats at the start of the grid (`f64::MAX` without one).
fn beat_px(s: &Scene) -> f64 {
    s.lines.windows(2).next().map_or(f64::MAX, |p| {
        ms(p[1].frame - p[0].frame, s.rate) * s.axis.px_per_ms
    })
}

/// The smallest power of two `n` with `n * px >= min`: show every n-th bar so what is shown stays `min` px apart.
fn every(px: f64, min: f64) -> usize {
    (0..16)
        .map(|p| 1usize << p)
        .find(|n| *n as f64 * px >= min)
        .unwrap_or(1 << 16)
}

/// Beat lines (dim) once they are `3k` px apart, bar lines (bright) every n-th bar so they stay `8k` px apart.
fn grid(c: &mut Canvas, s: &Scene, band: usize) {
    let (beat_px, k) = (beat_px(s), s.k as f64);
    let n = every(beat_px * 4.0, 8.0 * k);
    let rows = band..c.h - band;
    let mut bar = 0;
    for l in s.lines {
        bar += l.bar as usize;
        let x = s.axis.col(ms(l.frame, s.rate));
        if !(0..s.w as i64).contains(&x) {
            continue;
        }
        if l.bar && (bar - 1) % n == 0 {
            c.shade(x, x + 1, rows.clone(), BAR_LINE, 0.9);
        } else if !l.bar && beat_px >= 3.0 * k {
            c.shade(x, x + 1, rows.clone(), [255, 255, 255], 0.3);
        }
    }
}

/// Segment blocks: runs of the source alternate two colours (labelled with their source time), silence is grey.
fn lane(c: &mut Canvas, s: &Scene, band: usize) {
    let rows = c.h - band..c.h;
    let mut at = 0;
    for (i, seg) in s.segs.iter().enumerate() {
        let (a, b) = (
            s.axis.col(ms(at, s.rate)),
            s.axis.col(ms(at + seg.len(), s.rate)),
        );
        let (colour, label) = match *seg {
            Seg::Src { start, .. } => (BLOCKS[i % 2], mmss(ms(start, s.rate))),
            Seg::Silence(_) => (SILENCE, "SILENCE".into()),
        };
        c.shade(a, b, rows.clone(), colour, 1.0);
        c.vline(a, rows.clone(), [0, 0, 0]);
        c.text(
            a.max(0) + 3 * s.k as i64,
            rows.start + 4,
            &label,
            s.k,
            [230, 230, 230],
            b - 2,
        );
        at += seg.len();
    }
}

/// Time marks on top (every `step` s so they stay ≥ 60 logical px apart), bar numbers and beat ticks under them.
fn ruler(c: &mut Canvas, s: &Scene, band: usize) {
    let (k, ppm) = (s.k as i64, s.axis.px_per_ms);
    c.shade(0, s.w as i64, 0..band, BG, 1.0);
    let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0]
        .into_iter()
        .find(|st| st * 1000.0 * ppm >= 60.0 * k as f64)
        .unwrap_or(1200.0);
    let first = (s.axis.start_ms / 1000.0 / step).floor().max(0.0) as u64;
    for n in first.. {
        let t = n as f64 * step * 1000.0;
        let x = s.axis.col(t);
        if x >= s.w as i64 {
            break;
        }
        c.vline(x, 0..7 * s.k + 4, [150, 150, 150]);
        c.text(x + 3, 2, &mmss(t), s.k, [200, 200, 200], s.w as i64);
    }
    let beat_px = beat_px(s);
    let (lines_every, labels_every) = (
        every(beat_px * 4.0, 8.0 * k as f64),
        every(beat_px * 4.0, 36.0 * k as f64),
    );
    let y = 7 * s.k + 6;
    let mut bar = 0;
    for l in s.lines {
        bar += l.bar as usize;
        let x = s.axis.col(ms(l.frame, s.rate));
        if !(0..s.w as i64).contains(&x) {
            continue;
        }
        if l.bar && (bar - 1) % lines_every == 0 {
            c.vline(x, y..band, if s.snap_on { ORANGE } else { [255, 255, 255] });
            if (bar - 1) % labels_every == 0 {
                c.text(x + 3, y, &bar.to_string(), s.k, [255, 255, 255], s.w as i64);
            }
        } else if !l.bar && beat_px >= 6.0 * k as f64 {
            c.vline(x, band - 4 * s.k..band, [160, 160, 160]);
        }
    }
}

/// `base` with the 2-px playhead at pixel column `x`.
pub fn with_playhead(base: &Canvas, x: i64) -> slint::Image {
    let mut c = base.clone();
    playhead(&mut c, x);
    c.image()
}
