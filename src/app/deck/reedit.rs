//! Undo / redo of library edits: the loaded track's cues and grid are read again after the store changed behind the deck's back.

use super::{cuelist, Deck};
use openfader::{
    analysis::{self, Beat},
    library::Library,
    store,
};
use slint::{ModelRc, VecModel};

impl Deck {
    /// Re-reads the stored cues and grid of the loaded track and shows them (nothing is written back: this is the store's
    /// state coming in). A track without stored edits gets the library's values; no cues at all get the automatic ones
    /// again, as a LOAD does.
    pub(in crate::app) fn reread_edits(&self, lib: &dyn Library) {
        if !self.ready() {
            return;
        }
        let Some(t) = self.with_track(Clone::clone) else {
            return;
        };
        let Ok(edits) = store::global().edits(&store::key(&t)) else {
            return;
        };
        // The unedited grid: our analysis when the load had it, else what the library says without the store's grid.
        let base = self
            .loaded
            .borrow()
            .base_beats
            .clone()
            .or_else(|| lib.analysis(&t).map(|a| a.beats));
        let beats = target_grid(edits.grid, base);
        if let Some(beats) = beats.filter(|b| *b != self.loaded.borrow().analysis.beats) {
            self.apply_grid(beats, "grid restored".into());
        }
        let phrases = match edits.phrases {
            Some(p) => Some(p),
            None => self.unedited_phrases(&self.loaded.borrow()),
        };
        {
            let mut l = self.loaded.borrow_mut();
            if l.analysis.phrases != phrases {
                l.analysis.phrases = phrases;
                l.invalidate();
            }
        }
        let stored = edits
            .cues
            .or_else(|| lib.analysis(&t).map(|a| a.cues))
            .unwrap_or_default();
        let mut l = self.loaded.borrow_mut();
        let auto = stored.is_empty();
        let cues = if auto {
            analysis::auto::cues(&l.analysis)
        } else {
            stored
        };
        if cues == l.analysis.cues {
            return;
        }
        l.analysis.cues = cues;
        l.auto_cues.set(auto);
        l.invalidate();
        let (pads, views) = (self.pad_views(&l), cuelist::views(&l.analysis));
        drop(l);
        self.show(|v| {
            v.pads = ModelRc::new(VecModel::from(pads));
            v.cues = ModelRc::new(VecModel::from(views));
            v.status = "cues restored".into();
        });
    }
}

/// The grid to show: the stored edit, else the unedited one. No stored grid and none from the library: the grid is this
/// session's own analysis, which stays (`None`).
fn target_grid(stored: Option<Vec<Beat>>, base: Option<Vec<Beat>>) -> Option<Vec<Beat>> {
    stored.or(base.filter(|b| !b.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(ms: u32) -> Vec<Beat> {
        vec![Beat {
            time_ms: ms,
            bpm_x100: 12000,
            bar_pos: 1,
        }]
    }

    #[test]
    fn undo_of_the_only_grid_edit_returns_the_unedited_grid() {
        assert_eq!(target_grid(None, Some(g(10))), Some(g(10)));
        assert_eq!(target_grid(Some(g(20)), Some(g(10))), Some(g(20)));
        assert_eq!(target_grid(None, Some(Vec::new())), None);
    }
}
