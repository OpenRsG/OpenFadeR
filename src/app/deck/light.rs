//! What LIGHTING needs from a deck: the beat position and phrase while it plays.

use super::{Deck, beat_at, grid_bpm};
use openfader::lighting::{Input, scene::PhraseCtx};

impl Deck {
    /// The playing deck's beat on its own grid, effective tempo and phrase; `None` when it is stopped or has no grid.
    pub(in crate::app) fn light_input(&self) -> Option<Input> {
        let l = self.loaded.borrow();
        let (pos, playing) = self.clock(&l);
        if !playing || !l.ready {
            return None;
        }
        let beat = beat_at(&l.analysis.beats, pos)?;
        let bpm = grid_bpm(&l.analysis.beats)? * self.rate();
        Some(Input {
            beat,
            bpm,
            phrase: l.analysis.phrases.as_ref().and_then(|p| PhraseCtx::at(p, beat)),
        })
    }
}
