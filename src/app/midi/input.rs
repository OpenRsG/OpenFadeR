//! One decoded message: captured by MIDI LEARN, taken as a state-dump report, or dispatched to its binding's action. Runs per
//! message on the UI thread, so nothing here allocates unless `OPENFADER_MIDI_LOG` or LEARN asks for text.

use super::actions::{ACTIONS, Action, Kind};
use super::{App, Binding, Ev, Midi, Outcome, combine14, describe, layer};
use crate::app::{
    MidiState,
    deck::{DECK_NAMES, active, slots},
};
use slint::ComponentHandle;

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
        if state.get_visible() {
            if !ev.is_off() {
                self.captured.set(Some(ev.key()));
                state.set_captured(describe(ev.key()).into());
            }
            return;
        }
        let out = self.dump_step(&app, ev).unwrap_or_else(|| {
            let mut held = self.held.borrow_mut();
            if ev.is_off() {
                held.remove(&ev.key());
            } else if ev.status & 0xF0 == 0x90 {
                held.insert(ev.key());
            }
            drop(held);
            self.dispatch(&app, ev)
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
        match self.find(ev.key()) {
            Some(b) => self.run(app, ev, &b),
            None => self.lsb(app, ev),
        }
    }

    /// Runs binding `b` for `ev` (what [`Midi::dispatch`] does once the message has its binding; controller plugins' controls
    /// come here too, `claim/control.rs`).
    pub(super) fn run(&self, app: &App, ev: Ev, b: &Binding) -> Outcome {
        let act = &ACTIONS[b.act];
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
        for d in self.gang(act, b.deck as usize) {
            if let Some(done) = self.fire(app, ev, b, d) {
                return done;
            }
        }
        Outcome::Done(b.act, b.deck)
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
