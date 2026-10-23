//! What LIGHTING and the MIDI clock need from a deck: the beat position (and phrase) while it plays, and which deck leads.

use super::{DECKS, Deck, Decks, beat_at, cue::Preview, grid, tempo_at};
use openfader::lighting::{Input, scene::PhraseCtx};

/// What the MIDI clock follows: the beat counted from the grid's first downbeat (a multiple of `per_bar` is a bar's first
/// beat), the effective tempo and the beats per bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) struct Pulse {
    pub beat: f64,
    pub bpm: f64,
    pub per_bar: u16,
}

/// The first of `master` then decks A to D for which `f` answers: the master while it plays, else the first playing deck.
pub(in crate::app) fn lead<T>(
    decks: &Decks,
    master: Option<usize>,
    f: impl Fn(&Deck) -> Option<T>,
) -> Option<T> {
    master
        .filter(|&m| m < DECKS)
        .into_iter()
        .chain(0..DECKS)
        .find_map(|i| f(&decks[i]))
}

impl Deck {
    /// The playing deck's beat on its own grid, effective tempo and phrase; `None` when it is stopped or has no grid.
    pub(in crate::app) fn light_input(&self) -> Option<Input> {
        let l = self.loaded.borrow();
        let (pos, playing) = self.clock(&l);
        if !playing || !l.ready {
            return None;
        }
        let beat = beat_at(&l.analysis.beats, pos)?;
        let bpm = tempo_at(&l.analysis.beats, pos)? * self.rate();
        Some(Input {
            beat,
            bpm,
            phrase: l
                .analysis
                .phrases
                .as_ref()
                .and_then(|p| PhraseCtx::at(p, beat)),
        })
    }

    /// [`Pulse`] of the playing deck; `None` when it is stopped, only previewing its cue (CUE held) or has no grid.
    pub(in crate::app) fn pulse_input(&self) -> Option<Pulse> {
        if self.cue_state.get() == Preview::Held {
            return None;
        }
        let l = self.loaded.borrow();
        let (pos, playing) = self.clock(&l);
        if !playing || !l.ready {
            return None;
        }
        let beats = &l.analysis.beats;
        Some(Pulse {
            beat: beat_at(beats, pos)? - grid::downbeat(beats) as f64,
            bpm: tempo_at(beats, pos)? * self.rate(),
            per_bar: grid::per_bar(beats),
        })
    }
}
