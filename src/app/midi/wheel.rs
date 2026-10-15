//! Platter and jog state of the controller's decks: [`super::jog`] decides, this applies the result to the deck.

use super::actions::JogMode;
use super::jog::{self, Out};
use super::{DECKS, Midi};
use crate::app::MidiState;
use slint::ComponentHandle;

impl Midi {
    pub(super) fn turn(&self, d: usize, mode: JogMode, ticks: i32) {
        let out = {
            let mut jogs = self.jog.borrow_mut();
            match mode {
                // In JOG CUTTER mode a turning platter opens the gate whichever control reports it (no dedicated
                // message carries the area, see [`super::jog`]).
                JogMode::Scratch if self.cutter.get()[d] => jogs[d].cut(ticks),
                JogMode::Scratch => Some(jogs[d].turn(ticks)),
                JogMode::Bend => jogs[d].bend(ticks),
                JogMode::Search => Some(jogs[d].search(ticks)),
                JogMode::Cut => jogs[d].cut(ticks),
            }
        };
        self.drive(d, out);
    }

    /// The platter of deck `d` touched (`on`) or released.
    pub(super) fn touch(&self, d: usize, on: bool) {
        let out = self.jog.borrow_mut()[d].touch(on, self.vinyl.get()[d]);
        self.drive(d, out);
    }

    /// VINYL mode of deck `d` on / off; switching it off takes JOG CUTTER with it ([`jog::cutters`]).
    pub(super) fn set_vinyl(&self, d: usize, on: bool) {
        let mut v = self.vinyl.get();
        v[d] = on;
        self.vinyl.set(v);
        if !on {
            self.set_cutter(d, false);
        }
        let out = self.jog.borrow_mut()[d].vinyl(on);
        self.drive(d, out);
    }

    /// JOG CUTTER of deck `d` on / off: it needs VINYL mode and runs on one deck at a time ([`jog::cutters`]); a press that
    /// cannot be honoured says why.
    pub(super) fn set_cutter(&self, d: usize, on: bool) {
        let before = self.cutter.get();
        let after = jog::cutters(before, self.vinyl.get(), d, on);
        self.cutter.set(after);
        for i in (0..DECKS).filter(|&i| before[i] != after[i]) {
            let outs = self.jog.borrow_mut()[i].cutter(after[i]);
            for out in outs {
                self.drive(i, Some(out));
            }
        }
        if on
            && !after[d]
            && let Some(app) = self.app.upgrade()
        {
            app.global::<MidiState>()
                .set_status("JOG CUTTER needs VINYL mode (SHIFT + JOG CUTTER)".into());
        }
    }

    fn drive(&self, d: usize, out: Option<Out>) {
        let deck = &self.decks[d];
        match out {
            None => {}
            Some(Out::Begin) => deck.jog_begin(),
            Some(Out::End) => deck.scratch_end(),
            Some(Out::Scratch(s)) => deck.scratch_move(s * self.jog_factor.get()),
            Some(Out::Bend(r)) => deck.bend(r),
            Some(Out::Search(ms)) => deck.search(ms),
            Some(Out::Cut(on)) => deck.cut(on),
        }
    }

    /// 5 ms: the pitch bend of a stopped wheel fades out.
    pub(super) fn tick(&self) {
        for d in 0..DECKS {
            let out = self.jog.borrow_mut()[d].decay();
            self.drive(d, out);
            let out = self.jog.borrow_mut()[d].decay_cut();
            self.drive(d, out);
        }
    }
}
