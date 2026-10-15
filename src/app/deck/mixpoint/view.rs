//! The panel's pictures, drawn 1:1 into buffers of the element's physical size: a lane (one deck's waveform around its
//! MIX POINT, the two lanes share the time axis of [`super::plan::Lane`]) and the overview of both tracks with the white
//! frame of what the lanes show.

use super::plan::{self, Lane, View};
use crate::app::render::{self, Axis, Canvas, Rgb, Wave};
use openfader::analysis::{Beat, Cue};

const ACCENT: Rgb = [20, 115, 235]; // Theme.accent
const CUE: Rgb = [255, 185, 55]; // Theme.cue
const BAR: Rgb = [230, 40, 40]; // Theme.wave-bar
const GRID: Rgb = [128, 128, 128]; // Theme.wave-grid
const DIM: Rgb = [150, 150, 150]; // Theme.text-dim
const SILENT: Rgb = [230, 40, 40]; // Theme.danger
const LINE: Rgb = [0, 125, 225]; // the one deck colour used for every deck
const WHITE: Rgb = [255, 255, 255];

/// A deck as a lane draws it.
pub struct Src<'a> {
    pub wave: Option<&'a Wave>,
    pub beats: &'a [Beat],
    pub cues: &'a [Cue],
    pub lane: Lane,
    /// The playhead, track ms.
    pub pos: f64,
}

/// PREFS > MIX POINT LINK: "Display time", "Display grids" and "Display only when waveform is enlarged".
#[derive(Clone, Copy)]
pub struct Opts {
    pub time: bool,
    pub grids: bool,
    pub grids_enlarged_only: bool,
}

/// What a lane marks besides the waveform: the MIX POINT (blue once linked, a lane without one has none) and the end of SILENT.
#[derive(Clone, Copy, Default)]
pub struct Marks {
    pub point: bool,
    pub linked: bool,
    pub silent_end: Option<u32>,
}

/// `m:ss` of a track time.
fn clock(ms: f64) -> String {
    let s = (ms / 1000.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Beat lines: every beat, or only the bar starts when the beats would be closer than [`plan::MIN_BEAT_PX`]; nothing when
/// even the bars would be.
fn grid(c: &mut Canvas, axis: &Axis, beats: &[Beat]) {
    let Some(first) = beats.first().filter(|b| b.bpm_x100 > 0) else {
        return;
    };
    let beat_px = 6_000_000.0 / f64::from(first.bpm_x100) * axis.px_per_ms;
    if beat_px * 4.0 < plan::MIN_BEAT_PX {
        return;
    }
    let bars_only = beat_px < plan::MIN_BEAT_PX;
    let h = c.h;
    for b in beats.iter().filter(|b| !bars_only || b.bar_pos == 1) {
        let rgb = if b.bar_pos == 1 { BAR } else { GRID };
        c.vline(axis.col(f64::from(b.time_ms)), 0..h, rgb);
    }
}

/// "Display time": the track time under a tick along the bottom edge.
fn time_scale(c: &mut Canvas, lane: &Lane, v: View) {
    let (w, h) = (c.w, c.h);
    let step = plan::tick_step_ms(v.span, w as f64, 64.0);
    let mut t = (v.start() / step).ceil() * step;
    while t <= v.start() + v.span {
        let ms = lane.track_ms(t);
        if (0.0..=lane.duration).contains(&ms) {
            let x = ((t - v.start()) / v.span * w as f64) as i64;
            c.vline(x, h.saturating_sub(4)..h, DIM);
            c.text(x + 2, h.saturating_sub(10), &clock(ms), 1, DIM, w as i64);
        }
        t += step;
    }
}

/// One deck's waveform on the shared axis, `w`×`h` px.
pub fn lane(s: &Src, v: View, o: Opts, m: Marks, w: usize, h: usize) -> slint::Image {
    let mut c = Canvas::new(w, h);
    let axis = Axis {
        start_ms: s.lane.track_ms(v.start()),
        px_per_ms: w as f64 / v.span / s.lane.rate,
    };
    if o.grids && (!o.grids_enlarged_only || v.span <= plan::ENLARGED_MS) {
        grid(&mut c, &axis, s.beats);
    }
    if let Some(wave) = s.wave {
        wave.draw(&mut c, &axis, true);
    }
    for cue in s.cues {
        c.vline(
            axis.col(f64::from(cue.time_ms)),
            (0..h).step_by(2),
            render::cue_color(cue),
        );
    }
    if let Some(end) = m.silent_end {
        c.shade(axis.col(0.0), axis.col(f64::from(end)), 0..h, SILENT, 0.35);
    }
    if m.point {
        let x = axis.col(s.lane.point);
        let rgb = if m.linked { ACCENT } else { CUE };
        c.vline(x - 1, 0..h, rgb);
        c.vline(x, 0..h, WHITE);
        c.vline(x + 1, 0..h, rgb);
    }
    render::playhead(&mut c, axis.x(s.pos).round() as i64);
    if o.time {
        time_scale(&mut c, &s.lane, v);
    }
    c.image()
}

/// Both tracks as lines (their length on the axis) with the playheads, the MIX POINT and the white frame of `v`, `w`×`h` px;
/// `ext` is the axis range the picture spans.
pub fn overview(
    lanes: &[(Lane, f64)],
    ext: (f64, f64),
    v: View,
    w: usize,
    h: usize,
) -> slint::Image {
    let mut c = Canvas::new(w, h);
    let px = |t: f64| ((t - ext.0) / (ext.1 - ext.0) * w as f64).round() as i64;
    let band = (h / lanes.len().max(1)).max(2);
    for (i, (lane, pos)) in lanes.iter().enumerate() {
        let (a, b) = lane.extent();
        let y = i * band + band / 2;
        c.shade(px(a), px(b), y.saturating_sub(1)..y + 1, LINE, 1.0);
        c.vline(px(lane.time_of(*pos)), i * band..(i + 1) * band, WHITE);
    }
    c.vline(px(0.0), 0..h, CUE);
    let (x0, x1) = (px(v.start()), px(v.start() + v.span));
    c.shade(x0, x1 + 1, 0..1, WHITE, 1.0);
    c.shade(x0, x1 + 1, h.saturating_sub(1)..h, WHITE, 1.0);
    c.vline(x0, 0..h, WHITE);
    c.vline(x1, 0..h, WHITE);
    c.image()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_times_read_m_ss() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(92_400.0), "1:32");
        assert_eq!(clock(600_000.0), "10:00");
    }

    #[test]
    fn a_lane_draws_at_the_size_asked_for() {
        let lane = Lane {
            point: 10_000.0,
            duration: 60_000.0,
            rate: 1.0,
        };
        let s = Src {
            wave: None,
            beats: &[],
            cues: &[],
            lane,
            pos: 9_000.0,
        };
        let o = Opts {
            time: true,
            grids: true,
            grids_enlarged_only: false,
        };
        let img = lane_image(&s, o);
        assert_eq!((img.size().width, img.size().height), (200, 30));
    }

    fn lane_image(s: &Src, o: Opts) -> slint::Image {
        lane(s, View::default(), o, Marks::default(), 200, 30)
    }

    #[test]
    fn the_overview_draws_at_the_size_asked_for() {
        let l = Lane {
            point: 0.0,
            duration: 100_000.0,
            rate: 1.0,
        };
        let img = overview(&[(l, 0.0)], (0.0, 100_000.0), View::default(), 100, 12);
        assert_eq!(img.size().width, 100);
    }
}
