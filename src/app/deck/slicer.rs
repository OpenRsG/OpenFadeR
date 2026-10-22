//! SLICER and SLICE LOOP pad modes. A range of 1, 2, 4 or 8 bars (LENGTH) is cut into 8 slices, one per pad.
//! Holding a pad plays its slice as a loop (the ROLL, 1/8 … 1/64 of the LENGTH from the slice start, so 1/8 is the whole slice) while the track
//! goes on in the background; releasing it returns to where the track would have been. The range follows the playhead: SLICER moves on to
//! the next range when the playhead leaves it, SLICE LOOP keeps the range as an ordinary loop. ◀ ▶ move the range by its LENGTH.
//!
//! Ranges sit on the beat grid, anchored at the beat nearest to the playhead when the mode was chosen . The pad lit is the slice under the playhead (or the held one). The background position of a held pad is tracked on
//! the wall clock at the deck's tempo (the `slip` shadow ignores loops, so it cannot do the SLICE LOOP wrap).
//! Pure range / slice arithmetic first; the pad, option and tick code is below.

use super::{Deck, Loaded, beat_at, ms_at, pads::PadMode, prefs};
use crate::app::prefs::controller::{SLICER_BARS, SLICER_ROLL};
use std::{cell::Cell, time::Instant};

/// Slices (= pads) per range.
pub const SLICES: usize = 8;

/// Start (beats) of the slicing range for a playhead at `pos` beats: the `len`-beat range of the grid anchored at `origin` that holds it,
/// moved on by `shift` ranges.
pub fn range_start(origin: f64, len: f64, pos: f64, shift: i32) -> f64 {
    origin + ((pos - origin) / len).floor() * len + shift as f64 * len
}

/// (start, length) in beats of the roll of slice `pad` in the range starting at `start`: the roll starts with the slice and is `1/roll` of the range long.
pub fn roll_beats(start: f64, len: f64, pad: usize, roll: u8) -> (f64, f64) {
    (start + len / SLICES as f64 * pad as f64, len / roll as f64)
}

/// The slice of the range `start..start + len` (beats) that `pos` is in.
pub fn slice_at(start: f64, len: f64, pos: f64) -> Option<usize> {
    let i = ((pos - start) / len * SLICES as f64).floor();
    (0.0..SLICES as f64).contains(&i).then_some(i as usize)
}

/// Where a track that was at `pos` ms when it was left running at `rate` is `elapsed_ms` later; inside `wrap` (a loop's ms range) it goes round.
pub fn background(pos: f64, elapsed_ms: f64, rate: f64, wrap: Option<(f64, f64)>) -> f64 {
    let at = pos + elapsed_ms * rate;
    match wrap {
        Some((s, e)) if e > s && at >= e => s + (at - s).rem_euclid(e - s),
        _ => at,
    }
}

/// The entry after `cur` in `list`, wrapping; the first when `cur` is not in it.
fn next_of(list: &[u8], cur: u8) -> u8 {
    let at = list.iter().position(|&x| x == cur).unwrap_or(0);
    list[(at + 1) % list.len()]
}

#[derive(Clone, Copy)]
struct Held {
    pad: usize,
    /// Playhead and the clock when the first pad went down; whether the deck played.
    pos_ms: f64,
    at: Instant,
    playing: bool,
}

#[derive(Default)]
pub(super) struct State {
    /// LENGTH bars / ROLL denominator; 0 = not chosen yet (PREFS > CONTROLLER > Sampler > SLICER).
    bars: Cell<u8>,
    roll: Cell<u8>,
    /// The beat the ranges are anchored at; ranges moved by ◀ ▶ (SLICER).
    origin: Cell<f64>,
    shift: Cell<i32>,
    /// SLICE LOOP: start (beats) of the range being looped.
    looped: Cell<Option<f64>>,
    held: Cell<Option<Held>>,
    /// The pad lit as of the last tick.
    lit: Cell<Option<usize>>,
}

impl Deck {
    pub(super) fn slicer_bars(&self) -> u8 {
        match self.slicer.bars.get() {
            0 => prefs::with(|p| p.controller.slicer_bars()),
            b => b,
        }
    }

    fn slicer_roll(&self) -> u8 {
        match self.slicer.roll.get() {
            0 => prefs::with(|p| p.controller.slicer_roll()),
            r => r,
        }
    }

    pub(super) fn slicer_text(&self) -> (String, String) {
        let b = self.slicer_bars();
        (
            tr_n!("{n} BAR", "{n} BARS", b),
            format!("1/{}", self.slicer_roll()),
        )
    }

    /// Range length, beats.
    fn slicer_len(&self) -> f64 {
        self.slicer_bars() as f64 * 4.0
    }

    /// Start (beats) of the range the pads address now.
    fn slicer_range(&self, beats: f64) -> f64 {
        self.slicer.looped.get().unwrap_or_else(|| {
            range_start(
                self.slicer.origin.get(),
                self.slicer_len(),
                beats,
                self.slicer.shift.get(),
            )
        })
    }

    /// `(start, end)` ms of the range starting at beat `b`.
    fn slicer_ms(l: &Loaded, b: f64, len: f64) -> Option<(u32, u32)> {
        Some((
            ms_at(&l.analysis.beats, b)?.round().max(0.0) as u32,
            ms_at(&l.analysis.beats, b + len)?.round() as u32,
        ))
    }

    /// The mode changes from `from` to `to`: a slicer mode entered from outside anchors its ranges at the playhead's beat; SLICE LOOP loops the
    /// range holding the playhead; leaving SLICE LOOP ends that loop.
    pub(super) fn slicer_switch(&self, from: PadMode, to: PadMode) {
        if from == to {
            return; // a controller's pad press re-selects its mode first
        }
        self.slicer_release();
        let slicer = PadMode::is_slicer;
        let s = &self.slicer;
        let l = self.loaded.borrow_mut();
        if slicer(to) && !slicer(from) {
            s.shift.set(0);
            s.origin
                .set(beat_at(&l.analysis.beats, self.clock(&l).0).map_or(0.0, f64::floor));
        }
        if to == PadMode::SlicerLoop && l.ready {
            let pos = beat_at(&l.analysis.beats, self.clock(&l).0);
            let len = self.slicer_len();
            let range = pos
                .map(|p| self.slicer_range(p))
                .and_then(|b| Self::slicer_ms(&l, b, len).map(|ms| (b, ms)));
            match range {
                Some((b, ms)) => {
                    s.looped.set(Some(b));
                    return self.apply_loop(l, Some(ms));
                }
                None => {
                    drop(l);
                    return self.say(tr!("the slicer needs a track with a beat grid"));
                }
            }
        }
        if s.looped.take().is_some() {
            self.apply_loop(l, None);
        }
    }

    /// Pad `pad` (0..8) down: its slice rolls while the pad is held.
    pub(super) fn slicer_pad(&self, pad: usize) {
        let l = self.loaded.borrow_mut();
        let (pos, playing) = self.clock(&l);
        let len = self.slicer_len();
        let Some(beat) = beat_at(&l.analysis.beats, pos) else {
            drop(l);
            return self.say(tr!("the slicer needs a track with a beat grid"));
        };
        let (start, roll) = roll_beats(self.slicer_range(beat), len, pad, self.slicer_roll());
        let Some((s, e)) = Self::slicer_ms(&l, start, roll).filter(|(s, e)| e > s) else {
            return;
        };
        if self.slicer.held.get().is_none() {
            self.slicer.held.set(Some(Held {
                pad,
                pos_ms: pos,
                at: Instant::now(),
                playing,
            }));
        }
        self.apply_loop(l, Some((s, e)));
        self.with_player(|p| {
            p.seek(s);
            if !playing {
                p.play();
            }
        });
    }

    /// A pad was released: the loop goes (SLICE LOOP: back to the range), the track returns to where it would have been, a paused deck pauses.
    pub(super) fn slicer_release(&self) {
        let Some(h) = self.slicer.held.take() else {
            return;
        };
        let l = self.loaded.borrow_mut();
        let range = self
            .slicer
            .looped
            .get()
            .and_then(|b| Self::slicer_ms(&l, b, self.slicer_len()));
        let at = if h.playing {
            background(
                h.pos_ms,
                h.at.elapsed().as_secs_f64() * 1000.0,
                self.rate.get(),
                range.map(|(s, e)| (s as f64, e as f64)),
            )
        } else {
            h.pos_ms
        };
        self.apply_loop(l, range);
        self.with_player(|p| {
            if !h.playing {
                p.pause();
            }
            p.seek(at.round() as u32);
        });
    }

    /// ◀ (−1) / ▶ (+1): the range moves by its LENGTH; SLICE LOOP loops the new range from its start.
    pub(super) fn slicer_shift(&self, dir: i32) {
        self.slicer_release();
        let l = self.loaded.borrow_mut();
        let len = self.slicer_len();
        if let Some(b) = self.slicer.looped.get() {
            let b = b + dir as f64 * len;
            if let Some(ms) =
                Self::slicer_ms(&l, b, len).filter(|&(s, _)| b >= 0.0 && s < l.duration_ms)
            {
                self.slicer.looped.set(Some(b));
                self.apply_loop(l, Some(ms));
                self.with_player(|p| p.seek(ms.0));
            }
        } else {
            drop(l);
            self.slicer.shift.set(self.slicer.shift.get() + dir);
        }
        self.refresh_pads();
    }

    /// LENGTH: the next of 1, 2, 4, 8 bars (a looped range keeps its start).
    pub(super) fn slicer_cycle_length(&self) {
        self.slicer_release();
        self.slicer
            .bars
            .set(next_of(&SLICER_BARS, self.slicer_bars()));
        if let Some(b) = self.slicer.looped.get() {
            let l = self.loaded.borrow_mut();
            let ms = Self::slicer_ms(&l, b, self.slicer_len());
            self.apply_loop(l, ms);
        }
        self.refresh_pads();
    }

    /// ROLL: the next of 1/8, 1/16, 1/32, 1/64.
    pub(super) fn slicer_cycle_roll(&self) {
        self.slicer
            .roll
            .set(next_of(&SLICER_ROLL, self.slicer_roll()));
        self.refresh_pads();
    }

    /// The slice lit: the held pad, else the one under the playhead.
    pub(super) fn slicer_lit(&self, l: &Loaded) -> Option<usize> {
        if let Some(h) = self.slicer.held.get() {
            return Some(h.pad);
        }
        let beat = beat_at(&l.analysis.beats, self.clock(l).0)?;
        slice_at(self.slicer_range(beat), self.slicer_len(), beat)
    }

    /// Every tick in a slicer mode: lights the pad under the playhead.
    pub(super) fn slicer_tick(&self) {
        if !self.pad_mode().is_slicer() {
            return;
        }
        let lit = self.slicer_lit(&self.loaded.borrow());
        if self.slicer.lit.replace(lit) != lit {
            self.refresh_pads();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_range_holding_the_playhead_follows_it_and_shifts_by_whole_ranges() {
        // anchored at beat 4, 1 bar = 4 beats
        assert_eq!(range_start(4.0, 4.0, 4.0, 0), 4.0);
        assert_eq!(range_start(4.0, 4.0, 7.99, 0), 4.0);
        assert_eq!(
            range_start(4.0, 4.0, 8.0, 0),
            8.0,
            "the playhead reached the end: the next range"
        );
        assert_eq!(range_start(4.0, 4.0, 9.3, 1), 12.0, "▶ moves one range on");
        assert_eq!(range_start(4.0, 8.0, 3.0, 0), -4.0, "before the anchor");
    }

    #[test]
    fn eight_slices_roll_from_their_start_over_a_fraction_of_the_range() {
        // 2 bars = 8 beats: a slice is 1 beat long
        let r = |pad, roll| roll_beats(16.0, 8.0, pad, roll);
        assert_eq!(r(0, 8), (16.0, 1.0));
        assert_eq!(r(5, 8), (21.0, 1.0), "1/8 of the range = the whole slice");
        assert_eq!(r(7, 16), (23.0, 0.5));
        assert_eq!(r(2, 64), (18.0, 0.125));
        assert_eq!(slice_at(16.0, 8.0, 21.5), Some(5));
        assert_eq!(slice_at(16.0, 8.0, 24.0), None);
        assert_eq!(slice_at(16.0, 8.0, 15.9), None);
    }

    #[test]
    fn slices_land_on_the_beat_grid_in_ms() {
        use openfader::analysis::Beat;
        let grid: Vec<Beat> = (0..64)
            .map(|i| Beat {
                time_ms: 500 * i,
                bpm_x100: 12000,
                bar_pos: (i % 4 + 1) as u16,
            })
            .collect();
        // 1 bar at 120 BPM from beat 4 (2000 ms): slice 3 = beat 5.5 = 2750 ms, a 1/32 roll = 1/8 beat = 62.5 ms
        let (b, len) = roll_beats(4.0, 4.0, 3, 32);
        assert_eq!(
            (ms_at(&grid, b).unwrap(), ms_at(&grid, b + len).unwrap()),
            (2750.0, 2812.5)
        );
    }

    #[test]
    fn the_track_goes_on_in_the_background_and_wraps_in_a_slice_loop() {
        assert_eq!(background(1000.0, 400.0, 1.0, None), 1400.0);
        assert_eq!(background(1000.0, 400.0, 1.25, None), 1500.0);
        // looped range 1000..2000: 1900 + 250 wraps to 1150
        assert_eq!(
            background(1900.0, 250.0, 1.0, Some((1000.0, 2000.0))),
            1150.0
        );
        assert_eq!(
            background(1100.0, 250.0, 1.0, Some((1000.0, 2000.0))),
            1350.0
        );
    }
}
