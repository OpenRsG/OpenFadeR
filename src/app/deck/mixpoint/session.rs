//! The MIX POINT LINK session: which deck is selected as MIX OUT / MIX IN, their MIX POINTs, the links
//! the LINK button made and SILENT. Pure state, no decks and no Slint ([`super::Mix`] applies it).

use crate::app::deck::DECKS;

/// MIX IN / MIX OUT selection mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    In,
    Out,
}

impl Mode {
    pub fn other(self) -> Self {
        match self {
            Mode::In => Mode::Out,
            Mode::Out => Mode::In,
        }
    }
}

/// The LINK button: MIX POINT not set / set / set and automatic playback set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    NotSet = 0,
    Set = 1,
    Linked = 2,
}

/// One deck's button: no MIX POINT, MIX OUT set, MIX IN set, MIX OUT set + linked, MIX IN set + linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckState {
    None = 0,
    Out = 1,
    In = 2,
    OutLinked = 3,
    InLinked = 4,
}

#[derive(Default)]
pub struct Session {
    pub mode: Mode,
    /// The decks selected as MIX OUT / MIX IN.
    pub out: Option<usize>,
    pub inn: Option<usize>,
    /// MIX POINT (track ms) of a deck as MIX OUT / as MIX IN.
    pub out_ms: [Option<u32>; DECKS],
    pub in_ms: [Option<u32>; DECKS],
    /// MIX OUT deck → the MIX IN deck that starts by itself when it gets to its point.
    pub link: [Option<usize>; DECKS],
    /// A link that has started its IN deck; it fires again only after the OUT deck went back before the start.
    pub fired: [bool; DECKS],
    /// SILENT of a MIX IN deck and where the silence ends (`None` = at its MIX POINT).
    pub silent: [bool; DECKS],
    pub silent_end: [Option<u32>; DECKS],
    /// SILENT EDIT: a click on the IN deck's waveform moves its silent end.
    pub silent_edit: bool,
}

impl Session {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// The deck selected in the current mode.
    pub fn selected(&self) -> Option<usize> {
        match self.mode {
            Mode::In => self.inn,
            Mode::Out => self.out,
        }
    }

    /// Makes `deck` the MIX OUT / MIX IN deck of the current mode (a deck is never both).
    pub fn select(&mut self, deck: usize) {
        match self.mode {
            Mode::In => {
                self.inn = Some(deck);
                if self.out == Some(deck) {
                    self.out = None;
                }
            }
            Mode::Out => {
                self.out = Some(deck);
                if self.inn == Some(deck) {
                    self.inn = None;
                }
            }
        }
    }

    /// The MIX POINT of the current mode on `deck` (which becomes the selected one); `auto_switch` then flips the mode
    /// ("Automatically switches between MIX IN/MIX OUT").
    pub fn set_point(&mut self, deck: usize, ms: u32, auto_switch: bool) {
        self.select(deck);
        match self.mode {
            Mode::In => self.in_ms[deck] = Some(ms),
            Mode::Out => self.out_ms[deck] = Some(ms),
        }
        if auto_switch {
            self.mode = self.mode.other();
        }
    }

    /// The MIX POINT of `mode` on `deck`.
    pub fn point(&self, deck: usize, mode: Mode) -> Option<u32> {
        match mode {
            Mode::In => self.in_ms[deck],
            Mode::Out => self.out_ms[deck],
        }
    }

    /// The silent end of a MIX IN deck with SILENT on.
    pub fn silent_until(&self, deck: usize) -> Option<u32> {
        self.silent[deck]
            .then(|| self.silent_end[deck].or(self.in_ms[deck]))
            .flatten()
    }

    /// Clears the MIX POINT of `mode` on `deck` and the links that stood on it.
    pub fn clear(&mut self, deck: usize, mode: Mode) {
        match mode {
            Mode::Out => {
                self.out_ms[deck] = None;
                self.link[deck] = None;
                self.fired[deck] = false;
            }
            Mode::In => {
                self.in_ms[deck] = None;
                self.silent_end[deck] = None;
                for o in 0..DECKS {
                    if self.link[o] == Some(deck) {
                        self.link[o] = None;
                        self.fired[o] = false;
                    }
                }
            }
        }
    }

    /// A new track on `deck`: its points, links and SILENT go.
    pub fn forget(&mut self, deck: usize) {
        self.clear(deck, Mode::Out);
        self.clear(deck, Mode::In);
        self.silent[deck] = false;
        if self.out == Some(deck) {
            self.out = None;
        }
        if self.inn == Some(deck) {
            self.inn = None;
        }
    }

    /// The pair the LINK button works on: the selected MIX OUT and MIX IN decks with both points set.
    pub fn pair(&self) -> Option<(usize, usize)> {
        let (o, i) = (self.out?, self.inn?);
        (self.out_ms[o].is_some() && self.in_ms[i].is_some()).then_some((o, i))
    }

    pub fn link_state(&self) -> LinkState {
        match self.pair() {
            None => LinkState::NotSet,
            Some((o, i)) if self.link[o] == Some(i) => LinkState::Linked,
            Some(_) => LinkState::Set,
        }
    }

    /// The LINK button: links the pair (again, when it is already linked: the caller puts a running IN deck back in step).
    /// Returns the pair, `None` while a MIX POINT is missing.
    pub fn press_link(&mut self) -> Option<(usize, usize)> {
        let (o, i) = self.pair()?;
        self.link[o] = Some(i);
        self.fired[o] = false;
        Some((o, i))
    }

    /// `deck` is a linked MIX OUT deck or the MIX IN deck of a link.
    pub fn linked(&self, deck: usize) -> bool {
        self.link[deck].is_some() || self.link.contains(&Some(deck))
    }

    pub fn deck_state(&self, d: usize) -> DeckState {
        let in_linked = self.link.contains(&Some(d));
        match (self.out_ms[d].is_some(), self.in_ms[d].is_some()) {
            (true, _) if self.link[d].is_some() => DeckState::OutLinked,
            (_, true) if in_linked => DeckState::InLinked,
            (true, _) if self.mode == Mode::Out => DeckState::Out,
            (_, true) => DeckState::In,
            (true, _) => DeckState::Out,
            _ => DeckState::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_selects_its_deck_and_a_deck_is_never_both_in_and_out() {
        let mut s = Session::new(Mode::Out);
        s.set_point(0, 90_000, false);
        assert_eq!((s.out, s.out_ms[0]), (Some(0), Some(90_000)));
        s.mode = Mode::In;
        s.set_point(0, 5_000, false);
        assert_eq!(
            (s.inn, s.out),
            (Some(0), None),
            "deck 0 moved over to MIX IN"
        );
    }

    #[test]
    fn auto_switch_flips_the_mode_after_each_point() {
        let mut s = Session::new(Mode::Out);
        s.set_point(0, 1, true);
        assert_eq!(s.mode, Mode::In);
        s.set_point(1, 2, true);
        assert_eq!(s.mode, Mode::Out);
        s.set_point(0, 3, false);
        assert_eq!(s.mode, Mode::Out);
    }

    #[test]
    fn the_link_button_needs_both_points_and_shows_three_states() {
        let mut s = Session::new(Mode::Out);
        assert_eq!(s.link_state(), LinkState::NotSet);
        s.set_point(0, 90_000, false);
        s.mode = Mode::In;
        s.set_point(1, 8_000, false);
        assert_eq!(s.link_state(), LinkState::Set);
        assert_eq!(s.press_link(), Some((0, 1)));
        assert_eq!(s.link_state(), LinkState::Linked);
        assert_eq!(s.press_link(), Some((0, 1)), "pressed again: re-linked");
        s.fired[0] = true;
        assert_eq!(s.press_link(), Some((0, 1)));
        assert!(!s.fired[0], "a new press arms it again");
    }

    #[test]
    fn clearing_a_point_drops_the_link_that_stood_on_it() {
        let mut s = Session::new(Mode::Out);
        s.set_point(0, 90_000, false);
        s.mode = Mode::In;
        s.set_point(1, 8_000, false);
        s.press_link();
        s.clear(1, Mode::In);
        assert_eq!((s.link[0], s.link_state()), (None, LinkState::NotSet));
        s.press_link();
        assert_eq!(s.link[0], None, "no MIX IN point: no link");
        s.set_point(1, 9_000, false);
        s.press_link();
        s.clear(0, Mode::Out);
        assert_eq!(s.link[0], None);
    }

    #[test]
    fn deck_buttons_show_set_and_linked() {
        let mut s = Session::new(Mode::Out);
        assert_eq!(s.deck_state(0), DeckState::None);
        s.set_point(0, 90_000, false);
        assert_eq!(s.deck_state(0), DeckState::Out);
        s.mode = Mode::In;
        s.set_point(1, 8_000, false);
        assert_eq!(s.deck_state(1), DeckState::In);
        s.press_link();
        assert_eq!(
            (s.deck_state(0), s.deck_state(1)),
            (DeckState::OutLinked, DeckState::InLinked)
        );
    }

    #[test]
    fn silent_runs_to_the_in_point_unless_edited() {
        let mut s = Session::new(Mode::In);
        s.set_point(1, 8_000, false);
        assert_eq!(s.silent_until(1), None, "SILENT off");
        s.silent[1] = true;
        assert_eq!(s.silent_until(1), Some(8_000));
        s.silent_end[1] = Some(2_000);
        assert_eq!(s.silent_until(1), Some(2_000));
        s.clear(1, Mode::In);
        assert_eq!(
            s.silent_until(1),
            None,
            "no point, nothing to be silent for"
        );
    }

    #[test]
    fn a_new_track_wipes_the_deck() {
        let mut s = Session::new(Mode::Out);
        s.set_point(0, 90_000, false);
        s.mode = Mode::In;
        s.set_point(1, 8_000, false);
        s.silent[1] = true;
        s.press_link();
        s.forget(1);
        assert_eq!((s.inn, s.link[0], s.silent[1]), (None, None, false));
        assert_eq!(s.out_ms[0], Some(90_000), "the other deck keeps its point");
    }
}
