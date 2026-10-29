//! One decoded message: captured by MIDI LEARN, taken as a state-dump report, or dispatched to its binding's action. Runs per
//! message on the UI thread; text is only allocated for logging / LEARN, and momentary presses retain ownership until release.

use super::actions::{ACTIONS, Action, Kind};
use super::{App, Binding, Ev, HeldAction, Midi, Outcome, combine14, describe, layer};
use crate::app::{
    DeckState, MidiState, SamplerState,
    deck::{DECK_NAMES, Deck, active, slots},
};
use slint::{ComponentHandle, Model};

impl Midi {
    pub(super) fn find(&self, key: (u8, u8)) -> Option<Binding> {
        self.user
            .borrow()
            .iter()
            .find(|b| (b.status, b.d1) == key)
            .copied()
            .or_else(|| self.file_binds.borrow().get(&key).copied())
    }

    /// Controls with a binding: the mapping file's plus the user's that do not replace one.
    pub(super) fn mapped(&self) -> usize {
        let file = self.file_binds.borrow();
        file.len()
            + self
                .user
                .borrow()
                .iter()
                .filter(|b| !file.contains_key(&(b.status, b.d1)))
                .count()
    }

    pub(super) fn handle(&self, ev: Ev) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let state = app.global::<MidiState>();
        let owned = self.owned(&app, ev);
        if owned.is_none() && state.get_visible() {
            if !ev.is_off() {
                self.captured.set(Some(ev.key()));
                state.set_captured(describe(ev.key()).into());
            }
            return;
        }
        let out = owned.unwrap_or_else(|| {
            self.dump_step(&app, ev)
                .unwrap_or_else(|| self.dispatch(&app, ev))
        });
        if self.log {
            let name = self
                .file_names
                .borrow()
                .get(&ev.key())
                .map_or(String::new(), |n| format!("  [{n}]"));
            eprintln!(
                "midi {:02X} {:02X} {:02X} -> {out}{name}",
                ev.status, ev.d1, ev.d2
            );
        }
    }

    pub(super) fn dispatch(&self, app: &App, ev: Ev) -> Outcome {
        if let Some(out) = self.owned(app, ev) {
            return out;
        }
        match self.find(ev.key()) {
            Some(b) => self.run(app, ev, &b),
            None => self.lsb(app, ev),
        }
    }

    /// Runs a raw MIDI binding. Plugin buttons keep separate, per-session ownership.
    pub(super) fn run(&self, app: &App, ev: Ev, b: &Binding) -> Outcome {
        if let Some(out) = self.owned(app, ev) {
            return out;
        }
        let act = &ACTIONS[b.act];
        if act.momentary() && !ev.on() {
            return Outcome::Done(b.act, b.deck);
        }
        // The controller's own layer keys come first: DECK 3 / 4 reach decks a 2-deck layout hides (it keeps its two slots).
        match act.kind {
            Kind::Layer => {
                if ev.on() {
                    self.select_layer(app, b.deck as usize);
                }
                return Outcome::Done(b.act, b.deck);
            }
            Kind::Dual => {
                if ev.on() {
                    self.toggle_dual(app, b.arg as usize);
                }
                return Outcome::Done(b.act, b.deck);
            }
            _ => {}
        }
        if b.deck as usize >= active() {
            // A control of the DECK 3 / 4 layer whose select message was missed: the controller is on that layer.
            self.select_layer(app, b.deck as usize);
        }
        if act.momentary() {
            let held = self.prepare_hold(app, b);
            self.held.borrow_mut().insert(ev.key(), held);
            return self.fire_recipients(app, ev, held);
        }
        for d in self.gang(act, b.deck as usize) {
            if let Some(done) = self.fire(app, ev, b, d) {
                return done;
            }
        }
        Outcome::Done(b.act, b.deck)
    }

    /// Existing ownership wins over LEARN, dumps, remapping and changed deck layers.
    fn owned(&self, app: &App, ev: Ev) -> Option<Outcome> {
        let held = {
            let mut held = self.held.borrow_mut();
            if ev.on() {
                held.get(&ev.key()).copied()
            } else {
                held.remove(&ev.key())
            }
        }?;
        if !ev.on() {
            self.release_held(app, held);
        }
        Some(Outcome::Done(held.binding.act, held.binding.deck))
    }

    pub(super) fn prepare_hold(&self, app: &App, b: &Binding) -> HeldAction {
        let mut binding = *b;
        if ACTIONS[b.act].name == "hw-sampler" {
            binding.arg += app.global::<SamplerState>().get_bank() * 8;
        }
        if ACTIONS[b.act].name == "hw-pad" {
            binding.arg = Deck::pad_mode_index(binding.arg / 16) * 16 + binding.arg % 16;
        } else if matches!(
            ACTIONS[b.act].name,
            "pad1" | "pad2" | "pad3" | "pad4" | "pad5" | "pad6" | "pad7" | "pad8"
        ) {
            binding.arg = app
                .global::<DeckState>()
                .get_decks()
                .row_data(b.deck as usize)
                .map_or(0, |v| v.pad_mode);
        }
        let decks = self
            .gang(&ACTIONS[b.act], b.deck as usize)
            .fold(0, |mask, d| mask | (1 << d));
        HeldAction { binding, decks }
    }

    pub(super) fn fire_recipients(&self, app: &App, ev: Ev, held: HeldAction) -> Outcome {
        for d in 0..self.decks.len() {
            if held.decks & (1 << d) != 0 {
                self.fire(app, ev, &held.binding, d);
            }
        }
        Outcome::Done(held.binding.act, held.binding.deck)
    }

    pub(super) fn release_held(&self, app: &App, held: HeldAction) {
        self.fire_recipients(
            app,
            Ev {
                status: 0x80,
                d1: 0,
                d2: 0,
            },
            held,
        );
    }
    /// Runs binding `b`'s action on deck `d`; `Some` = the message is not finished (MSB of a 14-bit pair).
    fn fire(&self, app: &App, ev: Ev, b: &Binding, d: usize) -> Option<Outcome> {
        let (act, on, deck) = (&ACTIONS[b.act], ev.on(), d as i32);
        match &act.kind {
            Kind::Press(f) => {
                if on {
                    f(app, deck);
                } else if let Some(r) = act.release {
                    r(app, deck, b.arg);
                }
            }
            Kind::Arg(f) => {
                if on {
                    f(app, deck, b.arg);
                } else if let Some(r) = act.release {
                    r(app, deck, b.arg);
                }
            }
            Kind::Deck(f, release) => {
                if on {
                    f(&self.decks[d], b.arg);
                } else if let Some(r) = release {
                    r(&self.decks[d], b.arg);
                }
            }
            Kind::Knob(_) => {
                if b.hires {
                    self.msb.borrow_mut().insert(ev.key(), ev.d2);
                    return Some(Outcome::Half);
                }
                return self.absolute(app, ev.key(), b, ev.d2 as f32 / 127.0);
            }
            Kind::Rel { two_comp, f } => f(app, deck, ev.ticks(*two_comp)),
            Kind::Jog { two_comp, mode } => self.turn(d, *mode, ev.ticks(*two_comp)),
            Kind::Touch => self.touch(d, on),
            Kind::Vinyl if on => self.set_vinyl(d, !self.vinyl.get()[d]),
            Kind::Cutter if on => self.set_cutter(d, !self.cutter.get()[d]),
            Kind::Vinyl | Kind::Cutter | Kind::Layer | Kind::Dual => {}
        }
        None
    }

    /// The decks a control of deck `d` drives: itself, and while DUAL DECK is on for its side also the other deck of the pair
    /// ([`layer::dual_partner`]); the decks then answer "as one": which controls are ganged, see [`gangs`].
    pub(super) fn gang(&self, act: &Action, d: usize) -> impl Iterator<Item = usize> {
        let partner = gangs(act)
            .then(|| layer::dual_partner(self.dual.get(), d, active()))
            .flatten();
        std::iter::once(d).chain(partner)
    }

    /// SHIFT + DECK select of `side` (0 left, 1 right): DUAL DECK on / off. It needs both decks of the side, so a 2-deck
    /// layout keeps all four in use (as DECK 3 / 4 do).
    pub(super) fn toggle_dual(&self, app: &App, side: usize) {
        let side = side.min(1);
        if self.reach(side + 2) {
            slots::publish(app);
        }
        let mut dual = self.dual.get();
        dual[side] = !dual[side];
        self.dual.set(dual);
        let (a, c) = (DECK_NAMES[side], DECK_NAMES[side + 2]);
        app.global::<MidiState>().set_status(
            tr!(
                "DUAL DECK {} + {} {}",
                a,
                c,
                if dual[side] { tr!("on") } else { tr!("off") }
            )
            .into(),
        );
    }

    /// The controller reaches deck `d`, which a 2-deck layout hides: all four decks stay in use ([`slots::reach`]; the layout
    /// and its two slots stay). `true` when that is new: the controls of the decks it adds were ignored until now, so ask
    /// where they are.
    fn reach(&self, d: usize) -> bool {
        let new = slots::reach(d);
        if new {
            self.request_state_again();
        }
        new
    }

    /// The LSB half of a 14-bit control: combine with the stored MSB and apply.
    fn lsb(&self, app: &App, ev: Ev) -> Outcome {
        let Some(msb_key) = ev.msb_key() else {
            return Outcome::Unmapped;
        };
        let Some(b) = self
            .find(msb_key)
            .filter(|b| b.hires && (b.deck as usize) < active())
        else {
            return Outcome::Unmapped;
        };
        let msb = self.msb.borrow().get(&msb_key).copied();
        if let Some(msb) = msb
            && let Some(waiting) = self.absolute(app, msb_key, &b, combine14(msb, ev.d2))
        {
            return waiting;
        }
        Outcome::Done(b.act, b.deck)
    }

    /// The controller selected deck `d` on its side: the lights follow it (a layer just selected is sent in full) and the
    /// slot of that side draws it. A deck a 2-deck layout hides is reached first.
    pub(super) fn select_layer(&self, app: &App, d: usize) {
        let reached = self.reach(d);
        let Some(l) = layer::select(self.layer.get(), d, active()) else {
            return;
        };
        if l != self.layer.replace(l) {
            self.snap_at.set(None);
            app.global::<MidiState>()
                .set_status(tr!("controller decks: {}", layer::describe(l)).into());
        }
        if slots::set_layer(l) || reached {
            slots::publish(app);
        }
    }
}

/// Actions DUAL DECK sends to both decks of a side: the transport, the loop and memory-cue buttons, the pad and cue-call
/// keys (the two decks are controlled together). Mixer knobs, LOAD, MASTER, sync and the
/// FX units stay with the deck whose layer is selected.
const GANGED: &[&str] = &[
    "play",
    "cue",
    "loop-in",
    "loop-out",
    "loop-4",
    "loop-halve",
    "loop-double",
    "reloop",
    "memory-cue",
    "hw-track-start",
    "hw-cue-call",
    "hw-pad",
    "hw-pad-delete",
    "hw-pad-stop",
    "hw-pad-unload",
    "hw-pad-mode",
    "hw-pad-page",
    "hw-memory-delete",
];

/// Does DUAL DECK send `act` to both decks: the platter (touch and turns) and [`GANGED`].
fn gangs(act: &Action) -> bool {
    matches!(act.kind, Kind::Jog { .. } | Kind::Touch) || GANGED.contains(&act.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::{DeckState, MixerState};
    use openfader::{
        library::{Library, own::Own},
        store::Store,
    };
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    fn fixture() -> (App, super::super::Guard) {
        crate::app::headless::init();
        let app = App::new().unwrap();
        let lib: Rc<dyn Library> = Rc::new(Own::new(Arc::new(Store::in_memory().unwrap())));
        let decks = crate::app::deck::wire(&app, lib, Default::default());
        crate::app::prefs::set_for_test(|p| {
            p.decks = 4;
            p.controller.soft_takeover = false;
        });
        let guard = super::super::wire(&app, decks);
        guard.midi.close(false);
        guard.midi.user.borrow_mut().clear();
        guard.midi.file_binds.borrow_mut().clear();
        guard.midi.dual.set([false; 2]);
        (app, guard)
    }

    fn binding(status: u8, d1: u8, name: &str, deck: i32, arg: i32) -> Binding {
        Binding {
            status,
            d1,
            deck,
            arg,
            hires: false,
            act: ACTIONS.iter().position(|a| a.name == name).unwrap(),
        }
    }

    fn event(status: u8, d1: u8, d2: u8) -> Ev {
        super::super::decode([status, d1, d2]).unwrap()
    }

    #[test]
    fn cue_hold_keeps_dual_recipients_and_binding_through_learn() {
        let (app, guard) = fixture();
        let m = &guard.midi;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let down = seen.clone();
        app.global::<DeckState>()
            .on_cue(move |d| down.borrow_mut().push((true, d)));
        let (up, midi) = (seen.clone(), m.clone());
        app.global::<DeckState>().on_cue_released(move |d| {
            assert!(
                midi.held.borrow().is_empty(),
                "ownership is removed before callbacks"
            );
            up.borrow_mut().push((false, d));
        });
        let b = binding(0x90, 36, "cue", 0, 0);
        m.file_binds.borrow_mut().insert((b.status, b.d1), b);
        m.dual.set([true, false]);
        m.handle(event(0x90, 36, 127));
        m.dual.set([false; 2]);
        m.select_layer(&app, 2);
        m.file_binds
            .borrow_mut()
            .insert((b.status, b.d1), binding(0x90, 36, "pad1", 1, 0));
        let learn = app.global::<MidiState>();
        learn.set_visible(true);
        m.handle(event(0x90, 37, 127));
        m.handle(event(0x90, 36, 127));
        assert_eq!(
            m.captured.get(),
            Some((0x90, 37)),
            "held repeats do not become new captures"
        );
        m.handle(event(0x80, 36, 0));
        assert_eq!(
            *seen.borrow(),
            [(true, 0), (true, 2), (false, 0), (false, 2)]
        );
        assert!(m.held.borrow().is_empty());
    }

    #[test]
    fn disconnect_releases_cc_cue_directly_while_learning() {
        let (app, guard) = fixture();
        let m = &guard.midi;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let down = seen.clone();
        app.global::<DeckState>()
            .on_cue(move |d| down.borrow_mut().push((true, d)));
        let (up, midi) = (seen.clone(), m.clone());
        app.global::<DeckState>().on_cue_released(move |d| {
            assert!(midi.held.borrow().is_empty());
            up.borrow_mut().push((false, d));
        });
        let b = binding(0xB0, 12, "cue", 1, 0);
        m.file_binds.borrow_mut().insert((b.status, b.d1), b);
        m.handle(event(0xB0, 12, 127));
        app.global::<MidiState>().set_visible(true);
        m.file_binds.borrow_mut().clear();
        m.close(false);
        m.close(false);
        assert_eq!(*seen.borrow(), [(true, 1), (false, 1)]);
        assert_eq!(m.captured.get(), None);
    }

    #[test]
    fn momentary_cc_low_releases_once_even_after_mapping_removal() {
        let (app, guard) = fixture();
        let m = &guard.midi;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let down = seen.clone();
        app.global::<DeckState>()
            .on_cue(move |d| down.borrow_mut().push((true, d)));
        let up = seen.clone();
        app.global::<DeckState>()
            .on_cue_released(move |d| up.borrow_mut().push((false, d)));
        let b = binding(0xB0, 12, "cue", 0, 0);
        m.file_binds.borrow_mut().insert((b.status, b.d1), b);
        m.dispatch(&app, event(0xB0, 12, 127));
        m.file_binds.borrow_mut().clear();
        let low = event(0xB0, 12, 63);
        assert!(!low.is_off(), "CC is not globally reclassified as note-off");
        m.dispatch(&app, low);
        m.file_binds.borrow_mut().insert((b.status, b.d1), b);
        m.dispatch(&app, event(0xB0, 12, 0));
        assert_eq!(*seen.borrow(), [(true, 0), (false, 0)]);
        assert!(m.held.borrow().is_empty());
    }

    #[test]
    fn hardware_sampler_hold_freezes_bank_but_stop_uses_current_bank() {
        let (app, guard) = fixture();
        let m = &guard.midi;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sampler = app.global::<SamplerState>();
        let down = seen.clone();
        sampler.on_pad_pressed(move |slot| down.borrow_mut().push(("down", slot)));
        let up = seen.clone();
        sampler.on_pad_released(move |slot| up.borrow_mut().push(("up", slot)));
        let stop = seen.clone();
        sampler.on_pad_stopped(move |slot| stop.borrow_mut().push(("stop", slot)));
        let b = binding(0x90, 40, "hw-sampler", 0, 2);
        sampler.set_bank(1);
        m.run(&app, event(0x90, 40, 127), &b);
        sampler.set_bank(2);
        m.run(&app, event(0x90, 40, 127), &b);
        m.run(&app, event(0x80, 40, 0), &b);
        let stop = binding(0x90, 41, "hw-sampler-stop", 0, 2);
        m.run(&app, event(0x90, 41, 127), &stop);
        assert_eq!(*seen.borrow(), [("down", 10), ("up", 10), ("stop", 18)]);
    }

    #[test]
    fn learnable_pads_release_only_their_own_pad() {
        let (app, guard) = fixture();
        let m = &guard.midi;
        let mut rows = vec![crate::app::DeckView::default(); 4];
        rows[0].pad_mode = 9;
        app.global::<DeckState>()
            .set_decks(slint::ModelRc::new(slint::VecModel::from(rows.clone())));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let down = seen.clone();
        app.global::<DeckState>()
            .on_pad(move |d, p| down.borrow_mut().push((true, d, p)));
        let up = seen.clone();
        app.global::<DeckState>()
            .on_pad_released_at(move |d, p, mode| {
                assert_eq!(mode, 9, "release keeps the mode from the press");
                up.borrow_mut().push((false, d, p));
            });
        let first = binding(0x90, 40, "pad1", 0, 0);
        let last = binding(0x90, 47, "pad8", 0, 0);
        m.run(&app, event(0x90, 40, 127), &first);
        m.run(&app, event(0x90, 47, 127), &last);
        rows[0].pad_mode = 4;
        app.global::<DeckState>()
            .set_decks(slint::ModelRc::new(slint::VecModel::from(rows)));
        m.run(&app, event(0x80, 40, 0), &first);
        assert_eq!(m.held.borrow().len(), 1);
        m.run(&app, event(0x80, 47, 0), &last);
        assert_eq!(
            *seen.borrow(),
            [(true, 0, 0), (true, 0, 7), (false, 0, 0), (false, 0, 7)]
        );
    }

    #[test]
    fn low_continuous_cc_values_and_state_dumps_remain_unowned() {
        let (app, guard) = fixture();
        let m = &guard.midi;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let values = seen.clone();
        app.global::<MixerState>()
            .on_crossfade(move |v| values.borrow_mut().push(v));
        let b = binding(0xB0, 7, "crossfader", 0, 0);
        m.file_binds.borrow_mut().insert((b.status, b.d1), b);
        m.handle(event(0xB0, 7, 127));
        m.handle(event(0xB0, 7, 32));
        m.dump.set(Some(super::super::dump::Window::new(
            std::time::Instant::now(),
        )));
        m.handle(event(0xB0, 7, 0));
        let cue = binding(0x90, 36, "cue", 0, 0);
        m.file_binds.borrow_mut().insert((cue.status, cue.d1), cue);
        app.global::<DeckState>()
            .on_cue(|_| panic!("dump reports must not press cue"));
        m.handle(event(0x90, 36, 127));
        assert_eq!(*seen.borrow(), [1.0, 32.0 / 127.0, 0.0]);
        assert!(m.held.borrow().is_empty());
    }

    fn act(name: &str) -> &'static Action {
        ACTIONS
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("no action {name}"))
    }

    #[test]
    fn dual_deck_gangs_the_deck_section_but_not_the_mixer() {
        assert!(
            GANGED.iter().all(|n| gangs(act(n))),
            "every name is a real action"
        );
        for n in ["jog", "jog-touch"] {
            assert!(gangs(act(n)), "{n}");
        }
        for n in ["fader", "eq-hi", "load", "hw-master", "sync", "hw-sampler"] {
            assert!(!gangs(act(n)), "{n} stays with its own deck");
        }
    }
}
