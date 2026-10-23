//! Quantize (the Q button) and its settings; the beat maths is pure.
//! * SNAP: with Q on, set-points (cue, hot cue, memory cue, loop in, auto loop start) go to the nearest SNAP
//!   fraction of a beat (`super::snap`, used by [`Deck::mark`]).
//! * QUANTIZE HOT CUE / LOOP: calling a stored hot cue or loop while playing with Q on keeps the beat. IMMEDIATE jumps at
//!   once to the point plus the playhead's phase inside a QUANTIZE-length beat ([`shifted`]; a loop is activated as stored and the
//!   playhead goes into it, [`Call::landing`]); NEXT BEAT plays on to the next boundary and jumps there ([`next_boundary`], fired
//!   by [`Deck::quantize_tick`], [`due`]; inside a loop the boundary is where the loop wraps). Both hand the player the position
//!   heard when the target was worked out, so the jump catches up the output latency and the wait for the next audio
//!   buffer (`Player::seek_in_phase`): repeated presses land on the same beat phase, not up to a buffer apart.
//! * QUANTIZE REVERSE: with Q on REVERSE engages slip, so releasing it returns to where forward play would be (slip.rs).
//! * Q on all decks: the Q button of one deck switches every deck.
//!
//! ponytail: a NEXT BEAT jump fires on the 16 ms tick, so it lands that much (plus the output latency) into the point, on the
//! beat; landing exactly on the point would need the player to take a timed seek.

use super::{DECKS, Deck, beat_at, ms_at, prefs};
use crate::app::prefs::controller::QuantizeMode;
use openfader::analysis::Beat;

/// A call that waits longer than this (ms of track) lost its boundary (seek back): dropped.
const MAX_WAIT_MS: f64 = 4_000.0;

/// A stored point being called.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Call {
    /// Hot cue: jump there and play.
    Hot(u32),
    /// Stored loop: activate it and jump to its start.
    Loop(u32, u32),
}

impl Call {
    fn start(self) -> u32 {
        match self {
            Call::Hot(ms) | Call::Loop(ms, _) => ms,
        }
    }

    /// Where the call puts the playhead when the playhead's beat phase moves its start on to `ms`: a cue lands there; a loop is
    /// activated as stored (moving it would move its seam off the beat it was stored on, and RELOOP would creep it on) and the
    /// playhead goes into it, wrapped when the phase is longer than the loop.
    fn landing(self, ms: u32) -> u32 {
        match self {
            Call::Hot(_) => ms,
            Call::Loop(s, e) => s + ms.saturating_sub(s) % (e - s).max(1),
        }
    }
}

/// What a quantized call does: at once (`Now`, landing at this ms) or at the boundary `At` (ms).
#[derive(Debug, PartialEq)]
pub enum Plan {
    Now(u32),
    At(f64),
}

/// `call` while playing at `pos_ms` with `q`-beat quantize; `None` without a usable grid (the call then runs as is).
pub fn plan(beats: &[Beat], pos_ms: f64, mode: QuantizeMode, q: f64, call: Call) -> Option<Plan> {
    match mode {
        QuantizeMode::Immediate => {
            shifted(beats, pos_ms, call.start(), q).map(|ms| Plan::Now(call.landing(ms)))
        }
        QuantizeMode::NextBeat => next_boundary(beats, pos_ms, q).map(Plan::At),
    }
}

/// Beats the playhead is into its current `q`-beat (0..q).
fn phase(beats: &[Beat], pos_ms: f64, q: f64) -> Option<f64> {
    Some(beat_at(beats, pos_ms)?.rem_euclid(q))
}

/// `target_ms` moved by the playhead's phase inside a `q`-beat: landing there, the next boundary comes when it would have.
pub fn shifted(beats: &[Beat], pos_ms: f64, target_ms: u32, q: f64) -> Option<u32> {
    let at = beat_at(beats, target_ms as f64)? + phase(beats, pos_ms, q)?;
    ms_at(beats, at).map(|ms| ms.round().max(0.0) as u32)
}

/// Time (ms) of the first `q`-beat boundary after `pos_ms`.
pub fn next_boundary(beats: &[Beat], pos_ms: f64, q: f64) -> Option<f64> {
    ms_at(beats, ((beat_at(beats, pos_ms)? / q).floor() + 1.0) * q)
}

impl Deck {
    /// Q on or off for this deck (view included).
    fn set_quantize(&self, on: bool) {
        self.quantize.set(on);
        self.show(|v| v.quantize = on);
    }

    /// The Q button: this deck, or every deck with the "all decks" setting.
    pub fn toggle_quantize(&self) {
        let on = !self.quantize.get();
        let all = prefs::with(|p| p.controller.quantize_all_decks);
        (0..DECKS)
            .filter_map(|i| self.deck_at(i))
            .filter(|d| all || d.index == self.index)
            .for_each(|d| d.set_quantize(on));
    }

    /// REVERSE takes slip with it while Q and QUANTIZE REVERSE are on.
    pub(super) fn reverse_quantized(&self) -> bool {
        self.quantize.get() && prefs::with(|p| p.controller.quantize_reverse)
    }

    /// Calls a stored hot cue / loop: while playing with Q on and its QUANTIZE switch on the beat is kept (see the module doc).
    pub(super) fn call(&self, call: Call) {
        let (plan, pos) = prefs::with(|p| {
            let c = &p.controller;
            let l = self.loaded.borrow();
            let (pos, playing) = self.clock(&l);
            let on = self.quantize.get()
                && playing
                && if matches!(call, Call::Hot(_)) {
                    c.quantize_hot_cue
                } else {
                    c.quantize_loop
                };
            let planned = on.then(|| {
                plan(
                    &l.analysis.beats,
                    pos,
                    c.quantize_mode,
                    c.quantize_length.beats(),
                    call,
                )
            });
            (planned.flatten(), pos)
        });
        match plan {
            None => self.run(call, call.start(), None),
            Some(Plan::Now(landing)) => self.run(call, landing, Some(pos)),
            Some(Plan::At(ms)) => {
                self.pending.set(Some((ms, pos, call)));
                self.say(tr!("quantize: jumping on the next beat…"));
            }
        }
    }

    /// Jumps to `call`, the playhead landing at `landing`; `heard` (ms) = where the playhead was heard when a quantized target was
    /// worked out, so the player catches up what plays before the jump takes effect
    /// ([`openfader::audio::player::Player::seek_in_phase`]).
    fn run(&self, call: Call, landing: u32, heard: Option<f64>) {
        let seek = |ms: u32| {
            self.with_player(|p| match heard {
                Some(h) => p.seek_in_phase(ms, h),
                None => p.seek(ms),
            });
        };
        match call {
            Call::Hot(_) => {
                seek(landing);
                self.with_player(|p| p.play());
            }
            Call::Loop(s, e) => {
                let l = self.loaded.borrow_mut();
                self.apply_loop(l, Some((s, e)));
                seek(landing);
            }
        }
    }

    /// Per tick: a call waiting for its boundary runs when the playhead gets there (caught up from the boundary, so it lands
    /// on the beat however late the tick saw it); a deck that stopped drops it.
    pub(super) fn quantize_tick(&self) {
        let Some((at, seen, call)) = self.pending.get() else {
            return;
        };
        let (pos, playing, lp) = {
            let l = self.loaded.borrow();
            let (pos, playing) = self.clock(&l);
            (pos, playing, l.loop_)
        };
        if !playing || at - pos > MAX_WAIT_MS {
            self.pending.set(None);
        } else if let Some(heard) = due(at, seen, pos, lp) {
            self.pending.set(None);
            self.run(call, call.start(), Some(heard));
        } else {
            self.pending.set(Some((at, pos, call)));
        }
    }
}

/// A call waits for the boundary `at`; `seen` = where the playhead was at the last tick, `pos` = now. `Some(heard)` when it is
/// due: `heard` = the position of the playhead at the boundary. A loop whose end is at or before `at` is never passed, it wraps (the
/// playhead jumps back by more than half the loop): that is the boundary, and the playhead is at the loop's start.
fn due(at: f64, seen: f64, pos: f64, lp: Option<(u32, u32)>) -> Option<f64> {
    if pos >= at {
        return Some(at);
    }
    lp.filter(|&(s, e)| at >= e as f64 && seen - pos > (e - s) as f64 / 2.0)
        .map(|(s, _)| s as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::deck::tests::grid;

    #[test]
    fn immediate_keeps_the_phase_inside_the_quantize_beat() {
        let g = grid(0, 16); // beats every 500 ms
        // playhead 120 ms into beat 2 (1120 ms), cue at beat 6 (3000 ms): lands 120 ms after it
        assert_eq!(shifted(&g, 1120.0, 3000, 1.0), Some(3120));
        // quarter-beat quantize: 1200 ms is beat 2.4, 0.15 beat into its quarter → lands 75 ms after the cue
        assert_eq!(shifted(&g, 1200.0, 3000, 0.25), Some(3075));
        // half-beat quantize: 1370 ms is beat 2.74, 0.24 beat into its half
        assert_eq!(shifted(&g, 1370.0, 3000, 0.5), Some(3120));
        // on the beat: no shift
        assert_eq!(shifted(&g, 1000.0, 3000, 1.0), Some(3000));
    }

    #[test]
    fn next_beat_waits_for_the_boundary_after_the_playhead() {
        let g = grid(0, 16);
        assert_eq!(next_boundary(&g, 1120.0, 1.0), Some(1500.0));
        assert_eq!(
            next_boundary(&g, 1500.0, 1.0),
            Some(2000.0),
            "on a boundary: the next one"
        );
        assert_eq!(next_boundary(&g, 1120.0, 0.5), Some(1250.0));
        assert_eq!(next_boundary(&g, 1120.0, 2.0), Some(2000.0));
    }

    #[test]
    fn plan_by_mode_and_without_a_grid() {
        let g = grid(0, 16);
        assert_eq!(
            plan(
                &g,
                1120.0,
                QuantizeMode::Immediate,
                1.0,
                Call::Loop(3000, 5000)
            ),
            Some(Plan::Now(3120))
        );
        assert_eq!(
            plan(&g, 1120.0, QuantizeMode::NextBeat, 1.0, Call::Hot(3000)),
            Some(Plan::At(1500.0))
        );
        assert_eq!(
            plan(&[], 1120.0, QuantizeMode::Immediate, 1.0, Call::Hot(3000)),
            None
        );
    }

    #[test]
    fn a_stored_loop_is_called_as_stored_and_only_the_playhead_goes_by_the_phase() {
        let g = grid(0, 64);
        let call = Call::Loop(3000, 5000);
        // RELOOP / the list / a bank call the same loop again and again: only the landing follows the playhead's phase
        let landing = |pos| plan(&g, pos, QuantizeMode::Immediate, 1.0, call);
        assert_eq!(
            [1120.0, 1370.0, 1620.0].map(landing),
            [3120, 3370, 3120].map(|ms| Some(Plan::Now(ms)))
        );
        // a phase longer than the loop goes round it
        assert_eq!(
            plan(
                &g,
                1370.0,
                QuantizeMode::Immediate,
                1.0,
                Call::Loop(3000, 3200)
            ),
            Some(Plan::Now(3170))
        );
    }

    #[test]
    fn a_call_waiting_at_a_loops_end_is_due_when_the_loop_wraps() {
        let g = grid(0, 64);
        let lp = Some((1000, 3000)); // 4 beats
        let at = next_boundary(&g, 2700.0, 1.0).unwrap();
        assert_eq!(at, 3000.0, "the boundary is the loop's end");
        // the heard playhead runs round the loop for good: the call still comes due, at the wrap, from the loop's start
        let (mut seen, mut due_at) = (2700.0, None);
        for t in (0..4000).step_by(16) {
            let pos = 1000.0 + (1700.0 + t as f64) % 2000.0;
            due_at = due_at.or(due(at, seen, pos, lp));
            seen = pos;
        }
        assert_eq!(due_at, Some(1000.0));
        // an ordinary boundary inside the loop, and a playhead going backwards (reverse), are as before
        assert_eq!(due(2500.0, 2490.0, 2501.0, lp), Some(2500.0));
        assert_eq!(due(3000.0, 2000.0, 1990.0, lp), None);
        assert_eq!(due(3000.0, 2990.0, 2995.0, None), None);
    }
}
