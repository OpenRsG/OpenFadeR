//! The action table a MIDI control can be bound to. Each action calls the same `DeckState` /
//! `MixerState` / `FxState` / `BrowserState` callback the on-screen control or keyboard shortcut
//! (keys.rs) uses, so a controller never has a code path of its own.
//! Knob positions are 0..1 and use the same knob indices as ui/mixer.slint (TRIM, HI, MID, LOW, FILTER).
//!
//! [`ACTIONS`] is [`learn::TABLE`] followed by [`mapped::TABLE`]: MIDI LEARN's list row `i` is `ACTIONS[i]`, so the
//! learnable actions come first. The tail (`Kind::Arg`, `Kind::Deck`, `Kind::Layer`) is only reachable through a
//! mapping file ([`super::mapfile`]), which supplies a constant (pad slot, pad mode…) per row.

mod learn;
mod mapped;

use crate::app::{App, deck::Deck};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JogMode {
    /// Platter scratch while touched (vinyl mode), pitch bend otherwise.
    Scratch,
    /// Outer ring / jog without touch: temporary pitch bend.
    Bend,
    /// Shift + jog: fast search.
    Search,
    /// JOG CUTTER rotation: gates the channel while the platter turns.
    Cut,
}

#[derive(Clone, Copy)]
pub enum Kind {
    /// Fires on note-on / CC ≥ 64 with the target deck (0 = A).
    Press(fn(&App, i32)),
    /// Absolute CC 0..127 → 0..1 (14-bit pairs when the binding is `hires`); the deck is the channel for mixer knobs.
    Knob(fn(&App, i32, f32)),
    /// Relative encoder → signed ticks; `two_comp` = two's complement (1 = +1, 127 = −1), else centred on 64.
    Rel {
        two_comp: bool,
        f: fn(&App, i32, i32),
    },
    Jog {
        two_comp: bool,
        mode: JogMode,
    },
    /// Platter touch: note-on touches, note-off releases ([`super::jog`]).
    Touch,
    /// Vinyl mode on/off (touch scratches only while it is on).
    Vinyl,
    /// JOG CUTTER mode on/off.
    Cutter,
    /// Mapping-file rows only: press with the row's constant.
    Arg(fn(&App, i32, i32)),
    /// Mapping-file rows only: straight onto the deck (Shift layers etc.), with a release hook that gets the row's constant too.
    Deck(fn(&Deck, i32), Option<fn(&Deck, i32)>),
    /// Mapping-file rows: the controller selected the deck of the row ([`super::layer`]).
    Layer,
    /// Mapping-file rows only: SHIFT + DECK select, DUAL DECK of the side in the row's constant (0 left, 1 right) on / off.
    Dual,
}

#[derive(Clone, Copy)]
pub struct Action {
    pub name: &'static str,
    pub kind: Kind,
    /// Note-off / release hook for `Press` / `Arg` (deck, arg): hot-cue pads hand back to the slip playhead.
    pub release: Option<fn(&App, i32, i32)>,
    /// `Knob` only: the control's value on screen (deck), 0..1 as the knob sends it; `None` = no value to compare
    /// (the deck's tempo while SYNC drives it). An action without one is applied as sent ([`super::pickup`]).
    pub read: Option<fn(&App, i32) -> Option<f32>>,
}

impl Action {
    /// Offered by MIDI LEARN and accepted in midi.map.
    pub fn learnable(&self) -> bool {
        !matches!(
            self.kind,
            Kind::Arg(_) | Kind::Deck(..) | Kind::Layer | Kind::Dual
        )
    }

    const fn new(name: &'static str, kind: Kind) -> Action {
        Action {
            name,
            kind,
            release: None,
            read: None,
        }
    }

    const fn released_by(self, release: fn(&App, i32, i32)) -> Action {
        Action {
            release: Some(release),
            ..self
        }
    }

    const fn read_by(self, read: fn(&App, i32) -> Option<f32>) -> Action {
        Action {
            read: Some(read),
            ..self
        }
    }
}

const fn press(name: &'static str, f: fn(&App, i32)) -> Action {
    Action::new(name, Kind::Press(f))
}
const fn knob(name: &'static str, f: fn(&App, i32, f32)) -> Action {
    Action::new(name, Kind::Knob(f))
}
const fn arg(
    name: &'static str,
    f: fn(&App, i32, i32),
    release: Option<fn(&App, i32, i32)>,
) -> Action {
    Action {
        release,
        ..Action::new(name, Kind::Arg(f))
    }
}
const fn on_deck(name: &'static str, f: fn(&Deck, i32), release: Option<fn(&Deck, i32)>) -> Action {
    Action::new(name, Kind::Deck(f, release))
}
const fn rel(name: &'static str, two_comp: bool, f: fn(&App, i32, i32)) -> Action {
    Action::new(name, Kind::Rel { two_comp, f })
}
const fn jog(name: &'static str, two_comp: bool, mode: JogMode) -> Action {
    Action::new(name, Kind::Jog { two_comp, mode })
}

const fn concat<const N: usize>(parts: [&[Action]; 2]) -> [Action; N] {
    let mut out = [const { Action::new("", Kind::Touch) }; N];
    let (mut i, mut p) = (0, 0);
    while p < parts.len() {
        let mut j = 0;
        while j < parts[p].len() {
            out[i] = parts[p][j];
            (i, j) = (i + 1, j + 1);
        }
        p += 1;
    }
    assert!(i == N);
    out
}

pub static ACTIONS: [Action; learn::TABLE.len() + mapped::TABLE.len()] =
    concat([learn::TABLE, mapped::TABLE]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_are_unique() {
        let mut n: Vec<_> = ACTIONS.iter().map(|a| a.name).collect();
        n.sort();
        n.dedup();
        assert_eq!(n.len(), ACTIONS.len());
    }

    #[test]
    fn learnable_actions_come_first() {
        assert!(learn::TABLE.iter().all(Action::learnable));
        assert!(mapped::TABLE.iter().all(|a| !a.learnable()));
    }
}
