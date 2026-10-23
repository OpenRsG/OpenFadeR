//! SEQUENCER ↔ the sequencer block of ui/sampler.slint (`SamplerState.seq-*`); the pattern clock and its events are in
//! `src/audio/sampler/seq.rs`, saved patterns in `src/store/sequence.rs`.
//!
//! REC (stop → standby: the first hit of a one-shot slot starts the pattern; play → overdub; REC again leaves the overdub and keeps
//! playing), PLAY / STOP, 1 / 2 / 4 BAR, MUTE and ERASE (while lit, a slot pad mutes / erases that slot's events instead of playing),
//! SAVE into the saved-sequence slot in view (up to 8), ◀ ▶ to browse the saved ones (a saved sequence loads, its bank comes up), ×
//! to delete one. Starting a recording from a stop drops the temporary pattern.
//! The sequencer follows the master deck's BPM; without one it refuses to start. SEQ. CALL pads ([`call`]) load saved sequence `n`
//! and play it from its beginning. The step hits are moved to follows CONTROLLER > Others > QUANTIZE > SEQUENCER and its beat length.

use super::{ME, SLOTS, Sampler};
use crate::app::{SamplerState, prefs};
use openfader::audio::sampler::{BARS, Mode, TICKS, len_ticks};
use openfader::store::{self, MAX_SEQUENCES, Sequence};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::{Cell, RefCell};

#[derive(Default)]
pub(super) struct Ui {
    saved: RefCell<[Option<Sequence>; MAX_SEQUENCES]>,
    /// The saved slot in view (SAVE writes it, ◀ ▶ move it, SEQ. CALL plays it).
    sel: Cell<usize>,
    /// The pattern in the sequencer was recorded and not saved yet (the red dot).
    unsaved: Cell<bool>,
    mute: Cell<bool>,
    erase: Cell<bool>,
    /// Master BPM as of the last tick, 0 = none.
    pub(super) bpm: Cell<f32>,
    /// Mode and saved flags last pushed to the decks' pads.
    pads: Cell<(u8, u8)>,
}

impl Sampler {
    /// QUANTIZE > SEQUENCER on: the hits go to the grid of the QUANTIZE beat length (ticks), else none.
    fn seq_step() -> u32 {
        prefs::with(|p| {
            let c = &p.controller;
            if c.quantize_sequencer {
                (c.quantize_length.beats() * TICKS as f64).round() as u32
            } else {
                0
            }
        })
    }

    pub(super) fn seq_init(&self) {
        match store::global().sequences() {
            Ok(s) => *self.seq.saved.borrow_mut() = s,
            Err(e) => self.say(tr!("saved sequences: {}", e)),
        }
        self.seq_show();
    }

    fn seq_mode(&self) -> Mode {
        self.bus
            .as_ref()
            .map_or(Mode::Stop, |b| b.sampler.seq_mode())
    }

    /// Pushes the sequencer's state to the panel and, when it changed, to the decks' SAMPLER / SEQ. CALL pads.
    pub(super) fn seq_show(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (ui, st) = (&self.seq, app.global::<SamplerState>());
        let (mode, bars) = self.bus.as_ref().map_or((Mode::Stop, 1), |b| {
            (b.sampler.seq_mode(), b.sampler.seq_bars())
        });
        let flags: Vec<bool> = ui.saved.borrow().iter().map(Option::is_some).collect();
        st.set_seq_mode(mode as i32);
        st.set_seq_bars(bars as i32);
        st.set_seq_mute(ui.mute.get());
        st.set_seq_erase(ui.erase.get());
        st.set_seq_slot(ui.sel.get() as i32);
        st.set_seq_unsaved(ui.unsaved.get());
        let mask = flags
            .iter()
            .enumerate()
            .fold(0u8, |m, (i, &f)| m | (f as u8) << i);
        st.set_seq_saved(ModelRc::new(VecModel::from(flags)));
        if ui.pads.replace((mode as u8, mask)) != (mode as u8, mask) {
            self.decks.iter().for_each(|d| d.refresh_pads());
        }
    }

    /// The master tempo the sequencer needs; says so when there is none.
    fn seq_tempo(&self) -> bool {
        let ok = self.seq.bpm.get() > 0.0;
        if !ok {
            self.say(tr!(
                "the sequencer follows the master deck's BPM: no deck has a tempo yet"
            ));
        }
        ok
    }

    pub(super) fn seq_rec(&self) {
        let Some(bus) = &self.bus else { return };
        let next = match bus.sampler.seq_mode() {
            Mode::Stop => {
                if !self.seq_tempo() {
                    return;
                }
                bus.sampler.seq_set(bus.sampler.seq_bars(), &[]);
                self.say(tr!(
                    "sequence REC: standby, the first hit of a SHOT slot starts it"
                ));
                Mode::Standby
            }
            Mode::Play => {
                self.say(tr!("sequence REC: overdub"));
                Mode::Rec
            }
            Mode::Standby => Mode::Stop,
            Mode::Rec => Mode::Play,
        };
        bus.sampler.set_seq_step(Self::seq_step());
        bus.sampler.set_seq_mode(next);
        self.seq_show();
    }

    pub(super) fn seq_play(&self) {
        let Some(bus) = &self.bus else { return };
        let next = match bus.sampler.seq_mode() {
            Mode::Stop => {
                if bus.sampler.seq_events().is_empty() {
                    return self.say(tr!("nothing recorded: REC, then play the slots"));
                }
                if !self.seq_tempo() {
                    return;
                }
                Mode::Play
            }
            _ => Mode::Stop,
        };
        bus.sampler.set_seq_mode(next);
        self.seq_show();
    }

    pub(super) fn seq_bars(&self, bars: i32) {
        if let (Some(bus), Some(&b)) = (&self.bus, BARS.iter().find(|&&b| b as i32 == bars)) {
            bus.sampler.set_seq_bars(b);
            self.seq_show();
        }
    }

    pub(super) fn seq_toggle(&self, erase: bool) {
        let flag = if erase {
            &self.seq.erase
        } else {
            &self.seq.mute
        };
        flag.set(!flag.get());
        if flag.get() {
            // the two modes exclude each other
            (if erase {
                &self.seq.mute
            } else {
                &self.seq.erase
            })
            .set(false);
        }
        self.seq_show();
    }

    /// A slot pad pressed while MUTE or ERASE is lit: acts on the sequence instead of playing; true when it did.
    pub(super) fn seq_pad(&self, slot: usize) -> bool {
        let Some(bus) = self.bus.as_ref().filter(|_| slot < SLOTS) else {
            return false;
        };
        if self.seq.mute.get() {
            let muted = bus.sampler.seq_toggle_mute(slot);
            self.say(if muted {
                tr!("slot {} muted in the sequence", slot + 1)
            } else {
                tr!("slot {} unmuted in the sequence", slot + 1)
            });
        } else if self.seq.erase.get() {
            bus.sampler.seq_erase(slot);
            self.say(tr!("slot {} erased from the sequence", slot + 1));
        } else {
            return false;
        }
        self.refresh();
        true
    }

    pub(super) fn seq_save(&self) {
        let Some(bus) = &self.bus else { return };
        let events = bus.sampler.seq_events();
        if events.is_empty() {
            return self.say(tr!("nothing to save: record a sequence first"));
        }
        let (n, bank) = (
            self.seq.sel.get(),
            self.app
                .upgrade()
                .map_or(0, |a| a.global::<SamplerState>().get_bank()),
        );
        let s = Sequence {
            bars: bus.sampler.seq_bars(),
            bank: bank as u8,
            events,
        };
        match store::global().save_sequence(n, &s) {
            Ok(()) => {
                self.say(tr!("sequence saved in slot {}", n + 1));
                self.seq.saved.borrow_mut()[n] = Some(s);
                self.seq.unsaved.set(false);
            }
            Err(e) => self.say(tr!("sequence NOT saved: {}", e)),
        }
        self.seq_show();
    }

    /// Loads saved sequence `n` into the sequencer (stopped), stops the slots that play and brings up its bank; false when the slot is empty.
    fn seq_load(&self, n: usize) -> bool {
        let (Some(bus), Some(s)) = (&self.bus, self.seq.saved.borrow().get(n).cloned().flatten())
        else {
            return false;
        };
        bus.sampler.set_seq_mode(Mode::Stop);
        bus.sampler.seq_set(s.bars, &s.events);
        (0..SLOTS).for_each(|i| bus.sampler.stop(i));
        self.seq.sel.set(n);
        self.seq.unsaved.set(false);
        if let Some(app) = self.app.upgrade() {
            let bank = if self.used.get() > 8 {
                s.bank as i32
            } else {
                0
            };
            let st = app.global::<SamplerState>();
            st.set_bank(bank);
            self.select(bank as usize * 8 + st.get_selected() as usize % 8);
            self.decks.iter().for_each(|d| d.refresh_pads()); // the deck pads show the visible bank
        }
        true
    }
    /// ◀ ▶ : the next / previous saved-sequence slot; a saved one loads.
    pub(super) fn seq_pick(&self, dir: i32) {
        let n = (self.seq.sel.get() as i32 + dir).rem_euclid(MAX_SEQUENCES as i32) as usize;
        self.seq.sel.set(n);
        if self.seq_load(n) {
            self.say(tr!("sequence {} loaded", n + 1));
        } else {
            self.say(tr!("sequence slot {} is empty: SAVE writes it", n + 1));
        }
        self.seq_show();
    }

    pub(super) fn seq_delete(&self) {
        let n = self.seq.sel.get();
        if self.seq.saved.borrow()[n].is_none() {
            return self.say(tr!("sequence slot {} is empty", n + 1));
        }
        match store::global().delete_sequence(n) {
            Ok(()) => {
                self.seq.saved.borrow_mut()[n] = None;
                self.say(tr!("sequence {} deleted", n + 1));
            }
            Err(e) => self.say(tr!("sequence NOT deleted: {}", e)),
        }
        self.seq_show();
    }

    /// 50 ms: the clock marker, the mode the audio thread switched to (standby → rec on the first hit), the quantise step.
    pub(super) fn seq_tick(&self) {
        let (Some(app), Some(bus)) = (self.app.upgrade(), &self.bus) else {
            return;
        };
        let (st, c) = (app.global::<SamplerState>(), &bus.sampler);
        let mode = c.seq_mode();
        if mode == Mode::Rec && !self.seq.unsaved.get() && !c.seq_events().is_empty() {
            self.seq.unsaved.set(true);
        }
        c.set_seq_step(Self::seq_step());
        st.set_seq_pos(c.seq_phase() as f32 / len_ticks(c.seq_bars()) as f32);
        if st.get_seq_mode() != mode as i32 || st.get_seq_unsaved() != self.seq.unsaved.get() {
            self.seq_show();
        }
    }
}

/// SEQ. CALL pad `n`: loads saved sequence `n` and plays it from its beginning (again from the start when it already plays).
pub fn call(n: usize) {
    let Some(s) = ME.with(|m| m.borrow().upgrade()) else {
        return;
    };
    let Some(bus) = &s.bus else { return };
    if !s.seq_load(n) {
        return s.say(tr!("no saved sequence in slot {}", n + 1));
    }
    if s.seq_tempo() {
        bus.sampler.set_seq_mode(Mode::Play);
        bus.sampler.seq_restart();
    }
    s.seq_show();
}

/// Shift + SEQ. CALL pad: the sequence pauses.
pub fn call_stop() {
    if let Some(s) = ME.with(|m| m.borrow().upgrade()) {
        if let Some(bus) = &s.bus {
            bus.sampler.set_seq_mode(Mode::Stop);
        }
        s.seq_show();
    }
}

/// Per SEQ. CALL pad: (saved, playing now).
pub fn pad_state() -> [(bool, bool); MAX_SEQUENCES] {
    let Some(s) = ME.with(|m| m.borrow().upgrade()) else {
        return [(false, false); MAX_SEQUENCES];
    };
    let on = matches!(s.seq_mode(), Mode::Play | Mode::Rec);
    let saved = s.seq.saved.borrow();
    std::array::from_fn(|i| (saved[i].is_some(), on && s.seq.sel.get() == i))
}

pub(super) fn wire(me: &std::rc::Rc<Sampler>, st: &SamplerState<'_>) {
    on!(st, me, on_seq_rec, |s| s.seq_rec());
    on!(st, me, on_seq_play, |s| s.seq_play());
    on!(st, me, on_seq_bars_selected, |s, b| s.seq_bars(b));
    on!(st, me, on_seq_mute_toggled, |s| s.seq_toggle(false));
    on!(st, me, on_seq_erase_toggled, |s| s.seq_toggle(true));
    on!(st, me, on_seq_save, |s| s.seq_save());
    on!(st, me, on_seq_pick, |s, d| s.seq_pick(d));
    on!(st, me, on_seq_delete, |s| s.seq_delete());
    me.seq_init();
}
