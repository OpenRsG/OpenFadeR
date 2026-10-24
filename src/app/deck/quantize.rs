//! Quantize (the Q button) and its settings; the beat maths is pure.
//! * SNAP: with Q on, set-points (cue, hot cue, memory cue, loop in, auto loop start) go to the nearest SNAP
//!   fraction of a beat (`super::snap`, used by [`Deck::mark`]).
//! * QUANTIZE HOT CUE / LOOP: calling a stored hot cue or loop while playing with Q on keeps the beat. IMMEDIATE jumps at
//!   once; the call lands at its point plus how far into a QUANTIZE-length beat the player's own playhead is when it takes the
//!   jump (the grid's beat under the playhead goes to the player, [`window`], `Player::seek_on_beat`): the phase is read at the
//!   buffer the jump takes effect in, not at the press, so a beat boundary between the two never puts it a beat off. A loop is
//!   activated as stored (moving it would move its seam off the beat it was stored on, and RELOOP would creep it on) and the
//!   playhead goes into it, the phase wrapped round the loop when it is longer. NEXT BEAT plays on to the next boundary and
//!   jumps there ([`next_boundary`], fired by [`Deck::quantize_tick`], [`due`]; inside a loop the boundary is where the loop
//!   wraps) with the position heard at the boundary handed to the player to catch up the output latency and the wait for the
//!   next audio buffer (`Player::seek_in_phase`).
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

    /// ms the landing's phase wraps in: a loop's length, 0 (none) for a hot cue.
    fn len(self) -> f64 {
        match self {
            Call::Hot(_) => 0.0,
            Call::Loop(s, e) => e.saturating_sub(s) as f64,
        }
    }
}

/// What a quantized call does: it is handed the grid's `Beat` the playhead is in (`start` and `len`, ms) for the player to keep,
/// or waits for the boundary `At` (ms).
#[derive(Debug, PartialEq)]
pub enum Plan {
    Beat { start: f64, len: f64 },
    At(f64),
}

/// `call` while playing at `pos_ms` with `q`-beat quantize; `None` without a usable grid (the call then runs as is).
pub fn plan(beats: &[Beat], pos_ms: f64, mode: QuantizeMode, q: f64, call: Call) -> Option<Plan> {
    match (mode, call) {
        (QuantizeMode::NextBeat, _) => next_boundary(beats, pos_ms, q).map(Plan::At),
        (QuantizeMode::Immediate, _) => {
            window(beats, pos_ms, q).map(|(start, len)| Plan::Beat { start, len })
        }
    }
}

/// The `q`-beat of the grid the playhead `pos_ms` is in: its start and length (ms).
pub fn window(beats: &[Beat], pos_ms: f64, q: f64) -> Option<(f64, f64)> {
    let first = (beat_at(beats, pos_ms)? / q).floor() * q;
    let start = ms_at(beats, first)?;
    Some((start, ms_at(beats, first + q)? - start))
}

/// Time (ms) of the first `q`-beat boundary after `pos_ms`.
pub fn next_boundary(beats: &[Beat], pos_ms: f64, q: f64) -> Option<f64> {
    ms_at(beats, ((beat_at(beats, pos_ms)? / q).floor() + 1.0) * q)
}

/// How a jump keeps the beat the listener hears while it waits for the audio thread: nothing (a plain seek), the position heard
/// (ms) when the target was worked out (`Player::seek_in_phase`), or the grid's beat as `start` and `len` ms (`Player::seek_on_beat`).
#[derive(Clone, Copy)]
enum Keep {
    No,
    Heard(f64),
    Beat(f64, f64),
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
            None => self.run(call, Keep::No),
            Some(Plan::Beat { start, len }) => self.run(call, Keep::Beat(start, len)),
            Some(Plan::At(ms)) => {
                self.pending.set(Some((ms, pos, call)));
                self.say(tr!("quantize: jumping on the next beat…"));
            }
        }
    }

    /// Jumps to `call` (a loop is activated as stored), the beat kept as `keep` says.
    fn run(&self, call: Call, keep: Keep) {
        let ms = call.start();
        let seek = || {
            self.with_player(|p| match keep {
                Keep::No => p.seek(ms),
                Keep::Heard(h) => p.seek_in_phase(ms, h),
                // a loop wraps the phase round itself
                Keep::Beat(start, len) => p.seek_on_beat(ms, start, len, call.len()),
            });
        };
        match call {
            Call::Hot(_) => {
                seek();
                self.with_player(|p| p.play());
            }
            Call::Loop(s, e) => {
                let l = self.loaded.borrow_mut();
                self.apply_loop(l, Some((s, e)));
                seek();
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
            self.run(call, Keep::Heard(heard));
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
            Some(Plan::Beat {
                start: 1000.0,
                len: 500.0
            })
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
    fn a_hot_cue_is_handed_the_beat_under_the_playhead_not_a_landing() {
        let g = grid(0, 16); // beats every 500 ms
        let beat = |pos, q| plan(&g, pos, QuantizeMode::Immediate, q, Call::Hot(3000));
        // the player adds the phase of the frame it takes the jump at: only the grid goes to it, so a press just before a boundary
        // cannot land a beat late however long the jump takes
        assert_eq!(
            beat(1120.0, 1.0),
            Some(Plan::Beat {
                start: 1000.0,
                len: 500.0
            })
        );
        assert_eq!(
            beat(1499.0, 1.0),
            Some(Plan::Beat {
                start: 1000.0,
                len: 500.0
            })
        );
        assert_eq!(
            beat(1500.0, 1.0),
            Some(Plan::Beat {
                start: 1500.0,
                len: 500.0
            })
        );
        assert_eq!(
            beat(1370.0, 0.5),
            Some(Plan::Beat {
                start: 1250.0,
                len: 250.0
            })
        );
        assert_eq!(
            beat(2700.0, 2.0),
            Some(Plan::Beat {
                start: 2000.0,
                len: 1000.0
            })
        );
        // before the first beat and past the last one the grid goes on with its first / last interval
        let g = grid(100, 16);
        assert_eq!(window(&g, 20.0, 1.0), Some((-400.0, 500.0)));
        assert_eq!(window(&g, 9000.0, 1.0), Some((8600.0, 500.0)));
    }

    #[test]
    fn a_stored_loop_is_handed_the_beat_like_a_cue_and_wraps_its_landing_in_its_own_length() {
        let g = grid(0, 64);
        let call = Call::Loop(3000, 5000);
        // RELOOP / the list / a bank call the same loop again and again: it is activated as stored, only the landing goes by the phase
        let beat = |pos| plan(&g, pos, QuantizeMode::Immediate, 1.0, call);
        assert_eq!(
            beat(1370.0),
            Some(Plan::Beat {
                start: 1000.0,
                len: 500.0
            })
        );
        assert_eq!((call.len(), Call::Hot(3000).len()), (2000.0, 0.0));
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
