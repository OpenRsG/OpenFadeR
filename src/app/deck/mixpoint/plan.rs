//! MIX POINT LINK maths, no decks and no Slint: where the MIX IN deck has to be when the MIX OUT deck is at its point, which
//! cue MIX POINT SELECT takes next, the panel's time axis and zoom. Times are ms; a deck's tempo is its playback rate.

use openfader::analysis::Beat;

/// A MIX OUT deck that is already this far past its point (ms) has missed the mix: nothing starts any more.
pub const LATE_MS: f64 = 200.0;
/// Zoom of the panel's waveforms, ms of the window.
pub const SPAN_MS: (f64, f64) = (2_000.0, 20.0 * 60_000.0);
pub const DEFAULT_SPAN_MS: f64 = 16_000.0;
/// Where the grid switches off with "Display only when waveform is enlarged".
pub const ENLARGED_MS: f64 = 32_000.0;
/// Grids are not drawn closer than this many px between beats (they turn into a solid block).
pub const MIN_BEAT_PX: f64 = 4.0;

/// Where the MIX IN deck (point `in_ms`, rate `r_in`) has to be while the MIX OUT deck (at `out_pos`, rate `r_out`)
/// heads for its point `out_ms`: both points reach the speakers together. Negative = the IN deck would have had to
/// start before its own beginning.
pub fn in_position(out_pos: f64, out_ms: f64, r_out: f64, in_ms: f64, r_in: f64) -> f64 {
    in_ms - (out_ms - out_pos) / r_out * r_in
}

/// The position (ms) to start the MIX IN deck at now, or `None` while it is not time yet (it would start before its
/// beginning) or the mix is missed ([`LATE_MS`] past the MIX OUT point) or the IN deck would start past its end.
pub fn start_at(
    out_pos: f64,
    out_ms: f64,
    r_out: f64,
    in_ms: f64,
    r_in: f64,
    in_duration: f64,
) -> Option<u32> {
    if out_pos > out_ms + LATE_MS {
        return None;
    }
    let at = in_position(out_pos, out_ms, r_out, in_ms, r_in);
    (at >= 0.0 && at < in_duration).then_some(at.round() as u32)
}

/// MIX POINT SELECT: the next of `cues` (ms) after `current`, wrapping to the first; a track without cues
/// takes `fallback` (the current cue point).
pub fn next_cue(
    cues: impl Iterator<Item = u32> + Clone,
    current: Option<u32>,
    fallback: u32,
) -> u32 {
    match (cues.clone().min(), current) {
        (None, _) => fallback,
        (Some(first), None) => first,
        (Some(first), Some(cur)) => cues.filter(|&c| c > cur).min().unwrap_or(first),
    }
}

/// The cue of `cues` within `tol` ms of `ms` (the nearest), else `ms`: a click on a cue marker takes the cue's own time.
pub fn snap_to_cue(cues: impl IntoIterator<Item = u32>, ms: u32, tol: f64) -> u32 {
    cues.into_iter()
        .filter(|&c| (c as f64 - ms as f64).abs() <= tol)
        .min_by_key(|&c| c.abs_diff(ms))
        .unwrap_or(ms)
}

/// SILENT: the MIX IN deck is muted from its beginning up to its silent end.
pub fn muted(pos: f64, silent_end: u32) -> bool {
    pos < silent_end as f64
}

/// "Automatically changes the BEAT/BPM SYNC setting": a track whose grid tempo drifts (more than 1 % from its first beat)
/// cannot keep BEAT SYNC, which would chase the drift: it follows the tempo only.
pub fn inconsistent_bpm(beats: &[Beat]) -> bool {
    let Some(first) = beats.first().filter(|b| b.bpm_x100 > 0) else {
        return false;
    };
    let base = f64::from(first.bpm_x100);
    beats
        .iter()
        .any(|b| (f64::from(b.bpm_x100) - base).abs() > base * 0.01)
}

/// A deck's place on the panel's time axis: `t = 0` is the mix point, `t` runs in wall-clock ms at the deck's rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lane {
    /// Track ms of the mix point and the track's length.
    pub point: f64,
    pub duration: f64,
    pub rate: f64,
}

impl Lane {
    /// Track ms at axis time `t`.
    pub fn track_ms(&self, t: f64) -> f64 {
        self.point + t * self.rate
    }
    /// Axis time of track ms `ms`.
    pub fn time_of(&self, ms: f64) -> f64 {
        (ms - self.point) / self.rate
    }
    /// The track's start and end on the axis.
    pub fn extent(&self) -> (f64, f64) {
        (self.time_of(0.0), self.time_of(self.duration))
    }
}

/// The part of the axis the lanes cover (what the overview shows), with a margin so a lane's end is not on the border.
pub fn extent(lanes: impl IntoIterator<Item = Lane>) -> (f64, f64) {
    let span = lanes
        .into_iter()
        .map(|l| l.extent())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (a, b)| {
            (lo.min(a), hi.max(b))
        });
    if span.0 > span.1 {
        return (-DEFAULT_SPAN_MS / 2.0, DEFAULT_SPAN_MS / 2.0);
    }
    let pad = (span.1 - span.0) * 0.02;
    (span.0 - pad, span.1 + pad)
}

/// The panel's window on the axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub centre: f64,
    pub span: f64,
}

impl Default for View {
    fn default() -> Self {
        Self {
            centre: 0.0,
            span: DEFAULT_SPAN_MS,
        }
    }
}

impl View {
    /// Zoomed by `steps` (positive = in, a quarter per step); "Automatically limits the zoom out range" stops at
    /// the whole `extent` (else at [`SPAN_MS`]'s end).
    pub fn zoom(&mut self, steps: f64, extent: (f64, f64), limit: bool) {
        let max = if limit {
            (extent.1 - extent.0).clamp(SPAN_MS.0, SPAN_MS.1)
        } else {
            SPAN_MS.1
        };
        self.span = (self.span * 1.25f64.powf(-steps)).clamp(SPAN_MS.0, max.max(SPAN_MS.0));
        self.clamp(extent);
    }

    /// Centre moved to `t`, kept inside the extent.
    pub fn move_to(&mut self, t: f64, extent: (f64, f64)) {
        self.centre = t;
        self.clamp(extent);
    }

    /// The window may not leave the extent (a window wider than it stays centred on it).
    pub fn clamp(&mut self, (lo, hi): (f64, f64)) {
        let half = self.span / 2.0;
        self.centre = if hi - lo <= self.span {
            (lo + hi) / 2.0
        } else {
            self.centre.clamp(lo + half, hi - half)
        };
    }

    pub fn start(&self) -> f64 {
        self.centre - self.span / 2.0
    }
}

/// Time-scale label spacing: the smallest step (ms) from a round list that keeps labels at least `min_px` apart.
pub fn tick_step_ms(span_ms: f64, width_px: f64, min_px: f64) -> f64 {
    const STEPS: [f64; 12] = [
        500.0,
        1_000.0,
        2_000.0,
        5_000.0,
        10_000.0,
        15_000.0,
        30_000.0,
        60_000.0,
        120_000.0,
        300_000.0,
        600_000.0,
        1_200_000.0,
    ];
    STEPS
        .iter()
        .copied()
        .find(|s| s * width_px / span_ms >= min_px)
        .unwrap_or(STEPS[STEPS.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_in_deck_sits_where_both_points_meet_at_the_same_moment() {
        // out heads for 100 s from 70 s at rate 1.0: 30 s to go; the in point at 40 s is 30 s ahead of 10 s
        assert_eq!(
            in_position(70_000.0, 100_000.0, 1.0, 40_000.0, 1.0),
            10_000.0
        );
        // a faster out deck gets there sooner: 30 s of track = 25 s of wall clock at 1.2×; the in deck at 1.0 covers 25 s
        assert!((in_position(70_000.0, 100_000.0, 1.2, 40_000.0, 1.0) - 15_000.0).abs() < 1e-6);
    }

    #[test]
    fn the_in_deck_starts_from_its_beginning_when_its_time_comes() {
        // the in point is 40 s into its track: it starts 40 s before the out deck reaches its point
        assert_eq!(
            start_at(59_900.0, 100_000.0, 1.0, 40_000.0, 1.0, 300_000.0),
            None
        );
        assert_eq!(
            start_at(60_000.0, 100_000.0, 1.0, 40_000.0, 1.0, 300_000.0),
            Some(0)
        );
        // armed late (50 s of the lead already gone): it joins in step instead
        assert_eq!(
            start_at(90_000.0, 100_000.0, 1.0, 40_000.0, 1.0, 300_000.0),
            Some(30_000)
        );
    }

    #[test]
    fn a_missed_mix_or_an_in_point_past_the_end_never_starts() {
        assert_eq!(
            start_at(100_300.0, 100_000.0, 1.0, 0.0, 1.0, 300_000.0),
            None
        );
        assert_eq!(
            start_at(100_100.0, 100_000.0, 1.0, 0.0, 1.0, 300_000.0),
            Some(100)
        );
        assert_eq!(
            start_at(0.0, 10_000.0, 1.0, 400_000.0, 1.0, 300_000.0),
            None
        );
    }

    #[test]
    fn select_walks_the_cues_in_order_and_wraps() {
        let cues = [30_000, 5_000, 5_000, 12_000].into_iter();
        assert_eq!(next_cue(cues.clone(), None, 1), 5_000);
        assert_eq!(next_cue(cues.clone(), Some(5_000), 1), 12_000);
        assert_eq!(next_cue(cues.clone(), Some(12_000), 1), 30_000);
        assert_eq!(next_cue(cues.clone(), Some(30_000), 1), 5_000);
        assert_eq!(next_cue(cues, Some(7), 1), 5_000);
        assert_eq!(
            next_cue(std::iter::empty(), Some(7), 1234),
            1234,
            "no cue: the current cue point"
        );
    }

    #[test]
    fn a_click_near_a_cue_marker_takes_the_cue() {
        let cues = [1_000, 1_100, 9_000];
        assert_eq!(snap_to_cue(cues, 1_090, 50.0), 1_100);
        assert_eq!(snap_to_cue(cues, 1_030, 100.0), 1_000, "the nearer one");
        assert_eq!(snap_to_cue(cues, 5_000, 50.0), 5_000);
    }

    #[test]
    fn silent_mutes_up_to_the_silent_end_only() {
        assert!(muted(0.0, 30_000) && muted(29_999.0, 30_000));
        assert!(!muted(30_000.0, 30_000) && !muted(0.0, 0));
    }

    #[test]
    fn a_drifting_grid_is_inconsistent() {
        let beat = |bpm_x100| Beat {
            bar_pos: 1,
            bpm_x100,
            time_ms: 0,
        };
        assert!(!inconsistent_bpm(&[]));
        assert!(!inconsistent_bpm(&[
            beat(12_000),
            beat(12_050),
            beat(12_000)
        ]));
        assert!(inconsistent_bpm(&[
            beat(12_000),
            beat(12_000),
            beat(12_400)
        ]));
    }

    #[test]
    fn lanes_map_axis_time_to_track_time_at_their_rate() {
        let l = Lane {
            point: 40_000.0,
            duration: 200_000.0,
            rate: 1.25,
        };
        assert_eq!(l.track_ms(8_000.0), 50_000.0);
        assert_eq!(l.time_of(50_000.0), 8_000.0);
        let (a, b) = l.extent();
        assert_eq!((a, b), (-32_000.0, 128_000.0));
    }

    #[test]
    fn the_extent_covers_every_lane_with_a_margin() {
        let out = Lane {
            point: 100_000.0,
            duration: 300_000.0,
            rate: 1.0,
        };
        let inn = Lane {
            point: 20_000.0,
            duration: 200_000.0,
            rate: 1.0,
        };
        let (lo, hi) = extent([out, inn]);
        assert!(lo < -100_000.0 && hi > 200_000.0);
        assert_eq!(extent([]), (-8_000.0, 8_000.0));
    }

    #[test]
    fn zoom_out_stops_at_the_whole_extent_when_limited() {
        let ext = (-60_000.0, 60_000.0);
        let mut v = View::default();
        v.zoom(-30.0, ext, true);
        assert_eq!(v.span, 120_000.0);
        v.zoom(-30.0, ext, false);
        assert_eq!(v.span, SPAN_MS.1);
        v.zoom(100.0, ext, true);
        assert_eq!(v.span, SPAN_MS.0);
    }

    #[test]
    fn the_window_stays_inside_the_extent() {
        let ext = (-60_000.0, 60_000.0);
        let mut v = View {
            centre: 0.0,
            span: 16_000.0,
        };
        v.move_to(500_000.0, ext);
        assert_eq!(v.centre, 52_000.0);
        v.move_to(-500_000.0, ext);
        assert_eq!(v.start(), -60_000.0);
        v.span = 200_000.0;
        v.clamp(ext);
        assert_eq!(v.centre, 0.0, "wider than the extent: centred");
    }

    #[test]
    fn time_labels_keep_their_distance() {
        // 16 s over 800 px = 50 px/s: 2 s steps are 100 px apart, 1 s steps only 50
        assert_eq!(tick_step_ms(16_000.0, 800.0, 80.0), 2_000.0);
        assert_eq!(tick_step_ms(1_200_000.0, 800.0, 80.0), 120_000.0);
    }
}
