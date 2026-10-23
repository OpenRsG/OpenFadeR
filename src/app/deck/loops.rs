//! Loops: beat-grid auto-loops (1/2/4/8 beats), manual LOOP IN / LOOP OUT / RELOOP, and halve /
//! double of the active loop. The active loop lives in `Loaded.loop_` and the player; everything
//! the buttons show (`loop-beats`, `loop-active`) is derived from it. Loops are session only.
//!
//! Semantics: LOOP OUT closes the loop at the playhead and a playing deck goes on inside it at the same beat phase, as if it had
//! wrapped there ([`resumes`]; a paused one is put on the in-point); RELOOP exits an active loop, or re-enters the last one from its start.

use super::{
    Deck, Loaded, beat_at, beats_between, fmt_ms, is_loop_len, ms_at, prefs, quantize::Call,
};
use openfader::analysis::Beat;
use slint::{ModelRc, VecModel};
use std::cell::RefMut;

/// Shortest loop (ms); also the smallest a halve may produce.
const MIN_MS: u32 = 10;

/// Loop of `n` beats (may be fractional) that holds the playhead: it starts on the last point at or before `pos_ms` of a grid
/// of `step` beats (1 = the beat itself; with Q on the SNAP length), made as fine as the loop when the loop is shorter, so
/// `pos_ms` is always inside. A loop behind the playhead would never play (the player only wraps on reaching the end).
/// Before the first beat: the first beat. Past the last beat the grid extrapolates with the last interval. `None` for a
/// grid with fewer than 2 beats or zero tempo. `beats` must be time-ordered.
pub fn auto_loop(beats: &[Beat], pos_ms: f64, n: f64, step: f64) -> Option<(u32, u32)> {
    let step = step.min(n);
    // A hair of slack so a playhead standing on a point (float error) keeps that point.
    let b = ((beat_at(beats, pos_ms)? / step + 1e-6).floor() * step).max(0.0);
    Some((
        ms_at(beats, b)?.round() as u32,
        ms_at(beats, b + n)?.round() as u32,
    ))
}

/// The active loop `(s, _)` made `n` beats long from the same start (the grid gives the length where `s` sits).
fn resized(beats: &[Beat], (s, _): (u32, u32), n: f64) -> Option<(u32, u32)> {
    let b = beat_at(beats, s as f64)?;
    Some((s, ms_at(beats, b + n)?.round() as u32)).filter(|&(s, e)| e >= s + MIN_MS)
}

/// The loop `(s, e)` `factor` times as long from the same start. A loop of a whole number of 1/8 beats takes its new end from the
/// grid, so halving and doubling go back and forth exactly whatever the ms rounding of its ends; any other loop is scaled in ms.
/// `None` when that would be shorter than [`MIN_MS`].
fn scaled(beats: &[Beat], (s, e): (u32, u32), factor: f64) -> Option<(u32, u32)> {
    let on_grid = beats_between(beats, (s, e)).and_then(|n| {
        let whole = (n * 8.0).round() / 8.0;
        (whole > 0.0 && is_loop_len(n, whole)).then_some(whole)
    });
    match on_grid {
        Some(n) => resized(beats, (s, e), n * factor),
        None => {
            let len = ((e - s) as f64 * factor) as u32;
            (len >= MIN_MS).then_some((s, s + len))
        }
    }
}

/// First half of a loop; `None` when that would be shorter than [`MIN_MS`].
fn halve(beats: &[Beat], lp: (u32, u32)) -> Option<(u32, u32)> {
    scaled(beats, lp, 0.5)
}

/// Twice the length from the same start; `None` past the end of the track (`duration_ms` 0 = unknown).
fn double(beats: &[Beat], lp: (u32, u32), duration_ms: u32) -> Option<(u32, u32)> {
    scaled(beats, lp, 2.0).filter(|&lp| fits(lp, duration_ms))
}

/// A loop ending past the track never wraps: the deck would run off the end (`duration_ms` 0 = unknown).
fn fits((_, e): (u32, u32), duration_ms: u32) -> bool {
    duration_ms == 0 || e <= duration_ms
}

/// What the player loops for `lp`. Ends in whole ms make a loop run up to a ms per pass off the track's tempo, so a loop of a
/// whole number of 1/8 beats gets the length the grid's tempo gives it from its start, when that is within the rounding of its ends.
fn exact_loop(beats: &[Beat], (s, e): (u32, u32)) -> (f64, f64) {
    let n = beats_between(beats, (s, e)).map(|n| (n * 8.0).round() / 8.0);
    let tempo = beat_at(beats, s as f64)
        .and_then(|b| beats.get(b.floor().max(0.0) as usize).or(beats.last()))
        .map(|b| b.bpm_x100)
        .filter(|&t| t > 0);
    let len = n.zip(tempo).map(|(n, t)| n * 6_000_000.0 / t as f64);
    match len.filter(|&len| len > 0.0 && (len - (e - s) as f64).abs() <= 1.0) {
        Some(len) => (s as f64, s as f64 + len),
        None => (s as f64, e as f64),
    }
}

/// Where a playing deck at `pos_ms` goes on when a loop is closed at its end `e` and the playhead is already past it: the same
/// phase of the loop, as if it had wrapped there. `None` while the playhead is before the end (it plays on and wraps).
fn resumes(pos_ms: f64, (s, e): (u32, u32)) -> Option<u32> {
    (pos_ms >= e as f64).then(|| (s as f64 + (pos_ms - e as f64) % (e - s) as f64).round() as u32)
}

/// Loop from `start` to `at`; `None` without a start or when `at` is not after it.
fn close(start: Option<u32>, at: u32) -> Option<(u32, u32)> {
    start.filter(|&s| at >= s + MIN_MS).map(|s| (s, at))
}

/// Whole number of beats the loop spans on the grid (the 1/2/4/8 pad lights up), else 0.
pub(super) fn beats_len(beats: &[Beat], lp: (u32, u32)) -> i32 {
    match beats_between(beats, lp) {
        Some(n) if n >= 1.0 && (n - n.round()).abs() < 0.02 => n.round() as i32,
        _ => 0,
    }
}

impl Deck {
    /// Makes `new` (or no loop) the active loop and mirrors it into the player and the view.
    pub(super) fn apply_loop(&self, mut l: RefMut<Loaded>, new: Option<(u32, u32)>) {
        l.loop_ = new;
        if new.is_some() {
            l.last_loop = new;
        }
        let beats = new.map_or(0, |lp| beats_len(&l.analysis.beats, lp));
        let played = new.map(|lp| exact_loop(&l.analysis.beats, lp));
        let pads = self.pad_views(&l);
        drop(l);
        self.with_player(|p| p.set_loop(played));
        self.show(|v| {
            v.loop_beats = beats;
            v.pads = ModelRc::new(VecModel::from(pads));
            v.loop_active = new.is_some();
            v.status = new.map_or(tr!("loop off").into(), |(s, e)| {
                tr!("loop {} – {} (session only)", fmt_ms(s), fmt_ms(e)).into()
            });
        });
    }

    /// Auto-loop of a whole number of beats (the 1/2/4/8 buttons and L key); 0 = the length of Preferences > Auto Beat Loop.
    pub fn auto_loop(&self, beats: u32) {
        let n = if beats == 0 {
            prefs::with(|p| p.controller.loop_beats())
        } else {
            beats
        };
        self.beat_loop(n as f64);
    }

    /// Loop of `beats` (1/8 … 16 on the pads) holding the playhead: from the beat at/before it, with Q on from the SNAP point
    /// at/before it ([`auto_loop`]); the same length again exits. With a loop already active another length
    /// resizes it from its start (like halving, so the playhead never drops out of a loop it is in). The clock
    /// gives the heard playhead, which lags what the player already wrote; the player wraps that overshoot into the new
    /// loop (`audio::shrunk`).
    pub fn beat_loop(&self, beats: f64) {
        if !self.ready() {
            return;
        }
        let l = self.loaded.borrow_mut();
        let (pos, _) = self.clock(&l);
        let same = l
            .loop_
            .and_then(|lp| beats_between(&l.analysis.beats, lp))
            .is_some_and(|len| is_loop_len(len, beats));
        let new = if same {
            None
        } else if let Some(lp) = l.loop_ {
            resized(&l.analysis.beats, lp, beats)
        } else {
            let step = if self.quantize.get() {
                prefs::with(|p| p.controller.snap_length.beats())
            } else {
                1.0
            };
            auto_loop(&l.analysis.beats, pos, beats, step)
        };
        if new.is_some_and(|lp| !fits(lp, l.duration_ms)) {
            return self.say(tr!("no room for that loop"));
        }
        self.apply_loop(l, new);
    }

    /// Sets the in-point at the (quantized) playhead; with a loop active, moves its start.
    pub fn loop_in(&self) {
        if !self.ready() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let at = self.mark(&l);
        let cur = l.loop_;
        match cur {
            Some((_, e)) => match close(Some(at), e) {
                Some(lp) => self.apply_loop(l, Some(lp)),
                None => self.say(tr!("loop in must come before the loop end")),
            },
            None => {
                l.loop_in = Some(at);
                self.say(tr!("loop in at {}", fmt_ms(at)));
            }
        }
    }

    /// Closes the loop at the playhead (the loop-in point, or the active loop's start, is its start). A playing deck goes on in the
    /// loop at the same beat phase ([`resumes`]), or, with the end still ahead of the playhead (Q rounded it up), plays on and wraps
    /// there: either way the loop stays on the beat grid. A paused one is put on the in-point.
    pub fn loop_out(&self) {
        if !self.ready() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let at = self.mark(&l);
        let (pos, playing) = self.clock(&l);
        match close(l.loop_.map(|(s, _)| s).or(l.loop_in), at) {
            Some(lp) => {
                l.loop_in = None;
                self.apply_loop(l, Some(lp));
                self.with_player(|p| match (playing, resumes(pos, lp)) {
                    (false, _) => p.seek(lp.0),
                    (true, Some(ms)) => p.seek_in_phase(ms, pos),
                    (true, None) => {}
                });
            }
            None => self.say(tr!("loop out must come after loop in")),
        }
    }

    /// Exits the active loop, or re-enters the last one from its start.
    pub fn reloop(&self) {
        if !self.ready() {
            return;
        }
        let l = self.loaded.borrow_mut();
        let (pos, playing) = self.clock(&l);
        let ended = !playing && super::at_end(pos, l.duration_ms);
        match (l.loop_, l.last_loop) {
            (Some(_), _) => self.apply_loop(l, None),
            (None, Some(lp)) if playing => {
                drop(l);
                self.call(Call::Loop(lp.0, lp.1)); // kept on the beat with Q on (QUANTIZE LOOP)
            }
            (None, Some(lp)) => {
                self.apply_loop(l, Some(lp));
                self.with_player(|p| {
                    p.seek(lp.0);
                    if ended {
                        p.play(); // track finished: RELOOP resumes at the loop start
                    }
                });
            }
            (None, None) => self.say(tr!("no loop to re-enter")),
        }
    }

    pub fn loop_halve(&self) {
        self.resize_loop(
            |beats, lp, _| halve(beats, lp),
            tr!("loop is at its shortest"),
        );
    }

    pub fn loop_double(&self) {
        self.resize_loop(double, tr!("no room to double the loop"));
    }

    /// Replaces the active loop by `f(grid, loop, track duration)`. A playing deck past the new end is wrapped into it by the audio
    /// thread (`audio::shrunk`, which sees the real playhead, not the heard one this clock gives); a paused one goes to its start.
    fn resize_loop(
        &self,
        f: impl FnOnce(&[Beat], (u32, u32), u32) -> Option<(u32, u32)>,
        refused: &str,
    ) {
        if !self.ready() {
            return;
        }
        let l = self.loaded.borrow_mut();
        let Some(cur) = l.loop_ else {
            return self.say(tr!("no active loop"));
        };
        let (pos, playing) = self.clock(&l);
        match f(&l.analysis.beats, cur, l.duration_ms) {
            Some(lp) => {
                self.apply_loop(l, Some(lp));
                if !playing && pos >= lp.1 as f64 {
                    self.with_player(|p| p.seek(lp.0));
                }
            }
            None => self.say(refused),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::deck::tests::grid;

    #[test]
    fn auto_loop_starts_at_or_before_the_playhead() {
        let g = grid(1000, 4); // beats at 1000, 1500, 2000, 2500
        assert_eq!(auto_loop(&g, 1700.0, 1.0, 1.0), Some((1500, 2000)));
        assert_eq!(auto_loop(&g, 1500.0, 1.0, 1.0), Some((1500, 2000)));
        assert_eq!(
            auto_loop(&g, 0.0, 2.0, 1.0),
            Some((1000, 2000)),
            "before the grid: its first beat"
        );
        // past the last beat the grid goes on, so the loop still holds the playhead
        assert_eq!(auto_loop(&g, 9999.0, 2.0, 1.0), Some((9500, 10500)));
        assert_eq!(auto_loop(&g, 1000.0, 8.0, 1.0), Some((1000, 5000)));
    }

    #[test]
    fn the_playhead_is_inside_every_auto_loop() {
        let g = grid(1000, 16);
        // 0.49 beat past a beat (the reported case): a 1/8 loop is the eighth holding it, not the beat behind it
        assert_eq!(auto_loop(&g, 1245.0, 0.125, 1.0), Some((1188, 1250)));
        // Q on (SNAP 1 beat): the beat at/before, never the nearer one ahead or a half beat behind
        assert_eq!(auto_loop(&g, 1240.0, 4.0, 1.0), Some((1000, 3000)));
        assert_eq!(auto_loop(&g, 1260.0, 4.0, 1.0), Some((1000, 3000)));
        assert_eq!(auto_loop(&g, 1260.0, 1.0, 0.25), Some((1250, 1750)));
        for pos in (1000..4000).step_by(7) {
            for n in [0.125, 0.25, 0.5, 1.0, 2.0, 4.0] {
                for step in [0.25, 0.5, 1.0] {
                    let (s, e) = auto_loop(&g, pos as f64, n, step).unwrap();
                    assert!(
                        s as i64 <= pos as i64 && pos < e,
                        "{pos} ms, {n} beats, SNAP {step}: ({s}, {e})"
                    );
                }
            }
        }
    }

    #[test]
    fn auto_loop_lengths_and_empty() {
        let g = grid(1000, 4);
        let ends: Vec<u32> = [1.0, 2.0, 4.0, 8.0]
            .iter()
            .map(|&n| auto_loop(&g, 1000.0, n, 1.0).unwrap().1)
            .collect();
        assert_eq!(ends, [1500, 2000, 3000, 5000]);
        // the pad lengths below one beat: 1/8, 1/4, 1/2 of 500 ms
        let short: Vec<u32> = [0.125, 0.25, 0.5]
            .iter()
            .map(|&n| auto_loop(&g, 1000.0, n, 1.0).unwrap().1)
            .collect();
        assert_eq!(short, [1063, 1125, 1250]);
        assert_eq!(auto_loop(&[], 0.0, 4.0, 1.0), None);
    }

    #[test]
    fn halve_and_double_keep_the_start() {
        assert_eq!(halve(&[], (1000, 3000)), Some((1000, 2000)));
        assert_eq!(double(&[], (1000, 3000), 10_000), Some((1000, 5000)));
        // limits: too short to halve, no room to double (0 = unknown duration never blocks)
        assert_eq!(halve(&[], (1000, 1019)), None);
        assert_eq!(halve(&[], (1000, 1020)), Some((1000, 1010)));
        assert_eq!(double(&[], (1000, 3000), 4999), None);
        assert_eq!(double(&[], (1000, 3000), 5000), Some((1000, 5000)));
        assert_eq!(double(&[], (1000, 3000), 0), Some((1000, 5000)));
        // halve then double round-trips an even length
        assert_eq!(
            halve(&[], (500, 2500)).and_then(|l| double(&[], l, 0)),
            Some((500, 2500))
        );
    }

    /// 128 BPM: a beat is 468.75 ms, so the grid's beats sit 468 or 469 ms apart.
    fn grid_128() -> Vec<Beat> {
        (0..64)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12800,
                time_ms: (1000.0 + i as f64 * 468.75).round() as u32,
            })
            .collect()
    }

    #[test]
    fn halving_and_doubling_a_beat_loop_gives_the_same_loop_back() {
        let g = grid_128();
        for n in [1.0, 2.0, 4.0] {
            for pos in [1000.0, 1500.0, 3000.0] {
                let lp = auto_loop(&g, pos, n, 1.0).unwrap();
                let back = halve(&g, lp).and_then(|h| double(&g, h, 0));
                assert_eq!(back, Some(lp), "{n} beats at {pos}");
            }
        }
    }

    #[test]
    fn a_beat_loop_is_played_at_the_length_of_the_grid_tempo() {
        let g = grid_128();
        // 1 beat is 469 ms as the grid has it, 468.75 as the tempo has it: 0.25 ms more per pass would be 5 ms in 20 s
        let lp = auto_loop(&g, 1000.0, 1.0, 1.0).unwrap();
        assert_eq!(lp, (1000, 1469));
        assert_eq!(exact_loop(&g, lp), (1000.0, 1468.75));
        assert_eq!(
            exact_loop(&g, auto_loop(&g, 1000.0, 0.125, 1.0).unwrap()).1,
            1058.59375
        );
        // a loop that is not whole 1/8 beats, or that the tempo does not fit, is played as set
        assert_eq!(exact_loop(&g, (1000, 1300)), (1000.0, 1300.0));
        assert_eq!(exact_loop(&g, (1000, 1471)), (1000.0, 1471.0));
        assert_eq!(exact_loop(&[], (1000, 1469)), (1000.0, 1469.0));
    }

    #[test]
    fn closing_a_loop_behind_a_playing_deck_goes_on_at_the_same_phase_of_it() {
        // out at 3000 with the playhead 250.5 ms past it: as if the loop had wrapped there
        assert_eq!(resumes(3250.5, (1000, 3000)), Some(1251));
        assert_eq!(resumes(3000.0, (1000, 3000)), Some(1000));
        // many loops past it (a coarse SNAP put the out far behind): the phase only
        assert_eq!(resumes(7500.0, (1000, 3000)), Some(1500));
        // the playhead before the end plays on and wraps there: no jump
        assert_eq!(resumes(2999.0, (1000, 3000)), None);
    }

    #[test]
    fn a_loop_ending_past_the_track_does_not_fit() {
        let g = grid(0, 64); // the grid goes on to 31 500 ms
        let lp = auto_loop(&g, 31_200.0, 4.0, 1.0).unwrap(); // the last beats of a 32 s track
        assert_eq!(lp, (31_000, 33_000));
        assert!(!fits(lp, 32_000));
        assert!(fits(auto_loop(&g, 31_200.0, 2.0, 1.0).unwrap(), 32_000));
        assert!(fits(lp, 0), "unknown duration never blocks");
    }

    #[test]
    fn close_needs_a_start_before_the_end() {
        assert_eq!(close(Some(1000), 2000), Some((1000, 2000)));
        assert_eq!(
            [
                close(None, 2000),
                close(Some(2000), 2000),
                close(Some(2000), 1000)
            ],
            [None; 3]
        );
        assert_eq!(close(Some(1000), 1009), None);
    }

    #[test]
    fn loop_length_in_whole_beats_lights_the_pad() {
        let g = grid(1000, 16);
        assert_eq!(
            [2.0, 4.0, 1.0].map(|n| beats_len(&g, auto_loop(&g, 1000.0, n, 1.0).unwrap())),
            [2, 4, 1]
        );
        assert_eq!(beats_len(&g, (1000, 2750)), 0); // 3.5 beats
        assert_eq!(beats_len(&g, (1000, 1250)), 0); // half a beat
        assert_eq!(beats_len(&g, (1000, 3005)), 4); // 4.01 beats: within tolerance
        assert_eq!(beats_len(&[], (0, 1000)), 0);
    }

    #[test]
    fn another_beat_loop_length_resizes_the_active_loop_from_its_start() {
        let g = grid(1000, 16); // a beat every 500 ms
        assert_eq!(resized(&g, (1000, 3000), 1.0), Some((1000, 1500)));
        assert_eq!(
            resized(&g, (1250, 1750), 0.25),
            Some((1250, 1375)),
            "an off-beat start stays put"
        );
        assert_eq!(resized(&g, (1000, 1500), 8.0), Some((1000, 5000)));
        assert_eq!(resized(&[], (1000, 3000), 1.0), None);
    }
}
