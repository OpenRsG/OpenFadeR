//! The control namespace a controller plugin sees (the table is in `docs/plugins.md`, Controller plugins): our own mapping actions
//! ([`ACTIONS`]) and some readable values, by key `name[/col[/arg]]`.
//!
//! `name` is an action name of the mapping files (`play`, `eq-low`, `crossfader`, `hw-fx-on`…) or a value (`playing`, `position`,
//! `bpm`…). `col` counts from 1: the deck, or the Beat FX unit / MERGE side the action's deck column names; actions with no deck
//! (`crossfader`, `master`, `browse`) take none. `arg` is the mapping row's constant of an action that has one (`hw-fx-on/1/2`: slot
//! 2 of FX unit 1; `hw-pad/1/<mode * 16 + slot>`: a pad of deck 1), as `docs/mappings.md` lists them.
//!
//! Setting takes a number by the action's kind: a button is pressed at 0.5 and up and released below (what a release does is the
//! action's, e.g. a hot cue pad hands back to the slip playhead), a knob or fader moves to 0 .. 1 (through soft takeover when the
//! plugin asked for it), an encoder or jog turns by that many signed ticks, the platter touch is down at 0.5 and up. Buttons toggle:
//! `play` pressed is play / pause, so a plugin reads `playing` to know what a press did.

use super::super::actions::{Kind, ACTIONS};
use super::super::pickup::Pickup;
use super::super::{App, Binding, Ev, Midi};
use super::State;
use crate::app::{
    DeckState, FxState, MixerState,
    deck::{DECKS, active},
};
use slint::{ComponentHandle, Model};
use std::collections::HashMap;
use std::sync::LazyLock;

/// A parsed key.
#[derive(Debug, PartialEq)]
pub(super) struct Key<'a> {
    pub name: &'a str,
    /// 0-based deck / unit (0 when the key names none).
    pub col: i32,
    pub arg: i32,
}

impl<'a> Key<'a> {
    pub fn parse(key: &'a str) -> Option<Key<'a>> {
        let mut parts = key.split('/');
        let name = parts.next().filter(|n| !n.is_empty())?;
        let col = match parts.next() {
            None => 0,
            Some(c) => {
                c.parse::<i32>()
                    .ok()
                    .filter(|c| (1..=DECKS as i32).contains(c))?
                    - 1
            }
        };
        let arg = match parts.next() {
            None => 0,
            Some(a) => a.parse().ok()?,
        };
        parts.next().is_none().then_some(Key { name, col, arg })
    }
}

/// Index of every action by name.
fn index() -> &'static HashMap<&'static str, usize> {
    static INDEX: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
        ACTIONS
            .iter()
            .enumerate()
            .map(|(i, a)| (a.name, i))
            .collect()
    });
    &INDEX
}

impl Midi {
    /// The value of control `key`, NaN when there is none (or its deck is not in the layout).
    pub(super) fn ctl_get(&self, key: &str) -> f64 {
        let (Some(k), Some(app)) = (Key::parse(key), self.app.upgrade()) else {
            return f64::NAN;
        };
        if k.col as usize >= active() {
            return f64::NAN;
        }
        self.value(&app, &k)
            .or_else(|| {
                let read = ACTIONS[*index().get(k.name)?].read?;
                read(&app, k.col).map(f64::from)
            })
            .unwrap_or(f64::NAN)
    }

    /// What a deck shows: the states and numbers that are no knob position.
    fn value(&self, app: &App, k: &Key) -> Option<f64> {
        let d = k.col as usize;
        let deck = || app.global::<DeckState>().get_decks().row_data(d);
        let channel = || app.global::<MixerState>().get_channels().row_data(d);
        let unit = || app.global::<FxState>().get_units().row_data(d);
        let on = |b: bool| Some(f64::from(b));
        match k.name {
            "loaded" => on(deck()?.loaded),
            "play" | "playing" => on(deck()?.playing),
            "cue" | "at-cue" => on(deck()?.at_cue),
            "sync" | "synced" => on(deck()?.synced),
            "deck-master" => on(deck()?.master),
            "reloop" | "loop-active" => on(deck()?.loop_active),
            "loop-beats" => Some(f64::from(deck()?.loop_beats)),
            "key-lock" => on(deck()?.key_lock),
            "quantize" => on(deck()?.quantize),
            "slip" => on(deck()?.slip),
            "reverse" => on(deck()?.reverse),
            "pad-mode" => Some(f64::from(deck()?.pad_mode)),
            "bpm" => Some(f64::from(deck()?.bpm)),
            "cue-monitor" => on(channel()?.cue),
            "level" => Some(f64::from(channel()?.level)),
            "vinyl" => on(self.vinyl.get()[d]),
            "jog-cutter" => on(self.cutter.get()[d]),
            "position" => self.decks[d].with_player(|p| {
                let len = f64::from(p.duration_ms());
                if len > 0.0 {
                    p.position_ms() / len
                } else {
                    0.0
                }
            }),
            "position-ms" => self.decks[d].with_player(|p| p.position_ms()),
            "duration-ms" => self.decks[d].with_player(|p| f64::from(p.duration_ms())),
            "rate" => self.decks[d].with_player(|p| p.rate()),
            "fx-on" => on(unit()?.slots.row_data(k.arg.max(0) as usize)?.on),
            "fx-assign" => Some(f64::from(unit()?.assign)),
            n => {
                // padN: the hot cue pad is set (1 to 8)
                let pad = n
                    .strip_prefix("pad")?
                    .parse::<usize>()
                    .ok()?
                    .checked_sub(1)?;
                on(deck()?.pads.row_data(pad)?.set)
            }
        }
    }

    /// Sets control `key` to `v` for a plugin; `st` owns its holds and pickup table.
    pub(super) fn ctl_set(&self, st: &State, takeover: bool, key: &str, v: f64) -> bool {
        let (Some(k), Some(app)) = (Key::parse(key), self.app.upgrade()) else {
            return false;
        };
        let act = index().get(k.name).copied();
        let down = v >= 0.5;
        if let Some(act) = act.filter(|&i| ACTIONS[i].momentary()) {
            let held = {
                let mut held = st.held.borrow_mut();
                if down {
                    held.get(&(act, k.col, k.arg)).copied()
                } else {
                    held.remove(&(act, k.col, k.arg))
                }
            };
            if let Some(held) = held {
                if !down {
                    self.release_held(&app, held);
                }
                return true;
            }
        }
        let d = k.col as usize;
        if d >= active() {
            return false;
        }
        match k.name {
            "position" => {
                self.decks[d].seek_frac(v.clamp(0.0, 1.0) as f32);
                return true;
            }
            "position-ms" => {
                self.decks[d].seek_ms(v);
                return true;
            }
            _ => {}
        }
        let Some(act) = act else {
            return false;
        };
        let b = Binding {
            status: 0,
            d1: 0,
            act,
            deck: k.col,
            arg: k.arg,
            hires: false,
        };
        match ACTIONS[act].kind {
            // The controller's own layer keys: a plugin has no layers, it addresses decks directly.
            Kind::Layer | Kind::Dual => return false,
            Kind::Knob(_) => {
                let slot = takeover.then(|| Pickup::plugin_slot(act, k.col));
                self.absolute_in(&st.pick, slot, &app, &b, v.clamp(0.0, 1.0) as f32);
            }
            Kind::Rel { f, .. } => f(&app, k.col, v.round() as i32),
            Kind::Jog { mode, .. } => {
                for d in self.gang(&ACTIONS[act], d) {
                    self.turn(d, mode, v.round() as i32);
                }
            }
            Kind::Vinyl => self.set_vinyl(d, down),
            Kind::Cutter => self.set_cutter(d, down),
            Kind::Press(_) | Kind::Arg(_) | Kind::Deck(..) | Kind::Touch => {
                let ev = Ev {
                    status: if down { 0x90 } else { 0x80 },
                    d1: 0,
                    d2: if down { 127 } else { 0 },
                };
                let momentary = ACTIONS[act].momentary();
                if !momentary || down {
                    let held = self.prepare_hold(&app, &b);
                    if momentary {
                        st.held.borrow_mut().insert((act, k.col, k.arg), held);
                    }
                    self.fire_recipients(&app, ev, held);
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_a_name_with_an_optional_deck_and_argument() {
        let k = |name, col, arg| Some(Key { name, col, arg });
        assert_eq!(Key::parse("crossfader"), k("crossfader", 0, 0));
        assert_eq!(Key::parse("play/2"), k("play", 1, 0));
        assert_eq!(Key::parse("hw-fx-on/1/2"), k("hw-fx-on", 0, 2));
        for bad in [
            "",
            "/1",
            "play/0",
            "play/5",
            "play/x",
            "play/1/x",
            "play/1/2/3",
        ] {
            assert_eq!(Key::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_action_has_one_name() {
        assert_eq!(index().len(), ACTIONS.len());
    }
}
