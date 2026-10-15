//! Plugin pad modes ([`PadModeDecl`]): mode buttons after the built-in ones. The pads are the deck's hot cues, as in
//! HOT CUE, but hitting a pad runs the mode's `press` steps and letting it go runs `release`, on the deck, in order.
//! Nothing is set or deleted on the pads. The steps are named by [`PadStep`]; [`Steps`] is what a deck does for each.

use super::{Deck, PadMode, hot};
use openfader::plugin::{PadModeDecl, PadStep, registry};

/// What the steps of a pad mode do to a deck. Every step is a no-op where it does not apply (a pad without a hot cue,
/// no beat grid, no loop).
pub(super) trait Steps {
    /// To pad `pad`'s hot cue, playing or paused as before; never sets a cue or starts playback.
    fn jump(&self, pad: usize);
    fn play(&self);
    fn pause(&self);
    fn beat_jump(&self, beats: f64);
    /// A loop of `beats` from the playhead.
    fn beat_loop(&self, beats: f64);
    fn loop_off(&self);
}

pub(super) fn run(steps: &[PadStep], pad: usize, on: &impl Steps) {
    for &step in steps {
        match step {
            PadStep::Jump => on.jump(pad),
            PadStep::Play => on.play(),
            PadStep::Pause => on.pause(),
            PadStep::BeatJump(beats) => on.beat_jump(beats),
            PadStep::Loop(beats) => on.beat_loop(beats),
            PadStep::LoopOff => on.loop_off(),
        }
    }
}

/// How many plugin modes there are.
pub(super) fn count() -> usize {
    registry().pad_modes().count()
}

/// Their button labels.
pub(super) fn names() -> impl Iterator<Item = &'static str> {
    registry().pad_modes().map(|m| m.name.as_str())
}

impl Steps for Deck {
    fn jump(&self, pad: usize) {
        let ms = hot(&self.loaded.borrow().analysis, pad).map(|c| c.time_ms);
        if let Some(ms) = ms {
            self.with_player(|p| p.seek(ms));
        }
    }

    fn play(&self) {
        self.with_player(|p| p.play());
    }

    fn pause(&self) {
        self.with_player(|p| p.pause());
    }

    fn beat_jump(&self, beats: f64) {
        Deck::beat_jump(self, beats);
    }

    fn beat_loop(&self, beats: f64) {
        Deck::beat_loop(self, beats);
    }

    fn loop_off(&self) {
        let active = self.loaded.borrow().loop_.is_some();
        if active {
            self.reloop(); // with a loop active RELOOP exits it
        }
    }
}

impl Deck {
    /// The declaration of the current mode (`DeckView.pad_mode` counted from the first plugin mode).
    fn plugin_decl(&self) -> Option<&'static PadModeDecl> {
        let i = usize::try_from(self.view.borrow().pad_mode).ok()?;
        registry()
            .pad_modes()
            .nth(i.checked_sub(PadMode::ALL.len())?)
    }

    /// A pad of a plugin mode went down: remembered for [`Deck::plugin_release`], then its `press` steps run.
    pub(super) fn plugin_pad(&self, pad: usize) {
        if let Some(m) = self.plugin_decl() {
            self.plugin_held.set(Some((m, pad)));
            run(&m.press, pad, self);
        }
    }

    /// The pad let go: the `release` steps of the mode it was pressed in (nothing for a pad of another mode).
    pub(in crate::app::deck) fn plugin_release(&self) {
        if let Some((m, pad)) = self.plugin_held.take() {
            run(&m.release, pad, self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::plugin::Manifest;
    use std::cell::{Cell, RefCell};

    /// A deck's play head with hot cues, as far as the steps can tell. `log` records what the steps asked for.
    #[derive(Default)]
    struct Sim {
        cues: [Option<u32>; 8],
        pos: Cell<u32>,
        playing: Cell<bool>,
        log: RefCell<Vec<&'static str>>,
    }

    impl Steps for Sim {
        fn jump(&self, pad: usize) {
            if let Some(ms) = self.cues[pad] {
                self.pos.set(ms);
            }
        }
        fn play(&self) {
            self.playing.set(true);
        }
        fn pause(&self) {
            self.playing.set(false);
        }
        fn beat_jump(&self, _: f64) {
            self.log.borrow_mut().push("beat_jump");
        }
        fn beat_loop(&self, _: f64) {
            self.log.borrow_mut().push("loop");
        }
        fn loop_off(&self) {
            self.log.borrow_mut().push("loop_off");
        }
    }

    /// The GATE CUE pad mode of the plugins repository's `gate-cue` (its declaration, inline so the core tests alone).
    fn gate_cue() -> PadModeDecl {
        let m: Manifest = serde_json::from_str(
            r#"{"name": "gate-cue", "version": "0.1.0", "pad_modes": [
                {"name": "GATE CUE", "press": ["jump", "play"], "release": ["pause", "jump"]}]}"#,
        )
        .unwrap();
        m.pad_modes.into_iter().next().unwrap()
    }

    #[test]
    fn gate_cue_plays_from_the_cue_while_held_and_returns_paused() {
        let gate = gate_cue();
        for was_playing in [false, true] {
            let deck = Sim {
                cues: [None, Some(5000), None, None, None, None, None, None],
                ..Default::default()
            };
            deck.pos.set(1200);
            deck.playing.set(was_playing);
            run(&gate.press, 1, &deck);
            assert_eq!((deck.pos.get(), deck.playing.get()), (5000, true));
            deck.pos.set(7300); // played on while held
            run(&gate.release, 1, &deck);
            assert_eq!((deck.pos.get(), deck.playing.get()), (5000, false));
        }
    }

    #[test]
    fn gate_cue_on_a_pad_without_a_hot_cue_does_not_move_the_deck() {
        let gate = gate_cue();
        let deck = Sim::default();
        deck.pos.set(1200);
        run(&gate.press, 0, &deck);
        run(&gate.release, 0, &deck);
        assert_eq!((deck.pos.get(), deck.playing.get()), (1200, false));
    }

    #[test]
    fn steps_run_in_order() {
        let deck = Sim::default();
        run(
            &[
                PadStep::BeatJump(-4.0),
                PadStep::Loop(0.5),
                PadStep::LoopOff,
            ],
            0,
            &deck,
        );
        assert_eq!(*deck.log.borrow(), ["beat_jump", "loop", "loop_off"]);
    }

    #[test]
    fn plugin_modes_come_after_the_built_ins() {
        let n = PadMode::ALL.len() as i32;
        assert_eq!(PadMode::count_in(0), n);
        assert_eq!(PadMode::count_in(2), n + 2);
        let at = |i| PadMode::from_index_in(i, 2);
        assert_eq!(
            [at(n - 1), at(n), at(n + 1), at(n + 2)],
            [
                PadMode::Stems,
                PadMode::Plugin,
                PadMode::Plugin,
                PadMode::HotCue
            ]
        );
        assert_eq!(PadMode::from_index_in(n, 0), PadMode::HotCue);
    }
}
