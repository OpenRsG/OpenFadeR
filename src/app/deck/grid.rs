//! GRID mode: beat-grid edits of one deck (the control set is
//! our design). Pure edits first (`shift`,
//! `set_downbeat`, `double`, `halve`, `tap`, `retempo`), then the [`Deck`] methods that apply them
//! to `Loaded.analysis.beats`. Everything that reads the grid sees an edit at once: the waveform grid
//! lines, SYNC, quantize, loops and `DeckView.bpm` (the BEAT FX tempo source). An edit is saved to
//! the store ([`openfader::store`], with the grid it replaced for RESET) and comes back on the next
//! LOAD; the library's files are never written. Export XML writes the edit.
//!
//! ponytail: bar length is the grid's largest bar position (4/4 and 3/4 keep their length); no
//! per-segment tempo edit, a TAP replaces the whole grid with a constant-tempo one.

use super::{Deck, Loaded, exportedit, fmt_ms, grid_bpm, loops};
use openfader::{analysis::Beat, store};
use std::{
    sync::{Arc, LazyLock},
    time::Instant,
};

/// A tap this long after the previous one starts a new series; only the last [`TAP_WINDOW`] count.
const TAP_RESET_MS: f64 = 2000.0;
const TAP_WINDOW: usize = 8;
/// Taps outside this tempo range (a double click, a forgotten series) are ignored.
const TAP_BPM: std::ops::RangeInclusive<f64> = 30.0..=300.0;

/// Beats per bar of the grid (its largest bar position, 1..=4; 4 for an empty grid).
pub(super) fn per_bar(beats: &[Beat]) -> u16 {
    beats
        .iter()
        .map(|b| b.bar_pos)
        .max()
        .unwrap_or(4)
        .clamp(1, 4)
}

/// Index of the grid's first bar-1 beat (0 without one).
pub(super) fn downbeat(beats: &[Beat]) -> usize {
    beats.iter().position(|b| b.bar_pos == 1).unwrap_or(0)
}

/// Bar positions counted from beat `anchor` (= bar position 1), `per` beats per bar.
fn renumber(beats: &mut [Beat], per: u16, anchor: usize) {
    for (i, b) in beats.iter_mut().enumerate() {
        b.bar_pos = (i as i64 - anchor as i64).rem_euclid(per.into()) as u16 + 1;
    }
}

/// The beat nearest to `ms` and its index; `None` for an empty grid.
pub fn nearest(beats: &[Beat], ms: f64) -> Option<(usize, Beat)> {
    beats.iter().copied().enumerate().min_by(|(_, a), (_, b)| {
        (a.time_ms as f64 - ms)
            .abs()
            .total_cmp(&(b.time_ms as f64 - ms).abs())
    })
}

/// Every beat `delta_ms` later (earlier when negative); beats that would fall before 0 are dropped.
pub fn shift(beats: &[Beat], delta_ms: i64) -> Vec<Beat> {
    beats
        .iter()
        .filter_map(|b| {
            u32::try_from(b.time_ms as i64 + delta_ms)
                .ok()
                .map(|time_ms| Beat { time_ms, ..*b })
        })
        .collect()
}

/// "Beat 1 here": the beat nearest `pos_ms` moves onto `pos_ms` (the whole grid with it) and
/// becomes bar position 1. `None` for an empty grid.
pub fn set_downbeat(beats: &[Beat], pos_ms: f64) -> Option<Vec<Beat>> {
    let (i, b) = nearest(beats, pos_ms)?;
    let mut v = beats.to_vec();
    renumber(&mut v, per_bar(beats), i);
    Some(shift(&v, pos_ms.round() as i64 - b.time_ms as i64))
}

/// BPM × 2: a beat inserted halfway after every beat (the last one uses its own interval), every
/// tempo doubled, the downbeat stays a downbeat. `None` for fewer than 2 beats or tempo overflow.
pub fn double(beats: &[Beat]) -> Option<Vec<Beat>> {
    if beats.len() < 2 {
        return None;
    }
    let mut v = Vec::with_capacity(beats.len() * 2);
    for (i, b) in beats.iter().enumerate() {
        let bpm_x100 = b.bpm_x100.checked_mul(2)?;
        let t = b.time_ms as i64;
        let next = beats
            .get(i + 1)
            .map_or_else(|| 2 * t - beats[i - 1].time_ms as i64, |n| n.time_ms as i64);
        v.push(Beat { bpm_x100, ..*b });
        v.push(Beat {
            bpm_x100,
            time_ms: ((t + next + 1) / 2) as u32,
            ..*b
        });
    }
    renumber(&mut v, per_bar(beats), 2 * downbeat(beats));
    Some(v)
}

/// BPM ÷ 2: every second beat is dropped, keeping the downbeat's parity, every tempo halved.
/// `None` when fewer than 2 beats would remain or a tempo would reach 0.
pub fn halve(beats: &[Beat]) -> Option<Vec<Beat>> {
    let d = downbeat(beats);
    let mut v: Vec<Beat> = beats
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == d % 2)
        .map(|(_, b)| Beat {
            bpm_x100: b.bpm_x100 / 2,
            ..*b
        })
        .collect();
    if v.len() < 2 || v.iter().any(|b| b.bpm_x100 == 0) {
        return None;
    }
    renumber(&mut v, per_bar(beats), d / 2);
    Some(v)
}

/// Registers a tap at `now_ms` in `taps` and returns the tempo of the series: the average interval
/// of the last [`TAP_WINDOW`] taps. `None` for the first tap or an implausible tempo.
pub fn tap(taps: &mut Vec<f64>, now_ms: f64) -> Option<f64> {
    if taps.last().is_some_and(|&l| now_ms - l > TAP_RESET_MS) {
        taps.clear();
    }
    taps.push(now_ms);
    if taps.len() > TAP_WINDOW {
        taps.remove(0);
    }
    if taps.len() < 2 {
        return None;
    }
    let bpm = 60_000.0 * (taps.len() - 1) as f64 / (now_ms - taps[0]);
    TAP_BPM.contains(&bpm).then_some(bpm)
}

/// Constant-tempo grid at `bpm` through the beat at `anchor_ms` (bar position `anchor_bar`),
/// covering 0 … `end_ms`. `None` when the tempo does not fit the grid's BPM × 100 field.
pub fn retempo(
    anchor_ms: f64,
    anchor_bar: u16,
    per: u16,
    bpm: f64,
    end_ms: f64,
) -> Option<Vec<Beat>> {
    let x100 = (bpm * 100.0).round();
    if !(1.0..=f64::from(u16::MAX)).contains(&x100) {
        return None;
    }
    let d = 6_000_000.0 / x100;
    let (first, last) = (
        -(anchor_ms / d).floor() as i64,
        ((end_ms.max(anchor_ms) - anchor_ms) / d).floor() as i64,
    );
    Some(
        (first..=last)
            .map(|k| Beat {
                bar_pos: (i64::from(anchor_bar) - 1 + k).rem_euclid(per.into()) as u16 + 1,
                bpm_x100: x100 as u16,
                time_ms: (anchor_ms + k as f64 * d).round().max(0.0) as u32,
            })
            .collect(),
    )
}

/// Tap series of a deck: the tap times and the beat the new grid hangs on (fixed at the first tap).
#[derive(Default)]
pub struct Tap {
    times: Vec<f64>,
    anchor: Option<(f64, u16)>,
}

/// Milliseconds since the first call: the clock of the tap series.
fn now_ms() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64() * 1000.0
}

/// Puts the current grid on the undo list.
fn record_undo(l: &mut Loaded) {
    let before = l.analysis.beats.clone();
    l.grid_hist.record(before);
}

impl Deck {
    /// Hands the grid's beat times (ms, ascending) to the player for the metronome; call after a LOAD (which clears them)
    /// and after every grid edit. No grid = `None`: the metronome is silent.
    pub(super) fn push_beats(&self) {
        let times: Arc<[u32]> = self
            .loaded
            .borrow()
            .analysis
            .beats
            .iter()
            .map(|b| b.time_ms)
            .collect();
        self.with_player(|p| p.set_beats((!times.is_empty()).then_some(times)));
    }

    /// METRO chip: the player clicks on every beat of the grid while on (tone and volume are the Preferences').
    pub fn toggle_metronome(&self) {
        let on = !self.view.borrow().metronome;
        self.with_player(|p| p.set_metronome(on));
        self.show(|v| v.metronome = on);
    }

    /// GRID chip: show / hide the edit row (the beat lines of the enlarged waveform turn blue while it is open).
    pub fn toggle_grid_edit(&self) {
        let on = !self.view.borrow().grid_edit;
        self.loaded.borrow_mut().invalidate();
        self.show(|v| v.grid_edit = on);
    }

    /// An edit: the grid it replaces goes on the undo list, then [`Deck::commit_grid`].
    fn set_grid(&self, beats: Vec<Beat>, status: String) {
        record_undo(&mut self.loaded.borrow_mut());
        self.commit_grid(beats, status);
    }

    /// [`Deck::apply_grid`], then saved to the store (it comes back on the next LOAD) and soon to the device copies (cue sync).
    pub(super) fn commit_grid(&self, beats: Vec<Beat>, mut status: String) {
        let saved: Result<(), String> = {
            let l = self.loaded.borrow();
            l.track.as_ref().map_or(Ok(()), |t| {
                store::global().set_grid(&store::key(t), &beats, &l.lib_beats)?;
                crate::app::device::track_edited(t);
                Ok(())
            })
        };
        if let Err(e) = saved {
            status += &tr!(" · NOT saved: {}", e);
        }
        self.apply_grid(beats, status);
    }

    /// Makes `beats` the deck's grid and pushes it everywhere the grid shows or is read.
    pub(super) fn apply_grid(&self, beats: Vec<Beat>, status: String) {
        let mut l = self.loaded.borrow_mut();
        let grid = grid_bpm(&beats);
        l.base_bpm = grid.unwrap_or(l.lib_bpm);
        l.analysis.beats = beats;
        // phrases are numbered by beat: derive ours again unless the user edited them
        let edited = l
            .track
            .as_ref()
            .is_some_and(|t| matches!(store::global().phrase_edit(&store::key(t)), Ok(Some(_))));
        if !edited {
            l.analysis.phrases = None;
            openfader::analysis::auto::fill(&mut l.analysis);
        }
        l.invalidate();
        let loop_beats = l
            .loop_
            .map_or(0, |lp| loops::beats_len(&l.analysis.beats, lp));
        let pads = self.pad_views(&l);
        drop(l);
        self.show(|v| {
            v.has_grid = grid.is_some();
            v.loop_beats = loop_beats;
            v.pads = slint::ModelRc::new(slint::VecModel::from(pads));
            v.status = status.into();
        });
        self.set_rate(self.rate.get()); // effective BPM, key and the BEAT FX tempo follow
        self.push_beats();
    }

    /// Runs `edit` on the grid and the playhead (ms) within the GRID range (the whole track, or the beats from the playhead
    /// on: [`exportedit`]); `None` → "needs a grid".
    pub(super) fn edit_grid(&self, edit: impl FnOnce(&[Beat], f64) -> Option<(Vec<Beat>, String)>) {
        self.edit_range(true, edit);
    }

    fn edit_range(
        &self,
        ranged: bool,
        edit: impl FnOnce(&[Beat], f64) -> Option<(Vec<Beat>, String)>,
    ) {
        if !self.ready() || self.grid_refused() {
            return;
        }
        let l = self.loaded.borrow();
        let (pos, _) = self.clock(&l);
        let per = per_bar(&l.analysis.beats);
        let (head, tail) =
            exportedit::split(&l.analysis.beats, pos, ranged && l.grid_hist.from_here);
        let r = edit(tail, pos).map(|(b, s)| (exportedit::join(head, b, per), s));
        drop(l);
        match r {
            Some((beats, status)) => self.set_grid(beats, status),
            None => self.say(tr!(
                "grid: needs a beat grid with at least 2 beats (TAP sets one) and a tempo that fits"
            )),
        }
    }

    /// Shift the whole grid by `ms` (negative = earlier).
    pub fn grid_shift(&self, ms: i32) {
        self.edit_grid(|b, _| {
            (!b.is_empty()).then(|| {
                (
                    shift(b, ms.into()),
                    tr!("grid shifted {} ms", format!("{ms:+}")),
                )
            })
        });
    }

    /// "Beat 1 here": the beat nearest the playhead lands on it as the downbeat (always the whole grid).
    pub fn grid_beat1(&self) {
        self.edit_range(false, |b, pos| {
            set_downbeat(b, pos).map(|v| (v, tr!("beat 1 at {}", fmt_ms(pos as u32))))
        });
    }

    pub fn grid_double(&self) {
        self.rescale(double, "×2");
    }

    pub fn grid_halve(&self) {
        self.rescale(halve, "÷2");
    }

    fn rescale(&self, f: fn(&[Beat]) -> Option<Vec<Beat>>, label: &str) {
        self.edit_grid(|b, _| {
            f(b).map(|v| {
                let s = tr!(
                    "grid {} · {} BPM",
                    label,
                    format!("{:.2}", grid_bpm(&v).unwrap_or(0.0))
                );
                (v, s)
            })
        });
    }

    /// TAP: from the second tap on, the average tempo of the series becomes a constant-tempo grid
    /// through the beat nearest the playhead at the first tap (the playhead itself without a grid).
    pub fn grid_tap(&self) {
        if !self.ready() || self.grid_refused() {
            return;
        }
        let l = self.loaded.borrow();
        let (pos, _) = self.clock(&l);
        let mut t = self.tap.borrow_mut();
        let bpm = tap(&mut t.times, now_ms());
        if t.times.len() == 1 {
            t.anchor = Some(
                nearest(&l.analysis.beats, pos)
                    .map_or((pos, 1), |(_, b)| (b.time_ms as f64, b.bar_pos)),
            );
        }
        let Some(((anchor, bar), bpm)) = t.anchor.zip(bpm) else {
            drop((t, l));
            return self.say(tr!("tap tempo: keep tapping on the beat"));
        };
        drop((t, l));
        self.regrid((anchor, bar), bpm, tr!("tap tempo"));
    }

    /// A constant-tempo grid of `bpm` through the beat `(ms, bar position)`: the whole track, or from that beat on (GRID range).
    pub(super) fn regrid(&self, (anchor, bar): (f64, u16), bpm: f64, label: &str) {
        let l = self.loaded.borrow();
        let per = per_bar(&l.analysis.beats);
        let end = l
            .duration_ms
            .max(l.analysis.beats.last().map_or(0, |b| b.time_ms)) as f64;
        let from = anchor.round() as u32;
        let grid = retempo(anchor, bar, per, bpm, end).map(|mut v| {
            let (head, _) =
                exportedit::split(&l.analysis.beats, f64::from(from), l.grid_hist.from_here);
            if head.is_empty() {
                return v;
            }
            v.retain(|b| b.time_ms >= from);
            exportedit::join(head, v, per)
        });
        drop(l);
        match grid {
            Some(v) => self.set_grid(v, tr!("{} · {} BPM", label, format!("{bpm:.2}"))),
            None => self.say(tr!("{}: tempo does not fit the grid", label)),
        }
    }

    /// RESET: back to the grid the library (or the analysis, for an unanalysed track) gave.
    pub fn grid_reset(&self) {
        if !self.ready() || self.grid_refused() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let stored = l
            .track
            .as_ref()
            .map(|t| store::global().reset_grid(&store::key(t)));
        let (beats, status) = match stored {
            Some(Ok(Some(orig))) => {
                l.lib_beats = orig.clone();
                (orig, tr!("grid reset to the original").to_owned())
            }
            Some(Err(e)) => {
                drop(l);
                return self.say(tr!("grid reset failed: {}", e));
            }
            _ => (
                l.lib_beats.clone(),
                tr!("grid reset to the library's").to_owned(),
            ),
        };
        record_undo(&mut l);
        drop(l);
        self.apply_grid(beats, status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` beats from `start` ms, 500 ms apart (120 BPM), bar positions counted from `bar`.
    fn grid(start: u32, n: u32, bar: u32) -> Vec<Beat> {
        (0..n)
            .map(|i| Beat {
                bar_pos: ((i + bar - 1) % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: start + 500 * i,
            })
            .collect()
    }

    fn times(b: &[Beat]) -> Vec<u32> {
        b.iter().map(|b| b.time_ms).collect()
    }

    fn bars(b: &[Beat]) -> Vec<u16> {
        b.iter().map(|b| b.bar_pos).collect()
    }

    #[test]
    fn shift_moves_every_beat_and_drops_those_before_zero() {
        let g = grid(100, 4, 1);
        assert_eq!(times(&shift(&g, 7)), [107, 607, 1107, 1607]);
        assert_eq!(times(&shift(&g, -100)), [0, 500, 1000, 1500]);
        let back = shift(&g, -600); // beats at 100 and 600 → -500 (dropped) and 0
        assert_eq!(
            (times(&back), bars(&back)),
            (vec![0, 500, 1000], vec![2, 3, 4])
        );
    }

    #[test]
    fn downbeat_moves_the_nearest_beat_onto_the_playhead_as_bar_one() {
        let g = grid(0, 8, 1); // beats 0, 500, … ; playhead at 1530 → beat at 1500 (bar pos 4)
        let v = set_downbeat(&g, 1530.0).unwrap();
        assert_eq!(times(&v)[3], 1530);
        assert_eq!(bars(&v), [2, 3, 4, 1, 2, 3, 4, 1]);
        assert_eq!(
            times(&v)
                .windows(2)
                .map(|w| w[1] - w[0])
                .collect::<Vec<_>>(),
            [500; 7]
        );
        assert!(set_downbeat(&[], 10.0).is_none());
    }

    #[test]
    fn double_inserts_beats_and_halve_takes_them_out_again() {
        let g = grid(100, 4, 1);
        let d = double(&g).unwrap();
        assert_eq!(times(&d), [100, 350, 600, 850, 1100, 1350, 1600, 1850]);
        assert_eq!(bars(&d), [1, 2, 3, 4, 1, 2, 3, 4]);
        assert!(d.iter().all(|b| b.bpm_x100 == 24000));
        assert_eq!(halve(&d).unwrap(), g);
        // a grid starting mid-bar keeps its downbeat: bar-1 beat is index 1 (odd) → odd beats stay
        let h = halve(&grid(0, 8, 4)).unwrap();
        assert_eq!(
            (times(&h), bars(&h)),
            (vec![500, 1500, 2500, 3500], vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn double_and_halve_refuse_what_does_not_fit() {
        assert!(double(&grid(0, 1, 1)).is_none());
        let fast: Vec<Beat> = grid(0, 4, 1)
            .into_iter()
            .map(|b| Beat {
                bpm_x100: 40000,
                ..b
            })
            .collect();
        assert!(double(&fast).is_none(), "80000 does not fit u16");
        assert!(halve(&grid(0, 2, 1)).is_none(), "one beat left");
    }

    #[test]
    fn three_four_keeps_its_bar_length() {
        let g: Vec<Beat> = (0..6)
            .map(|i| Beat {
                bar_pos: i % 3 + 1,
                bpm_x100: 9000,
                time_ms: i as u32 * 667,
            })
            .collect();
        assert_eq!(bars(&set_downbeat(&g, 667.0).unwrap()), [3, 1, 2, 3, 1, 2]);
    }

    #[test]
    fn tap_averages_the_last_taps_and_a_pause_starts_over() {
        let mut t = Vec::new();
        assert_eq!(tap(&mut t, 0.0), None);
        assert!((tap(&mut t, 500.0).unwrap() - 120.0).abs() < 1e-9);
        // slightly uneven taps average out: 4 intervals in 2010 ms
        tap(&mut t, 1000.0);
        tap(&mut t, 1520.0);
        let bpm = tap(&mut t, 2010.0).unwrap();
        assert!((bpm - 60_000.0 * 4.0 / 2010.0).abs() < 1e-9);
        // only the last 8 taps count: a slow start falls out of the window
        let mut t = vec![0.0];
        for i in 1..=12 {
            tap(&mut t, 1000.0 + 500.0 * i as f64);
        }
        assert_eq!(t.len(), TAP_WINDOW);
        assert!((tap(&mut t, 7500.0).unwrap() - 120.0).abs() < 1e-9);
        // > 2 s of silence restarts the series; absurd tempi are ignored
        assert_eq!(tap(&mut t, 20_000.0), None);
        assert_eq!(t.len(), 1);
        assert_eq!(tap(&mut t, 20_050.0), None, "1200 BPM");
    }

    #[test]
    fn retempo_hangs_a_constant_grid_on_the_anchor_beat() {
        let g = retempo(1250.0, 3, 4, 120.0, 3000.0).unwrap();
        assert_eq!(times(&g), [250, 750, 1250, 1750, 2250, 2750]);
        assert_eq!(bars(&g), [1, 2, 3, 4, 1, 2]);
        assert!(g.iter().all(|b| b.bpm_x100 == 12000));
        assert!(
            retempo(0.0, 1, 4, 700.0, 1000.0).is_none(),
            "70000 does not fit u16"
        );
        assert!(retempo(0.0, 1, 4, 0.0, 1000.0).is_none());
    }
}
