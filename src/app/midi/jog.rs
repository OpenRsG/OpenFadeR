//! Jog wheel model, one per deck, pure: ticks in, what to do to the deck out ([`Out`]); `mod.rs` applies
//! it through `Deck::scratch_*` / `Deck::bend` / a seek.
//!
//! - touch (vinyl mode on) → [`Out::Begin`]; release → [`Out::End`];
//! - turn of the scratch control while scratching → [`Out::Scratch`] (track seconds, platter model in the player);
//!   without touch (or vinyl off) the same ticks bend the pitch;
//! - ring / bend control → [`Out::Bend`] (pitch offset added per tick, decays when the wheel stops, see [`Jog::decay`]);
//! - shift + jog → [`Out::Search`] (milliseconds to move the playhead);
//! - JOG CUTTER (needs VINYL on, one deck at a time: [`cutters`]) → [`Out::Cut`]. A controller's JOG CUTTER may play preset
//!   scratch patterns by the wheel's area; no message carries that area here, so this gates the channel instead.
//!
//! Calibration (the real wheel has not been turned during development): a platter turns once
//! per [`REV_S`] (33⅓ rpm) in `OPENFADER_JOG_TICKS` ticks (default [`TICKS_PER_REV`]); a bend tick adds
//! [`BEND_GAIN`]; search moves [`SEARCH_MS`] per tick.

use crate::app::deck::DECKS;

/// Seconds of track per platter revolution (33⅓ rpm).
pub const REV_S: f64 = 1.8;
const TICKS_PER_REV: f64 = 720.0;
const BEND_GAIN: f32 = 0.01;
/// Pitch bend ceiling (rate offset); the player allows more, a wheel should not.
const BEND_MAX: f32 = 0.1;
/// Bend multiplier per decay step (the 5 ms input timer): τ ≈ 50 ms.
const DECAY: f32 = 0.9;
const BEND_EPS: f32 = 0.002;
const SEARCH_MS: f64 = 50.0;
/// Cutter: a platter that stops for this many 5 ms steps cuts the channel (30 ms: fast enough for
/// 1/16-note chops at 120 bpm, slower than the gap between ticks of a turning wheel).
const HOLD: u32 = 6;
/// The JOG CUTTER switches of every deck after deck `d` is asked for `want`: VINYL mode has to be on first, and
/// jog cutter mode cannot run on several decks at the same time, so the previous deck's goes off.
/// A press without VINYL on leaves the deck's cutter off.
pub fn cutters(cutter: [bool; DECKS], vinyl: [bool; DECKS], d: usize, want: bool) -> [bool; DECKS] {
    let mut c = cutter;
    if want && vinyl[d] {
        c = [false; DECKS];
    }
    c[d] = want && vinyl[d];
    c
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Out {
    Begin,
    End,
    Scratch(f64),
    Bend(f32),
    Search(f64),
    /// JOG CUTTER gate: `true` = channel cut.
    Cut(bool),
}

pub struct Jog {
    touched: bool,
    scratching: bool,
    bend: f32,
    s_per_tick: f64,
    cutter: bool,
    /// 5 ms steps left before a touched, stopped platter cuts the channel again.
    hold: u32,
    cut: bool,
}

impl Jog {
    pub fn new(ticks_per_rev: Option<f64>) -> Jog {
        let t = ticks_per_rev.filter(|t| *t >= 1.0).unwrap_or(TICKS_PER_REV);
        Jog {
            touched: false,
            scratching: false,
            bend: 0.0,
            s_per_tick: REV_S / t,
            cutter: false,
            hold: 0,
            cut: false,
        }
    }

    /// Platter touched / released. Scratching needs vinyl mode; a release always ends a scratch.
    pub fn touch(&mut self, on: bool, vinyl: bool) -> Option<Out> {
        self.touched = on;
        if self.cutter {
            self.hold = 0;
            return self.gate();
        }
        match (on && vinyl, self.scratching) {
            (true, false) => {
                self.scratching = true;
                Some(Out::Begin)
            }
            (false, true) => {
                self.scratching = false;
                Some(Out::End)
            }
            _ => None,
        }
    }

    /// JOG CUTTER on/off. While on and the platter is held, the channel is cut except while it turns (a
    /// crossfader cut by hand); touch never scratches. On ends a scratch; off opens the channel.
    pub fn cutter(&mut self, on: bool) -> Vec<Out> {
        self.cutter = on;
        self.hold = 0;
        let end = if on && self.scratching {
            self.scratching = false;
            Some(Out::End)
        } else {
            None
        };
        end.into_iter().chain(self.gate()).collect()
    }

    /// JOG CUTTER rotation (B028): opens the channel for [`HOLD`] steps; motion without touch changes nothing.
    pub fn cut(&mut self, ticks: i32) -> Option<Out> {
        if ticks != 0 && self.cutter && self.touched {
            self.hold = HOLD;
        }
        self.gate()
    }

    /// Emits the gate state when it changed.
    fn gate(&mut self) -> Option<Out> {
        let cut = self.cutter && self.touched && self.hold == 0;
        (cut != std::mem::replace(&mut self.cut, cut)).then_some(Out::Cut(cut))
    }

    /// Vinyl mode switched while the platter may be held: off ends the scratch, on starts one.
    pub fn vinyl(&mut self, on: bool) -> Option<Out> {
        self.touch(self.touched, on)
    }

    /// Scratch control (the platter top): a scratch while touched, a bend otherwise.
    pub fn turn(&mut self, ticks: i32) -> Out {
        if self.scratching {
            Out::Scratch(ticks as f64 * self.s_per_tick)
        } else {
            self.nudge(ticks)
        }
    }

    /// Ring / non-touch control: ignored while the platter is held (the scratch owns the playhead).
    pub fn bend(&mut self, ticks: i32) -> Option<Out> {
        (!self.scratching).then(|| self.nudge(ticks))
    }

    fn nudge(&mut self, ticks: i32) -> Out {
        self.bend = (self.bend + ticks as f32 * BEND_GAIN).clamp(-BEND_MAX, BEND_MAX);
        Out::Bend(self.bend)
    }

    pub fn search(&self, ticks: i32) -> Out {
        Out::Search(ticks as f64 * SEARCH_MS)
    }

    /// One 5 ms step: the bend fades out; `Some` while there is something to change (the last one is 0).
    /// The cutter's hold also runs here, see [`Jog::decay_cut`].
    pub fn decay(&mut self) -> Option<Out> {
        if self.bend == 0.0 {
            return None;
        }
        self.bend = if self.bend.abs() < BEND_EPS {
            0.0
        } else {
            self.bend * DECAY
        };
        Some(Out::Bend(self.bend))
    }

    /// One 5 ms step of the cutter hold: closes the gate when the platter stops.
    pub fn decay_cut(&mut self) -> Option<Out> {
        self.hold = self.hold.saturating_sub(1);
        self.gate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_scratches_only_in_vinyl_mode() {
        let mut j = Jog::new(None);
        assert_eq!(j.touch(true, false), None);
        assert!(
            matches!(j.turn(3), Out::Bend(_)),
            "no scratch without vinyl"
        );
        assert_eq!(j.touch(false, false), None);
        assert_eq!(j.touch(true, true), Some(Out::Begin));
        assert_eq!(
            j.touch(true, true),
            None,
            "a repeated touch does not restart the scratch"
        );
        assert_eq!(j.bend(5), None, "bend is ignored while scratching");
        assert_eq!(j.touch(false, true), Some(Out::End));
        assert_eq!(j.touch(false, true), None);
    }

    #[test]
    fn scratch_ticks_become_track_seconds() {
        let mut j = Jog::new(Some(360.0));
        j.touch(true, true);
        let Out::Scratch(s) = j.turn(360) else {
            panic!()
        };
        assert!(
            (s - 1.8).abs() < 1e-9,
            "one revolution = 1.8 s of track: {s}"
        );
        let Out::Scratch(s) = j.turn(-90) else {
            panic!()
        };
        assert!((s + 0.45).abs() < 1e-9);
    }

    #[test]
    fn vinyl_toggle_mid_touch_ends_and_starts_the_scratch() {
        let mut j = Jog::new(None);
        j.touch(true, true);
        assert_eq!(j.vinyl(false), Some(Out::End));
        assert_eq!(j.vinyl(false), None);
        assert_eq!(
            j.vinyl(true),
            Some(Out::Begin),
            "still held: scratching resumes"
        );
    }

    #[test]
    fn bend_is_clamped_and_decays_to_zero() {
        let mut j = Jog::new(None);
        assert_eq!(j.bend(1_000), Some(Out::Bend(BEND_MAX)));
        assert_eq!(j.bend(-1_000_000), Some(Out::Bend(-BEND_MAX)));
        j.bend(1_000_000);
        let mut steps = 0;
        let mut last = Out::Bend(BEND_MAX);
        while let Some(o) = j.decay() {
            last = o;
            steps += 1;
            assert!(steps < 200, "must settle");
        }
        assert_eq!(last, Out::Bend(0.0), "the final step releases the bend");
        assert_eq!(j.decay(), None);
    }

    fn steps(j: &mut Jog, n: u32) -> Vec<Out> {
        (0..n).filter_map(|_| j.decay_cut()).collect()
    }

    #[test]
    fn touch_cuts_turning_opens_and_stopping_cuts_again() {
        let mut j = Jog::new(None);
        assert_eq!(j.cutter(true), vec![], "not touched: nothing is cut");
        assert_eq!(
            j.touch(true, true),
            Some(Out::Cut(true)),
            "a held platter cuts and never scratches"
        );
        assert_eq!(j.cut(3), Some(Out::Cut(false)));
        assert_eq!(j.cut(-1), None, "still open");
        assert!(
            steps(&mut j, HOLD - 1).is_empty(),
            "gate stays open while ticks are fresh"
        );
        assert_eq!(
            steps(&mut j, 1),
            vec![Out::Cut(true)],
            "closes when the wheel has stopped for HOLD steps"
        );
        assert_eq!(j.touch(false, true), Some(Out::Cut(false)), "release opens");
    }

    #[test]
    fn motion_without_touch_and_mode_off_never_cut() {
        let mut j = Jog::new(None);
        j.cutter(true);
        assert_eq!(j.cut(5), None);
        j.touch(true, true);
        assert_eq!(
            j.cutter(false),
            vec![Out::Cut(false)],
            "leaving the mode opens the channel"
        );
        assert_eq!(j.cut(5), None);
    }

    #[test]
    fn entering_the_mode_mid_scratch_ends_it() {
        let mut j = Jog::new(None);
        j.touch(true, true);
        assert_eq!(j.cutter(true), vec![Out::End, Out::Cut(true)]);
    }

    #[test]
    fn jog_cutter_needs_vinyl_and_runs_on_one_deck() {
        let (off, on) = ([false; DECKS], [true; DECKS]);
        assert_eq!(cutters(off, on, 1, true), [false, true, false, false]);
        assert_eq!(
            cutters([false, true, false, false], on, 3, true),
            [false, false, false, true],
            "the other deck's cutter goes off"
        );
        assert_eq!(
            cutters(off, [true, false, true, true], 1, true),
            off,
            "VINYL off: the press does nothing"
        );
        assert_eq!(
            cutters(
                [false, true, false, false],
                [true, false, true, true],
                1,
                true
            ),
            off,
            "VINYL switched off takes the cutter with it"
        );
        assert_eq!(
            cutters([true, true, false, false], on, 0, false),
            [false, true, false, false]
        );
    }
}
