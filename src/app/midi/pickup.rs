//! Soft takeover (pickup) of absolute controls. A knob or fader ([`Kind::Knob`], CC 0..127 or 14-bit) whose value on screen
//! changed without it (a layer or STEM ISO switch, an edit with the mouse, a load, a remap, SYNC letting go of the tempo) would
//! make the control jump on its next message. It is ignored instead until its own value crosses the one on screen, or comes
//! within [`TOLERANCE`] of it; from then on it drives the control directly again, until the value on screen changes by itself.
//! While it waits and is moved, a toast says which way to turn it. PREFERENCES > CONTROLLER > Others > Soft takeover switches
//! all of it off; the state dump of a connecting controller ([`super::dump`]) always applies, it is how the screen learns where
//! the controls are. Relative encoders, buttons and jog wheels never come here, nor do actions without a reader
//! ([`Action::read`]) or controls that do not send CC.
//!
//! One fixed table slot per CC and channel, made once: a message allocates nothing.

use super::actions::{ACTIONS, Action, Kind};
use super::{App, Binding, Midi, Outcome};
use crate::app::{deck::DECK_NAMES, prefs, toast};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

/// How close (0..1, about two steps of a 7-bit control) a waiting control must come to the value on screen to take over.
const TOLERANCE: f32 = 0.02;
/// The value on screen that differs from the one this control left behind by more than this was changed by someone else.
const MOVED: f32 = 0.001;
/// The same hint comes again after this while the control is still being moved (the toast stays 3 s after the last one).
const HINT_EVERY: Duration = Duration::from_secs(1);
/// CC numbers per MIDI channel, and channels.
const SLOTS: usize = 16 * 128;

/// What one CC knows about its control.
#[derive(Clone, Copy)]
struct Slot {
    /// The (action, deck) it drove last: another one (a remap) starts the wait over.
    target: (u16, u8),
    /// The value it sent last, and the value on screen right after its last write.
    hw: f32,
    soft: f32,
    /// It drives its control directly.
    live: bool,
}

const FRESH: Slot = Slot {
    target: (u16::MAX, 0),
    hw: f32::NAN,
    soft: f32::NAN,
    live: false,
};

pub(super) struct Pickup {
    slots: Box<[Cell<Slot>]>,
    /// The last hint shown: slot, direction, when.
    hint: Cell<Option<(usize, bool, Instant)>>,
}

impl Pickup {
    /// The slot of a plugin's control `act` (index into the action table) of deck or unit `deck`: at most [`SLOTS`] distinct.
    pub(super) fn plugin_slot(act: usize, deck: i32) -> usize {
        (act * 8 + deck.clamp(0, 7) as usize) % SLOTS
    }

    pub(super) fn new() -> Pickup {
        Pickup {
            slots: vec![Cell::new(FRESH); SLOTS].into(),
            hint: Cell::new(None),
        }
    }

    /// The slot of control `key` (status, CC number); `None` unless it is a CC.
    fn slot(key: (u8, u8)) -> Option<usize> {
        (key.0 & 0xF0 == 0xB0).then(|| (key.0 & 0xF) as usize * 128 + (key.1 & 0x7F) as usize)
    }

    /// Control `slot` of `target` now sends `hw` while the value on screen is `soft`: `Some(up)` = it has to wait, and `up` is
    /// whether it has to be turned up to get there.
    fn check(&self, slot: usize, target: (usize, i32), hw: f32, soft: f32) -> Option<bool> {
        let cell = &self.slots[slot];
        let mut s = cell.get();
        let target = (target.0 as u16, target.1 as u8);
        let before = if s.target == target {
            s.hw
        } else {
            (s.target, s.live) = (target, false);
            f32::NAN
        };
        s.hw = hw;
        if s.live && (soft - s.soft).abs() > MOVED {
            s.live = false;
        }
        let arrived = (hw - soft).abs() <= TOLERANCE || (before - soft) * (hw - soft) <= 0.0;
        cell.set(s);
        (!s.live && !arrived).then_some(hw < soft)
    }

    /// The control's value was applied and the screen shows `soft` now: it drives the control from here.
    fn landed(&self, slot: usize, soft: f32) {
        let cell = &self.slots[slot];
        cell.set(Slot {
            soft,
            live: true,
            ..cell.get()
        });
    }

    /// Is the hint for `slot` due: another control or direction than the last one, or [`HINT_EVERY`] since it.
    fn announce(&self, slot: usize, up: bool, now: Instant) -> bool {
        let due = self
            .hint
            .get()
            .is_none_or(|(s, u, at)| (s, u) != (slot, up) || now.duration_since(at) >= HINT_EVERY);
        if due {
            self.hint.set(Some((slot, up, now)));
        }
        due
    }
}

impl Midi {
    /// Applies the absolute value `v` (0..1) of knob binding `b`, unless the control has to wait for pickup (`Some(Waiting)`).
    /// `key` names the control: for a 14-bit pair its MSB.
    pub(super) fn absolute(&self, app: &App, key: (u8, u8), b: &Binding, v: f32) -> Option<Outcome> {
        self.absolute_in(&self.pickup, Pickup::slot(key), app, b, v)
    }

    /// [`Midi::absolute`] with the soft takeover state in slot `slot` of `pick` (`None`: the control is not tracked, it is
    /// applied as sent). A controller plugin's controls have a table of their own ([`Pickup::plugin_slot`]).
    pub(super) fn absolute_in(
        &self,
        pick: &Pickup,
        slot: Option<usize>,
        app: &App,
        b: &Binding,
        v: f32,
    ) -> Option<Outcome> {
        let act = &ACTIONS[b.act];
        let Kind::Knob(set) = act.kind else {
            return None;
        };
        let tracked = slot.zip(act.read);
        if let Some((slot, read)) = tracked
            && let Some(soft) = read(app, b.deck)
            && let Some(up) = pick.check(slot, (b.act, b.deck), v, soft)
            && prefs::with(|p| p.controller.soft_takeover)
            && !self.dump.get().is_some_and(|w| w.open(Instant::now()))
        {
            if pick.announce(slot, up, Instant::now()) {
                toast::show(app, &hint(act, b.deck, up));
            }
            return Some(Outcome::Waiting(b.act, b.deck));
        }
        set(app, b.deck, v);
        if let Some((slot, read)) = tracked
            && let Some(soft) = read(app, b.deck)
        {
            pick.landed(slot, soft);
        }
        None
    }
}

/// "eq-low A: turn up ▲ to pick up".
fn hint(act: &Action, deck: i32, up: bool) -> String {
    let name = format!("{} {}", act.name, DECK_NAMES[deck as usize]);
    if up {
        tr!("{}: turn up ▲ to pick up", name)
    } else {
        tr!("{}: turn down ▼ to pick up", name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::set_for_test;
    use openfader::{
        library::{Library, own::Own},
        store::Store,
    };
    use slint::{ComponentHandle, Model};
    use std::rc::Rc;

    #[test]
    fn a_knob_waits_until_it_reaches_the_value_on_screen() {
        crate::app::headless::init();
        let app = App::new().unwrap();
        let lib: Rc<dyn Library> =
            Rc::new(Own::new(std::sync::Arc::new(Store::in_memory().unwrap())));
        let decks = crate::app::deck::wire(&app, lib, Default::default());
        crate::app::mixer::wire(&app, decks.clone());
        let guard = super::super::wire(&app, decks);
        let m = &guard.midi;
        let index = |name| ACTIONS.iter().position(|a| a.name == name).unwrap();
        let bind = |d1, act| Binding {
            status: 0xB0,
            d1,
            act: index(act),
            deck: 0,
            arg: 0,
            hires: false,
        };
        *m.user.borrow_mut() = vec![bind(1, "eq-low"), bind(2, "browse")];
        let low = || {
            app.global::<crate::app::MixerState>()
                .get_channels()
                .row_data(0)
                .unwrap()
                .knobs
                .row_data(3)
                .unwrap()
        };
        let send = |d1, v| {
            m.dispatch(&app, super::super::decode([0xB0, d1, v]).unwrap());
        };
        let screen = |v: f32| {
            app.global::<crate::app::MixerState>().invoke_knob(0, 3, v);
        };
        set_for_test(|p| p.controller.soft_takeover = true);

        screen(38.0 / 127.0);
        send(1, 100);
        assert_eq!(low(), 38.0 / 127.0, "a knob at 100 against 38 is ignored");
        send(1, 60);
        assert_eq!(low(), 38.0 / 127.0, "still waiting on its way down");
        send(1, 30);
        assert_eq!(low(), 30.0 / 127.0, "it crossed 38 and takes over");
        send(1, 90);
        assert_eq!(low(), 90.0 / 127.0, "and drives the control directly");

        screen(0.2);
        send(1, 100);
        assert_eq!(low(), 0.2, "a value changed on screen makes it wait again");

        let rel = m.dispatch(&app, super::super::decode([0xB0, 2, 5]).unwrap());
        assert!(
            matches!(rel, Outcome::Done(..)),
            "a relative encoder is no knob and never waits"
        );

        set_for_test(|p| p.controller.soft_takeover = false);
        send(1, 100);
        assert_eq!(low(), 100.0 / 127.0, "with takeover off the knob jumps");
        set_for_test(|p| p.controller.soft_takeover = true);
    }
}
