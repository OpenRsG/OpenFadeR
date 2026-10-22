//! What EXPORT mode's panels (`app/exportmode`) need beyond the PERFORMANCE decks, all on the deck's own grid and cues:
//! the GRID panel's range (the whole track, or the beats from the playhead on), widen / narrow the
//! beat spacing by 1 ms (6 / 7), a typed BPM (2), UNDO / REDO (12 / 13) and the Analysis Lock (16) that blocks grid edits
//!, plus the hot cue buttons that force the HOT CUE pad mode. Spacing, range and BPM-entry semantics are
//! our own.
//!
//! The range is applied where every grid edit already runs (`Deck::edit_range`): the edit sees only the tail, [`join`] puts the
//! untouched head back. History holds whole grids (`HIST` of them, ~6 bytes a beat); it is dropped with the track.

use super::{Deck, grid::nearest, pads};
use crate::app::PadView;
use openfader::{analysis::Beat, store};

/// Undo steps kept per track.
const HIST: usize = 100;

#[derive(Default)]
pub struct Hist {
    undo: Vec<Vec<Beat>>,
    redo: Vec<Vec<Beat>>,
    /// Range button: edits touch the beats from the playhead on (`false` = the whole track).
    pub(super) from_here: bool,
}

impl Hist {
    /// `before` is the grid an edit replaces; a new edit ends the redo line.
    pub fn record(&mut self, before: Vec<Beat>) {
        if self.undo.len() == HIST {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
    }
}

/// What the GRID panel shows of a deck.
pub struct GridInfo {
    pub can_undo: bool,
    pub can_redo: bool,
    pub from_here: bool,
    pub locked: bool,
    /// Tempo of the grid at the playhead, "114.50"; empty without a grid.
    pub bpm: String,
}

/// `(kept, edited)`: the whole grid is edited, or, with `from_here`, the beats from the playhead on.
pub fn split(beats: &[Beat], pos_ms: f64, from_here: bool) -> (&[Beat], &[Beat]) {
    let at = if from_here {
        beats.partition_point(|b| (b.time_ms as f64) < pos_ms)
    } else {
        0
    };
    beats.split_at(at)
}

/// `head` ++ the edited `tail` (beats not after the head's end are dropped); the tail's bar positions continue the head's.
pub fn join(head: &[Beat], tail: Vec<Beat>, per: u16) -> Vec<Beat> {
    let Some(last) = head.last() else {
        return tail;
    };
    let mut v = head.to_vec();
    let next = usize::from(last.bar_pos);
    v.extend(
        tail.into_iter()
            .filter(|b| b.time_ms > last.time_ms)
            .enumerate()
            .map(|(j, b)| Beat {
                bar_pos: ((next + j) % usize::from(per)) as u16 + 1,
                ..b
            }),
    );
    v
}

/// Every beat's distance to the one at `anchor` grows by `delta_ms` per beat (negative narrows): the beat at `anchor` stays,
/// beats pushed before 0 are dropped, the tempo of every beat follows its new interval. `None` for fewer than 2 beats left,
/// an interval that reaches 0 or a tempo that does not fit the BPM × 100 field.
pub fn stretch(beats: &[Beat], anchor: usize, delta_ms: i64) -> Option<Vec<Beat>> {
    let kept: Vec<(usize, u32)> = beats
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            let t = i64::from(b.time_ms) + (i as i64 - anchor as i64) * delta_ms;
            u32::try_from(t).ok().map(|t| (i, t))
        })
        .collect();
    if kept.len() < 2 {
        return None;
    }
    kept.iter()
        .enumerate()
        .map(|(k, &(i, t))| {
            let (a, b) = match kept.get(k + 1) {
                Some(&(_, next)) => (t, next),
                None => (kept[k - 1].1, t),
            };
            let x100 = 6_000_000.0 / f64::from(b.checked_sub(a).filter(|&d| d > 0)?);
            Some(Beat {
                time_ms: t,
                bpm_x100: u16::try_from(x100.round() as i64).ok().filter(|&x| x > 0)?,
                ..beats[i]
            })
        })
        .collect()
}

/// Tempo of the grid at `ms` in BPM (the interval around it); `None` for fewer than 2 beats.
pub fn bpm_at(beats: &[Beat], ms: f64) -> Option<f64> {
    if beats.len() < 2 {
        return None;
    }
    let i = beats
        .partition_point(|b| (b.time_ms as f64) <= ms)
        .saturating_sub(1)
        .min(beats.len() - 2);
    let d = f64::from(beats[i + 1].time_ms) - f64::from(beats[i].time_ms);
    (d > 0.0).then(|| 60_000.0 / d)
}

impl Deck {
    /// Whether the track has an Analysis Lock (no grid editing, no re-analysis).
    pub(in crate::app) fn grid_locked(&self) -> bool {
        self.loaded.borrow().track.as_ref().is_some_and(|t| {
            store::global()
                .analysis_locked(&store::key(t))
                .unwrap_or(false)
        })
    }

    /// Says so and returns `true` while the Analysis Lock blocks grid edits.
    pub(super) fn grid_refused(&self) -> bool {
        let locked = self.grid_locked();
        if locked {
            self.say(tr!(
                "Analysis Lock is on: unlock the track to edit its grid"
            ));
        }
        locked
    }

    /// The padlock of the GRID panel.
    pub fn toggle_grid_lock(&self) {
        let Some(key) = self.loaded.borrow().track.as_ref().map(store::key) else {
            return self.say(tr!("the Analysis Lock needs a track of the library"));
        };
        let on = !self.grid_locked();
        match store::global().set_analysis_lock(&key, on) {
            Ok(()) => self.say(if on {
                tr!("Analysis Lock on: the grid and the analysis are kept")
            } else {
                tr!("Analysis Lock off")
            }),
            Err(e) => self.say(tr!("Analysis Lock: {}", e)),
        }
    }

    pub fn grid_undo(&self) {
        self.grid_step(true);
    }

    pub fn grid_redo(&self) {
        self.grid_step(false);
    }

    /// One step along the history: the grid in use goes to the opposite list.
    fn grid_step(&self, undo: bool) {
        if !self.ready() || self.grid_refused() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let prev = if undo {
            l.grid_hist.undo.pop()
        } else {
            l.grid_hist.redo.pop()
        };
        let Some(prev) = prev else {
            drop(l);
            return self.say(if undo {
                tr!("nothing to undo")
            } else {
                tr!("nothing to redo")
            });
        };
        let now = std::mem::take(&mut l.analysis.beats);
        if undo {
            l.grid_hist.redo.push(now);
        } else {
            l.grid_hist.undo.push(now);
        }
        drop(l);
        self.commit_grid(
            prev,
            if undo {
                tr!("grid undone")
            } else {
                tr!("grid redone")
            }
            .into(),
        );
    }

    /// Range buttons: `true` edits from the playhead on, `false` the whole track.
    pub fn grid_range(&self, from_here: bool) {
        self.loaded.borrow_mut().grid_hist.from_here = from_here;
        self.say(if from_here {
            tr!("grid edits apply from the playhead on")
        } else {
            tr!("grid edits apply to the whole track")
        });
    }

    /// Widen (`ms` > 0) or narrow the distance between beats by `ms` per beat around the beat at the playhead.
    pub fn grid_stretch(&self, ms: i32) {
        self.edit_grid(|b, pos| {
            let (i, _) = nearest(b, pos)?;
            stretch(b, i, ms.into()).map(|v| (v, tr!("beat distance {} ms", format!("{ms:+}"))))
        });
    }

    /// A typed BPM: a constant-tempo grid through the beat at the playhead (the playhead itself without a grid).
    pub fn grid_set_bpm(&self, bpm: f64) {
        if !self.ready() || self.grid_refused() {
            return;
        }
        let anchor = {
            let l = self.loaded.borrow();
            let (pos, _) = self.clock(&l);
            nearest(&l.analysis.beats, pos).map_or((pos, 1), |(_, b)| (f64::from(b.time_ms), b.bar_pos))
        };
        self.regrid(anchor, bpm, tr!("BPM entered"));
    }

    pub(in crate::app) fn grid_info(&self) -> GridInfo {
        let locked = self.grid_locked();
        let l = self.loaded.borrow();
        let (pos, _) = self.clock(&l);
        GridInfo {
            can_undo: !l.grid_hist.undo.is_empty(),
            can_redo: !l.grid_hist.redo.is_empty(),
            from_here: l.grid_hist.from_here,
            locked,
            bpm: bpm_at(&l.analysis.beats, pos).map_or(String::new(), |b| format!("{b:.2}")),
        }
    }

    /// The 8 hot cues A to H whatever the pad mode is.
    pub(in crate::app) fn hot_views(&self) -> Vec<PadView> {
        pads::views(&self.loaded.borrow().analysis)
    }

    /// Hot cue button: the pads show the hot cues first (HOT CUE mode), then sets / plays cue `pad`.
    pub fn hot_cue(&self, pad: usize) {
        if self.view.borrow().pad_mode != 0 {
            self.set_pad_mode(0);
        }
        self.pad(pad);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::deck::grid::shift;

    fn grid(n: u32) -> Vec<Beat> {
        (0..n)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect()
    }

    #[test]
    fn the_range_keeps_the_head_and_bars_continue() {
        let g = grid(8);
        let (head, tail) = split(&g, 1600.0, true);
        assert_eq!((head.len(), tail[0].time_ms), (4, 2000));
        let joined = join(head, shift(tail, -700), 4);
        // 2000 - 700 = 1300 is before the head's end (1500): dropped; the rest keeps counting bars
        assert_eq!(
            joined.iter().map(|b| b.bar_pos).collect::<Vec<_>>(),
            [1, 2, 3, 4, 1, 2, 3]
        );
        assert_eq!(split(&g, 1600.0, false).0.len(), 0);
    }

    #[test]
    fn widening_moves_every_beat_away_from_the_anchor_and_sets_the_tempo() {
        let g = shift(&grid(5), 1000); // 1000 1500 2000 2500 3000
        let w = stretch(&g, 2, 1).unwrap();
        assert_eq!(
            w.iter().map(|b| b.time_ms).collect::<Vec<_>>(),
            [998, 1499, 2000, 2501, 3002]
        );
        assert!(w.iter().all(|b| b.bpm_x100 == 11976)); // 60000 / 501 ms
        assert_eq!(stretch(&g, 0, -1).unwrap()[4].time_ms, 2996);
        assert_eq!(
            stretch(&g, 0, -500),
            None,
            "intervals reaching 0 are refused"
        );
        assert_eq!(stretch(&grid(1), 0, 1), None);
        // beats pushed before 0 are dropped, the bar positions of the rest stay
        let early = stretch(&grid(5), 2, 1).unwrap();
        assert_eq!((early.len(), early[0].bar_pos), (4, 2));
    }

    #[test]
    fn bpm_at_reads_the_interval_around_the_playhead() {
        let mut g = grid(4);
        g[3].time_ms = 1400; // the last interval is 400 ms = 150 BPM
        assert_eq!(bpm_at(&g, 100.0), Some(120.0));
        assert_eq!(bpm_at(&g, 1200.0), Some(150.0));
        assert_eq!(bpm_at(&g, 9999.0), Some(150.0));
        assert_eq!(bpm_at(&grid(1), 0.0), None);
    }

    #[test]
    fn history_is_bounded_and_a_new_edit_ends_the_redo_line() {
        let mut h = Hist::default();
        h.redo.push(Vec::new());
        for i in 0..HIST as u32 + 5 {
            h.record(grid(i % 3));
        }
        assert_eq!((h.undo.len(), h.redo.len()), (HIST, 0));
    }
}
