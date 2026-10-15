//! The controller's deck layers. A 4-deck controller has two physical decks: the left
//! one is deck 1 or 3, the right one deck 2 or 4, whichever its DECK button selected. The controls of the selected
//! layer already arrive on that deck's own MIDI channel, so the bindings route straight
//! onto decks A to D; what the host must follow is the *selection*, for two reasons: the lights (the controller
//! shows the lights of the layer it is on, so only the selected deck of each side is sent to; a layer that has
//! just been selected is sent in full) and the status line. The selection arrives as the controller's deck-select
//! messages (read-only, one message per deck: the message of deck `d` = that deck's layer is now selected).

use crate::app::deck::DECK_NAMES;

/// Per side (left, right) the selected layer: 0 = deck 1 / 2 (A, B), 1 = deck 3 / 4 (C, D). `default()` = A and B.
pub type Layer = [usize; 2];

/// The decks the layout must show once deck `d` (0 = A … 3 = D) was selected on the controller with `shown` decks in
/// the layout (2 or 4): DECK 3 / 4 switch a 2-deck layout to 4 decks (the controller's DECK 3 / 4 layer must
/// reach decks C / D, which a 2-deck layout does not have); the controller's own DECK 1 / 2 never shrink it.
pub fn layout_for(d: usize, shown: usize) -> usize {
    if d < shown { shown } else { 4 }
}

/// The selection after deck `d` was selected on the controller, with `shown` decks in the layout (2 or 4).
/// A deck the layout hides cannot be selected: `None`, the selection stays ([`layout_for`] widens the layout first).
pub fn select(mut layer: Layer, d: usize, shown: usize) -> Option<Layer> {
    (d < shown.min(4)).then(|| {
        layer[d % 2] = d / 2;
        layer
    })
}

/// The other deck of `d`'s pair (A + C, B + D) when DUAL DECK is on for its side and the layout shows `shown` decks: the
/// deck whose controls `d`'s are copied to.
pub fn dual_partner(dual: [bool; 2], d: usize, shown: usize) -> Option<usize> {
    (dual[d % 2] && d ^ 2 < shown).then_some(d ^ 2)
}

/// Whether deck `d` is the selected one on its side: only that deck's lights reach the controller.
pub fn selected(layer: Layer, d: usize) -> bool {
    layer[d % 2] == d / 2
}

/// "left C, right B" for the status line.
pub fn describe(layer: Layer) -> String {
    let name = |side: usize| DECK_NAMES[side + 2 * layer[side].min(1)];
    format!("left {}, right {}", name(0), name(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_rows_three_and_four_take_over_their_side() {
        assert_eq!(
            select(Layer::default(), 2, 4),
            Some([1, 0]),
            "DECK 3 selects C on the left"
        );
        assert_eq!(select([1, 0], 3, 4), Some([1, 1]));
        assert_eq!(select([1, 1], 0, 4), Some([0, 1]), "back to deck 1");
        assert_eq!(select([1, 1], 1, 4), Some([1, 0]));
        assert_eq!(describe([1, 0]), "left C, right B");
        assert_eq!(describe(Layer::default()), "left A, right B");
    }

    #[test]
    fn a_hidden_deck_cannot_be_selected() {
        assert_eq!(
            select(Layer::default(), 2, 2),
            None,
            "the 2-deck layout has no deck C"
        );
        assert_eq!(select(Layer::default(), 3, 2), None);
        assert_eq!(select([1, 1], 1, 2), Some([1, 0]));
        assert_eq!(select(Layer::default(), 7, 4), None, "out of range");
    }

    #[test]
    fn deck_3_or_4_widens_the_two_deck_layout_and_nothing_shrinks_it() {
        for (d, expected) in [(2, [1, 0]), (3, [0, 1])] {
            assert_eq!(
                layout_for(d, 2),
                4,
                "deck {} needs the 4-deck layout",
                d + 1
            );
            assert_eq!(
                select(Layer::default(), d, layout_for(d, 2)),
                Some(expected)
            );
        }
        assert_eq!(
            [
                layout_for(0, 2),
                layout_for(1, 2),
                layout_for(3, 4),
                layout_for(0, 4)
            ],
            [2, 2, 4, 4]
        );
    }

    #[test]
    fn dual_deck_pairs_a_with_c_and_b_with_d_on_its_own_side() {
        let left = [true, false];
        assert_eq!(dual_partner(left, 0, 4), Some(2));
        assert_eq!(dual_partner(left, 2, 4), Some(0));
        assert_eq!(dual_partner(left, 1, 4), None, "the right side is not dual");
        assert_eq!(dual_partner([false, true], 3, 4), Some(1));
        assert_eq!(
            dual_partner(left, 0, 2),
            None,
            "deck C is not in a 2-deck layout"
        );
        assert_eq!(dual_partner([false; 2], 0, 4), None);
    }

    #[test]
    fn only_the_selected_deck_of_a_side_is_lit() {
        let on: Vec<bool> = (0..4).map(|d| selected([1, 0], d)).collect();
        assert_eq!(on, [false, true, true, false]);
        assert!((0..4).all(|d| selected(Layer::default(), d) == (d < 2)));
    }
}
