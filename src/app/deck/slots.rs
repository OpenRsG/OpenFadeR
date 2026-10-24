//! The slots of the layout and the decks they show. A 2-deck layout draws two slots (left, right); the controller's
//! DECK 3 / 4 layers put deck C / D in the slot of their side without widening the layout (`DeckState.slots` tells the
//! UI which deck each slot draws, so every part of a slot follows). In the 4-deck layout a slot is its deck.
//!
//! Decks C and D leave a 2-deck layout paused. Once the controller reaches them there ([`reach`]) all four decks stay
//! in use ([`super::active`]) until the layout changes again.

use super::{shown, App, DeckState};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::Relaxed};

/// The controller reached decks C / D while the layout shows two decks.
static REACHED: AtomicBool = AtomicBool::new(false);
/// Per side the selected layer: 0 = A / B, 1 = C / D ([`crate::app::midi::layer::Layer`]).
static LAYER: [AtomicU8; 2] = [AtomicU8::new(0), AtomicU8::new(0)];
/// The layout (decks shown) the last time anything looked.
static LAYOUT: AtomicUsize = AtomicUsize::new(0);

pub fn reached() -> bool {
    REACHED.load(Relaxed)
}

/// The deck each slot shows: the layer's deck on both sides while a 2-deck layout was reached by the controller, else
/// the slot's own deck. Slots 2 and 3 only exist in the 4-deck layout.
fn map(shown: usize, reached: bool, layer: [usize; 2]) -> [usize; 4] {
    let at = |s: usize| {
        s + if shown == 2 && reached {
            2 * layer[s].min(1)
        } else {
            0
        }
    };
    [at(0), at(1), 2, 3]
}

fn current() -> [usize; 4] {
    map(
        shown(),
        reached(),
        [0, 1].map(|s| usize::from(LAYER[s].load(Relaxed))),
    )
}

/// The deck slot `slot` shows (keyboard shortcuts of the first two decks follow the slots they stand for).
pub fn deck_in(slot: usize) -> usize {
    current().get(slot).copied().unwrap_or(slot)
}

/// Tells the UI which deck each slot draws.
pub fn publish(app: &App) {
    let slots: Vec<i32> = current().iter().map(|&d| d as i32).collect();
    app.global::<DeckState>()
        .set_slots(ModelRc::new(VecModel::from(slots)));
}

/// The layer the controller selected per side; `true` when a slot now shows another deck.
pub fn set_layer(layer: [usize; 2]) -> bool {
    let before = current();
    for (s, l) in layer.iter().enumerate() {
        LAYER[s].store(*l as u8, Relaxed);
    }
    before != current()
}

/// The controller reaches deck `d`: in a 2-deck layout that keeps all four decks in use; `true` when that is new.
pub fn reach(d: usize) -> bool {
    d >= shown() && !REACHED.swap(true, Relaxed)
}

/// The layout now shows `decks`: a change starts over (decks C / D out of use again in a 2-deck layout). Call from the
/// settings' change handler.
pub fn layout(app: &App, decks: usize) {
    if LAYOUT.swap(decks, Relaxed) != decks {
        REACHED.store(false, Relaxed);
        publish(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_deck_layout_shows_the_selected_layer_only_once_the_controller_reached_it() {
        assert_eq!(map(2, true, [1, 0]), [2, 1, 2, 3]);
        assert_eq!(map(2, true, [1, 1]), [2, 3, 2, 3]);
        assert_eq!(map(2, true, [0, 1]), [0, 3, 2, 3]);
        assert_eq!(map(2, true, [0, 0]), [0, 1, 2, 3]);
        assert_eq!(map(2, false, [1, 1]), [0, 1, 2, 3]);
    }

    #[test]
    fn the_four_deck_layout_shows_every_deck_in_its_own_slot() {
        for layer in [[0, 0], [1, 0], [0, 1], [1, 1]] {
            assert_eq!(map(4, true, layer), [0, 1, 2, 3]);
        }
    }
}
