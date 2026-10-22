//! Deck controls: play/seek, tempo and its range, key lock, quantize, beat jump, scratch, detail
//! zoom (CUE is in [`cue`](super::cue), SYNC in [`sync`](super::sync)). Each acts on the player and
//! mirrors the result into the deck's `DeckView`.

use super::{Deck, beat_at, cue::at_cue, grid_bpm, ms_at, prefs};
use openfader::{analysis::Beat, audio::Player};

/// Tempo fader travel per range step: ± this fraction, label shown on the button. ±6/±10/±16/WIDE
/// are the usual choices; the first is the start-up default. WIDE is ±100%: the fader's
/// bottom stops the deck, its top plays at double speed.
pub const RANGES: [(f64, &str); 4] = [
    (0.06, "±6%"),
    (0.10, "±10%"),
    (0.16, "±16%"),
    (1.00, n_!("WIDE")),
];

/// Detail window limits in ms.
const SPAN_MS: (u32, u32) = (2_000, 32_000);

/// Playback rate for a fader position 0..1 (0.5 = 1.0) at tempo `range` (± fraction).
pub fn rate_at(pos: f32, range: f64) -> f64 {
    1.0 + (pos as f64 - 0.5) * 2.0 * range
}

/// Fader position for a rate; rates outside the range (SYNC to a far tempo) pin to an end.
fn pos_of(rate: f64, range: f64) -> f32 {
    ((rate - 1.0) / (2.0 * range) + 0.5).clamp(0.0, 1.0) as f32
}

/// Detail window after one zoom step: `dir > 0` zooms in (halves the span), `< 0` out, `0` back to the default; 2 to 32 s.
fn zoom_span(span_ms: u32, dir: i32) -> u32 {
    match dir {
        0 => crate::app::render::detail::DEFAULT_SPAN_MS,
        d if d > 0 => span_ms / 2,
        _ => span_ms * 2,
    }
    .clamp(SPAN_MS.0, SPAN_MS.1)
}

pub fn span_text(span_ms: u32) -> String {
    format!("{} s", span_ms / 1000)
}

/// `pos_ms` moved `n` beats along the grid, keeping its beat phase (never before 0).
/// `None` without a usable grid.
fn jump(beats: &[Beat], pos_ms: f64, n: impl Into<f64>) -> Option<u32> {
    ms_at(beats, beat_at(beats, pos_ms)? + n.into()).map(|ms| ms.round().max(0.0) as u32)
}

impl Deck {
    pub(in crate::app) fn with_player<R>(&self, f: impl FnOnce(&mut Player) -> R) -> Option<R> {
        self.player.as_ref().map(|p| f(&mut p.borrow_mut()))
    }

    /// Pauses and puts the playhead on `ms`.
    pub(super) fn pause_at(&self, ms: u32) {
        self.with_player(|p| {
            p.pause();
            p.seek(ms);
        });
    }

    /// PLAY / PAUSE. A synced deck starts on the master's next beat; a press while that is armed
    /// cancels it, one during a held CUE preview latches playback.
    pub fn play_pause(&self) {
        if !self.ready() || self.cancel_start() || self.cue_latch() || self.gate.take().is_some() {
            return;
        }
        if self.playing() {
            // PAUSE brakes like a platter, over a snappy eighth of the BRAKE time (Vinyl Speed Adjust, Touch/Brake; the fraction is ours).
            let s = prefs::with(|p| p.controller.vinyl_seconds().0) / 8.0;
            self.with_player(|p| p.brake(s));
        } else if !self.play_quantized() {
            self.with_player(|p| p.play());
        }
    }

    /// Channel / cross fader start: `up` starts the deck, otherwise it stops; a CUE start returns to the cue point.
    pub fn fader_start(&self, up: bool, cue: bool) {
        if !self.ready() {
            return;
        }
        let at = (cue && !up).then(|| self.loaded.borrow().cue_ms);
        self.with_player(|p| match (up, at) {
            (true, _) => p.play(),
            (false, at) => {
                p.pause();
                if let Some(ms) = at {
                    p.seek(ms);
                }
            }
        });
    }

    /// Overview click: needle search ([`Deck::needle_free`]).
    pub fn seek_frac(&self, f: f32) {
        if !self.needle_free() {
            return;
        }
        let ms = (f.clamp(0.0, 1.0) * self.loaded.borrow().duration_ms as f32) as u32;
        self.with_player(|p| p.seek(ms));
    }

    /// The playhead to `ms` of the track (plugin `seek` command; [`Deck::needle_free`] applies).
    pub fn seek_ms(&self, ms: f64) {
        if self.ready() && self.needle_free() {
            let end = self.loaded.borrow().duration_ms;
            self.with_player(|p| p.seek((ms.max(0.0) as u32).min(end)));
        }
    }

    /// Jog search (shift + jog): the playhead moves by `ms` ([`Deck::needle_free`]).
    pub fn search(&self, ms: f64) {
        if self.needle_free() {
            self.with_player(|p| p.seek((p.position_ms() + ms).max(0.0) as u32));
        }
    }

    /// NEEDLE LOCK: no needle search while a track plays; scratching with the jog or the mouse is not search.
    fn needle_free(&self) -> bool {
        let locked = prefs::with(|p| p.controller.needle_lock) && self.playing();
        if locked {
            self.say(tr!("needle lock: no needle search while playing"));
        }
        !locked
    }

    /// Fader moved to `pos` (0..1); ignored while SYNC makes this deck follow the master.
    pub fn tempo(&self, pos: f32) {
        if !self.following() {
            self.set_rate(rate_at(pos, RANGES[self.range.get()].0));
        }
    }

    /// Playback rate (clamped to the player's 0 to 2x), shown as effective BPM and tempo %.
    pub fn set_rate(&self, rate: f64) {
        let rate = rate.clamp(0.0, 2.0);
        self.rate.set(rate);
        self.with_player(|p| p.set_rate(rate));
        let (grid, base) = {
            let l = self.loaded.borrow();
            (grid_bpm(&l.analysis.beats), l.base_bpm)
        };
        let range = RANGES[self.range.get()].0;
        self.show(|v| {
            v.tempo = pos_of(rate, range);
            v.bpm_text = super::bpm_text(grid, rate);
            v.tempo_text = super::tempo_text(rate);
            v.bpm = (base * rate) as f32;
        });
        self.refresh_key();
    }

    /// Next tempo range. The fader keeps its position, so the rate scales with the new range (a
    /// deck following the master keeps its rate and the fader is redrawn for the new range).
    pub fn cycle_range(&self) {
        let next = (self.range.get() + 1) % RANGES.len();
        self.range.set(next);
        if self.following() {
            self.set_rate(self.rate.get());
        } else {
            let pos = self.view.borrow().tempo;
            self.tempo(pos);
        }
        self.show(|v| v.tempo_range = crate::app::i18n::tr_dyn(RANGES[next].1).into());
    }

    /// Detail waveform dragged / jog touched: the platter scratches (`Player::scratch_*`). Sync holds
    /// its phase correction meanwhile.
    pub fn scratch_begin(&self) {
        self.begin_scratch(false);
    }

    /// Jog wheel touched (vinyl mode): a scratch. With JOG CUE RETURN on, one that starts while paused
    /// on the cue point sends the playhead back to it, paused, when the scratch ends.
    pub fn jog_begin(&self) {
        let back = prefs::with(|p| p.controller.jog_cue_return) && {
            let l = self.loaded.borrow();
            let (pos, playing) = self.clock(&l);
            !playing && at_cue(pos, l.cue_ms)
        };
        self.begin_scratch(back);
    }

    fn begin_scratch(&self, back_to_cue: bool) {
        if self.ready() {
            self.jog_cue.set(back_to_cue);
            self.sync.scratching.set(true);
            self.with_player(|p| p.scratch_begin());
        }
    }

    /// Platter moved by `seconds` of track time (+ forward).
    pub fn scratch_move(&self, seconds: f64) {
        self.with_player(|p| p.scratch_move(seconds));
    }

    pub fn scratch_end(&self) {
        self.sync.scratching.set(false);
        self.with_player(|p| p.scratch_end());
        if self.jog_cue.take() {
            let cue = self.loaded.borrow().cue_ms;
            self.pause_at(cue);
        }
    }

    /// JOG CUTTER gate (`Player::set_cut`): `true` cuts the channel.
    pub fn cut(&self, on: bool) {
        self.with_player(|p| p.set_cut(on));
    }

    /// Mouse drag of `dx` logical px across the detail waveform: the waveform follows the hand, so a
    /// drag right turns the platter back by that much of the visible window.
    pub fn scratch_drag(&self, dx: f32) {
        let width = self.detail_size.get().0.max(1.0) as f64;
        let span = self.span_ms.get() as f64 * self.rate().abs().max(0.1); // as the detail draws it (tick.rs)
        self.scratch_move(-(dx as f64) * span / width / 1000.0);
    }

    pub fn toggle_key_lock(&self) {
        let on = self
            .with_player(|p| {
                p.set_key_lock(!p.key_lock());
                p.key_lock()
            })
            .unwrap_or(false);
        self.show(|v| v.key_lock = on);
        self.refresh_key();
    }

    /// Detail zoom step: `dir > 0` in, otherwise out. Redrawn by the next tick (the span is in its key).
    pub fn zoom(&self, dir: i32) {
        let span = zoom_span(self.span_ms.get(), dir);
        self.span_ms.set(span);
        self.show(|v| v.zoom_text = span_text(span).into());
    }

    /// Seek `n` beats from the playhead along the beat grid. With a loop active the loop moves
    /// `n` beats instead and the playhead goes with it.
    pub fn beat_jump(&self, n: impl Into<f64>) {
        if !self.ready() {
            return;
        }
        let n = n.into();
        let l = self.loaded.borrow_mut();
        let (pos, _) = self.clock(&l);
        if let Some(cur) = l.loop_ {
            match shift_loop(&l.analysis.beats, cur, n, l.duration_ms) {
                Some(lp) => {
                    let at = (pos + lp.0 as f64 - cur.0 as f64).max(0.0) as u32;
                    self.apply_loop(l, Some(lp));
                    self.with_player(|p| p.seek(at));
                }
                None => self.say(tr!("loop cannot move that far")),
            }
            return;
        }
        let target = jump(&l.analysis.beats, pos, n).map(|ms| ms.min(l.duration_ms));
        drop(l);
        match target {
            Some(ms) => {
                self.with_player(|p| p.seek(ms));
            }
            None => self.say(tr!("beat jump needs a beat grid")),
        }
    }
}

/// Both ends of `lp` moved `n` beats along the grid; `None` without a grid, when the end would
/// pass the track (`duration_ms` 0 = unknown) or the start would hit 0.
fn shift_loop(
    beats: &[Beat],
    (s, e): (u32, u32),
    n: impl Into<f64>,
    duration_ms: u32,
) -> Option<(u32, u32)> {
    let n = n.into();
    let (s, e) = (jump(beats, s as f64, n)?, jump(beats, e as f64, n)?);
    (e > s && (duration_ms == 0 || e <= duration_ms) && !(n < 0.0 && s == 0)).then_some((s, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::deck::tests::grid;

    #[test]
    fn fader_maps_to_the_range_and_back() {
        for (range, _) in RANGES {
            assert_eq!(rate_at(0.5, range), 1.0);
            assert!(
                (rate_at(1.0, range) - (1.0 + range)).abs() < 1e-9
                    && (rate_at(0.0, range) - (1.0 - range)).abs() < 1e-9
            );
            for p in [0.0, 0.25, 0.5, 0.9, 1.0] {
                assert!((pos_of(rate_at(p, range), range) - p).abs() < 1e-6);
            }
        }
        // a rate beyond the range (SYNC) pins to the end; WIDE spans the player's 0× to 2×
        assert_eq!(pos_of(1.3, 0.16), 1.0);
        assert_eq!(rate_at(0.0, RANGES[3].0), 0.0);
        assert_eq!(rate_at(1.0, RANGES[3].0), 2.0);
        // the same fader position is a smaller tempo change on a narrower range
        assert!(rate_at(0.75, 0.06) < rate_at(0.75, 0.10));
    }

    #[test]
    fn zoom_halves_and_doubles_within_2_to_32_seconds() {
        assert_eq!([zoom_span(8000, 1), zoom_span(8000, -1)], [4000, 16000]);
        assert_eq!([zoom_span(2000, 1), zoom_span(32000, -1)], [2000, 32000]);
        assert_eq!(
            zoom_span(2000, 0),
            crate::app::render::detail::DEFAULT_SPAN_MS
        );
        assert_eq!(span_text(16000), "16 s");
    }

    #[test]
    fn jump_keeps_phase_and_clamps_at_zero() {
        let g = grid(1000, 4); // 1000, 1500, 2000, 2500
        assert_eq!(
            [jump(&g, 1250.0, 1), jump(&g, 1250.0, -1)],
            [Some(1750), Some(750)]
        );
        assert_eq!(jump(&g, 1250.0, 4), Some(3250)); // past the last beat: last interval
        assert_eq!(jump(&g, 1000.0, -4), Some(0));
        assert_eq!(jump(&[], 1000.0, 1), None);
    }

    #[test]
    fn loop_moves_by_beats_and_refuses_to_leave_the_track() {
        let g = grid(1000, 8); // 1000..4500
        assert_eq!(shift_loop(&g, (1000, 3000), 4, 10_000), Some((3000, 5000)));
        assert_eq!(shift_loop(&g, (3000, 5000), -4, 10_000), Some((1000, 3000)));
        assert_eq!(shift_loop(&g, (3000, 5000), 4, 6000), None);
        assert_eq!(shift_loop(&g, (0, 1000), -4, 6000), None);
        assert_eq!(shift_loop(&[], (0, 1000), 4, 6000), None);
    }
}
