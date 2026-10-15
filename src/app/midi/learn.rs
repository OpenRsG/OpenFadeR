//! MIDI LEARN: the list of learnable actions, and binding the captured control to the one the user clicks.

use super::actions::{ACTIONS, Kind};
use super::{Binding, Midi, Outcome, describe, usermap};
use crate::app::MidiState;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

fn kind_tag(k: &Kind) -> &'static str {
    match k {
        Kind::Press(_)
        | Kind::Arg(_)
        | Kind::Deck(..)
        | Kind::Vinyl
        | Kind::Cutter
        | Kind::Layer
        | Kind::Dual => "button",
        Kind::Touch => "touch",
        Kind::Knob(_) => "knob",
        Kind::Jog { .. } | Kind::Rel { .. } => "jog",
    }
}

/// Fills the overlay's action list; row `i` is `ACTIONS[i]` (the learnable actions come first).
pub(super) fn offer_actions(state: &MidiState) {
    let labels: Vec<SharedString> = ACTIONS
        .iter()
        .filter(|a| a.learnable())
        .map(|a| format!("{}  ({})", a.name, kind_tag(&a.kind)).into())
        .collect();
    state.set_actions(ModelRc::new(VecModel::from(labels)));
}

impl Midi {
    /// Binds the captured control to action `act` on `deck`, and saves midi.map.
    pub(super) fn assign(&self, act: usize, deck: i32) {
        let (Some(key), Some(app)) = (self.captured.take(), self.app.upgrade()) else {
            return;
        };
        let state = app.global::<MidiState>();
        let saved = {
            let mut user = self.user.borrow_mut();
            user.retain(|b| (b.status, b.d1) != key);
            user.push(Binding {
                status: key.0,
                d1: key.1,
                act,
                deck,
                arg: 0,
                hires: false,
            });
            usermap::save(&user)
        };
        state.set_captured("".into());
        state.set_mapped(self.mapped() as i32);
        state.set_last(
            match saved {
                Ok(()) => format!("{} → {}", describe(key), Outcome::Done(act, deck)),
                Err(e) => format!("bound, but midi.map not saved: {e}"),
            }
            .into(),
        );
    }
}
