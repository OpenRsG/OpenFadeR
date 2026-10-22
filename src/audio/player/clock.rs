//! The position clock: what was heard, not what was last written.
//!
//! Position = the sample being *heard*, not the one last written. Each output callback records an
//! [`Anchor`]: (wall-clock `Instant`, source-frame position of the buffer's first frame, output
//! latency, source frames heard per second, loop). Latency is
//! `OutputCallbackInfo::timestamp().playback - .callback` (observed on ALSA: buffer delay reported
//! by the driver); when the host reports none, the buffer duration is used. `Player::position_ms`
//! picks the newest anchor already audible and extrapolates from it with a fresh `Instant`, so
//! redraws between callbacks stay smooth ([`heard_frames`], pure and unit-tested). Anchors of the
//! last `latency` seconds are kept, so a rate change or a loop wrap shows up in the reported position
//! exactly when it becomes audible, not `latency` early.

use super::super::{Loop, advance};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Never extrapolate further than this past the last callback (stalled audio thread: freeze, don't run away).
const MAX_EXTRAPOLATE: Duration = Duration::from_millis(250);
/// Upper bound on kept anchors (latency / buffer duration stays far below this).
const MAX_ANCHORS: usize = 64;

/// What the audio thread knew at one callback.
#[derive(Clone, Copy)]
pub(super) struct Anchor {
    pub(super) at: Instant,
    /// Source frame that goes first into the buffer written at `at`; the resting position when not playing.
    pub(super) pos: f64,
    /// Time from `at` until that first frame is audible.
    pub(super) latency: Duration,
    /// Source frames heard per second: `rate * src_rate`.
    pub(super) fps: f64,
    /// Loop in source frames, in force for this buffer.
    pub(super) lp: Loop,
}

pub(super) struct Clock {
    /// Oldest first; `[0]` is the anchor audible at the newest callback.
    anchors: VecDeque<Anchor>,
    /// Reported position never goes below this (start of play / seek target) before the first
    /// anchor of a run is audible, so compensating latency cannot move the playhead backwards.
    floor: f64,
    pub(super) playing: bool,
}

impl Clock {
    pub(super) fn new() -> Clock {
        Clock { anchors: VecDeque::with_capacity(MAX_ANCHORS + 1), floor: 0.0, playing: false }
    }

    /// Record a callback. `reset`: seek or start of play, older anchors no longer describe the audio.
    pub(super) fn push(&mut self, a: Anchor, playing: bool, reset: bool) {
        if reset {
            self.anchors.clear();
            self.floor = a.pos;
        }
        self.playing = playing;
        self.anchors.push_back(a);
        while self.anchors.len() > MAX_ANCHORS || (self.anchors.len() > 2 && self.anchors[1].at + self.anchors[1].latency <= a.at) {
            self.anchors.pop_front();
        }
    }
}

/// Source-frame position audible at `now`, clamped to `0..=frames`; frozen at the last anchor when paused.
pub(super) fn heard_frames(c: &Clock, now: Instant, frames: f64) -> f64 {
    let Some(last) = c.anchors.back() else { return 0.0 };
    if !c.playing {
        return last.pos.min(frames);
    }
    // Seconds since the anchor's first frame became audible (negative before that).
    let dt = |a: &Anchor| now.saturating_duration_since(a.at).min(MAX_EXTRAPOLATE).as_secs_f64() - a.latency.as_secs_f64();
    let h = match c.anchors.iter().rev().find(|a| a.at + a.latency <= now) {
        Some(a) => advance(a.pos, dt(a).max(0.0) * a.fps, a.lp),
        // Nothing audible yet: run the oldest backwards in time, not past where this run started.
        None => {
            let a = &c.anchors[0];
            let h = a.pos + dt(a) * a.fps;
            if a.fps >= 0.0 { h.max(c.floor) } else { h.min(c.floor) }
        }
    };
    h.clamp(0.0, frames)
}

/// The audio of the current run is being heard: playing and the run's first buffer is past its output latency (after a seek or the
/// start of play the reported position waits at the target until then), or not playing.
pub(super) fn audible(c: &Clock, now: Instant) -> bool {
    !c.playing || c.anchors.front().is_some_and(|a| a.at + a.latency <= now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::step;

    const SRC: f64 = 44100.0;
    const DEV: u32 = 48000;
    const BUF: usize = 480; // 10 ms at DEV
    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn clock() -> Clock {
        Clock { anchors: VecDeque::new(), floor: 0.0, playing: true }
    }

    /// Drive `ms` of wall clock the way the device would: a callback every 10 ms (rate from
    /// `rate_at(ms)`), the clock queried every ms right after. Returns (ms, heard frames) samples.
    fn run(start: f64, lp: Loop, lat_ms: u64, ms: u64, rate_at: impl Fn(u64) -> f64) -> Vec<(u64, f64)> {
        let (t0, mut c, mut pos, mut out) = (Instant::now(), clock(), start, vec![]);
        for t in 0..ms {
            if t % 10 == 0 {
                let rate = rate_at(t);
                c.push(Anchor { at: t0 + MS(t), pos, latency: MS(lat_ms), fps: rate * SRC, lp }, true, t == 0);
                for _ in 0..BUF {
                    pos = advance(pos, step(SRC, rate, DEV), lp);
                }
            }
            out.push((t, heard_frames(&c, t0 + MS(t), 1e9)));
        }
        out
    }

    #[test]
    fn position_tracks_wall_clock_times_rate_across_callbacks() {
        for rate in [1.0, 1.08, 0.92] {
            let h = run(0.0, None, 30, 400, |_| rate);
            let mut last = f64::MIN;
            for &(t, f) in &h {
                // audible after the 30 ms latency; before that held at the start of play
                let want = (t.saturating_sub(30)) as f64 / 1000.0 * rate * SRC;
                assert!((f - want).abs() < 1e-3, "rate {rate} t {t}: {f} vs {want}");
                assert!(f >= last);
                last = f;
            }
        }
    }

    #[test]
    fn a_seek_is_audible_only_after_its_output_latency() {
        let (t0, mut c) = (Instant::now(), clock());
        let a = |t: u64, pos: f64| Anchor { at: t0 + MS(t), pos, latency: MS(40), fps: SRC, lp: None };
        c.push(a(0, 0.0), true, true);
        assert!(!audible(&c, t0 + MS(39)) && audible(&c, t0 + MS(40)));
        c.push(a(100, 4410.0), true, false);
        assert!(audible(&c, t0 + MS(100)), "a plain callback keeps the run audible");
        // A seek resets the run: the position holds at the target until the jump is heard.
        c.push(a(200, 88_200.0), true, true);
        assert!(!audible(&c, t0 + MS(220)) && audible(&c, t0 + MS(240)));
        c.push(a(300, 88_200.0), false, true);
        assert!(audible(&c, t0 + MS(300)), "paused: the resting position is the one heard");
    }

    #[test]
    fn rate_change_reanchors_without_a_jump() {
        // 1.0 until callback at 200 ms, then 1.08. The new rate is audible from 200 + 30 ms.
        let h = run(0.0, None, 30, 500, |t| if t < 200 { 1.0 } else { 1.08 });
        let mut last = 0.0;
        for &(t, f) in &h {
            let want = if t <= 230 { t.saturating_sub(30) as f64 * 44.1 } else { 200.0 * 44.1 + (t - 230) as f64 * 44.1 * 1.08 };
            assert!((f - want).abs() < 1e-3, "t {t}: {f} vs {want}");
            assert!(f - last <= 1.08 * 44.1 + 1e-6, "jump at {t}");
            assert!(f >= last);
            last = f;
        }
    }

    #[test]
    fn clock_stays_inside_loop_and_restarts_at_its_start() {
        let lp = Some((1000.0, 2000.0));
        let h = run(1500.0, lp, 20, 400, |_| 1.0);
        let mut wraps = 0;
        for w in h.windows(2) {
            let (a, b) = (w[0].1, w[1].1);
            assert!((1000.0..2000.0).contains(&b), "t {}: {b}", w[1].0);
            if b < a {
                wraps += 1;
                assert!(b < 1000.0 + 45.0, "continues from the start: {b}"); // one ms = 44.1 frames
            }
        }
        // 400 ms = 17640 frames; starts 20 ms after the first anchor: (17640 - 882 + 500) / 1000 loops
        assert_eq!(wraps, 17);
    }

    #[test]
    fn rate_change_inside_loop_stays_monotonic_between_wraps() {
        let lp = Some((1000.0, 2000.0));
        let h = run(1500.0, lp, 30, 600, |t| if t < 300 { 0.92 } else { 1.08 });
        assert!(h.iter().all(|&(_, f)| (1000.0..2000.0).contains(&f)));
    }

    #[test]
    fn heard_lags_written_by_latency_and_advances_with_wall_clock() {
        let t0 = Instant::now();
        let mut c = clock();
        c.push(Anchor { at: t0, pos: 44100.0, latency: MS(50), fps: SRC, lp: None }, true, false);
        // Before its first frame is audible: `latency` older than the first written one (no floor here).
        assert_eq!(heard_frames(&c, t0, 1e9), 44100.0 - 0.05 * SRC);
        assert_eq!(heard_frames(&c, t0 + MS(50), 1e9), 44100.0);
        let mut last = f64::MIN;
        for ms in 0..200 {
            let h = heard_frames(&c, t0 + MS(ms), 1e9);
            assert!(h >= last);
            last = h;
        }
    }

    #[test]
    fn heard_is_clamped() {
        let t0 = Instant::now();
        // Latency larger than the elapsed time cannot push below the floor (start of play).
        let mut c = clock();
        c.push(Anchor { at: t0, pos: 1000.0, latency: MS(80), fps: SRC, lp: None }, true, true);
        assert_eq!(heard_frames(&c, t0, 1e9), 1000.0);
        // A stalled audio thread: extrapolation stops after MAX_EXTRAPOLATE.
        let mut c = clock();
        c.push(Anchor { at: t0, pos: 0.0, latency: MS(0), fps: SRC, lp: None }, true, true);
        let cap = heard_frames(&c, t0 + MS(250), 1e9);
        assert_eq!(heard_frames(&c, t0 + MS(10_000), 1e9), cap);
        // Never past the end of the track.
        assert_eq!(heard_frames(&c, t0 + MS(100), 1000.0), 1000.0);
    }

    #[test]
    fn heard_is_frozen_while_paused() {
        let t0 = Instant::now();
        let mut c = clock();
        c.push(Anchor { at: t0, pos: 12345.0, latency: MS(50), fps: SRC, lp: None }, false, true);
        for ms in [0, 1, 100, 5000] {
            assert_eq!(heard_frames(&c, t0 + MS(ms), 1e9), 12345.0);
        }
        assert_eq!(heard_frames(&c, t0, 100.0), 100.0);
    }
}
