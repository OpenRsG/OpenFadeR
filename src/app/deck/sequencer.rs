//! SAMPLER and SEQ. CALL pad modes. SAMPLER: pad `n` plays slot `n` of the sampler's current bank (it is the
//! sampler panel's own pad: same press / release, bank, MUTE / ERASE and sequence recording, see [`crate::app::sampler`]); lit when the slot holds a
//! sample, brighter while it plays. SEQ. CALL: pad `n` plays saved sequence `n` from its beginning (again from the start when pressed again,
//! Shift + pad pauses it); lit when saved, brighter while it plays. Both work on an empty deck.

use super::{Deck, pads::PadMode};
use crate::app::{PadView, SamplerState, fx::pad_fx_rgb, prefs, prefs::view::PadFxColor, sampler};
use slint::{Color, ComponentHandle};
use std::cell::Cell;

#[derive(Default)]
pub(super) struct State {
    /// The slot each held SAMPLER pad plays (released with its pad).
    held: [Cell<Option<i32>>; 8],
}

/// `c` mixed 55 % towards white: a playing pad.
fn bright([r, g, b]: [u8; 3]) -> Color {
    let up = |c: u8| (c as f32 + (255.0 - c as f32) * 0.55) as u8;
    Color::from_rgb_u8(up(r), up(g), up(b))
}

impl Deck {
    /// SAMPLER pad down: the slot of the current bank.
    pub(super) fn sampler_pad(&self, pad: usize) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let st = app.global::<SamplerState>();
        let slot = st.get_bank() * 8 + pad as i32;
        self.sequencer.held[pad % 8].set(Some(slot));
        st.invoke_pad_pressed(slot);
    }

    /// Every held SAMPLER pad went up (a keyboard key, a mode change): the held slots are released (ends GATE slots).
    pub(super) fn sampler_release(&self) {
        (0..8).for_each(|pad| self.sampler_release_pad(pad));
    }

    /// SAMPLER pad `pad` went up: its slot is released.
    fn sampler_release_pad(&self, pad: usize) {
        if let (Some(slot), Some(app)) = (self.sequencer.held[pad % 8].take(), self.app.upgrade()) {
            app.global::<SamplerState>().invoke_pad_released(slot);
        }
    }

    /// A controller pad went up (`pad` 0..16): in SAMPLER mode only its own slot is released, so other held pads go on playing.
    pub fn pad_released_at(&self, pad: usize) {
        if self.pad_mode() == PadMode::Sampler {
            self.sampler_release_pad(pad);
        } else {
            self.pad_released();
        }
    }

    /// Shift + pad: SAMPLER stops the slot, SEQ. CALL pauses the sequence.
    pub fn pad_stop(&self, pad: usize) {
        match self.pad_mode() {
            PadMode::Sampler => {
                if let Some(app) = self.app.upgrade() {
                    let st = app.global::<SamplerState>();
                    st.invoke_pad_stopped(st.get_bank() * 8 + pad as i32);
                }
            }
            PadMode::SeqCall => sampler::call_stop(),
            _ => {}
        }
    }

    /// Lights the SAMPLER pads: slots of the current bank in use (PREFS > CONTROLLER > Sampler > SLOT) that hold a sample.
    pub(super) fn sampler_pads(&self, v: &mut [PadView]) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let bus = self.with_player(|p| p.bus());
        let (bank, slots) = (
            app.global::<SamplerState>().get_bank() as usize,
            prefs::with(|p| p.controller.slots()),
        );
        let rgb = pad_fx_rgb(PadFxColor::Purple);
        for (i, pad) in v.iter_mut().enumerate() {
            let slot = bank * 8 + i;
            let on = slot < slots
                && bus
                    .as_ref()
                    .is_some_and(|b| b.sampler.slice(slot).is_some());
            let muted = bus.as_ref().is_some_and(|b| b.sampler.seq_muted(slot));
            pad.label = format!("{}{}", slot + 1, if muted { " M" } else { "" }).into();
            pad.set = on;
            pad.color = if on && bus.as_ref().is_some_and(|b| b.sampler.playing(slot)) {
                bright(rgb)
            } else {
                Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
            };
        }
    }

    /// Lights the SEQ. CALL pads: the saved sequences.
    pub(super) fn seqcall_pads(&self, v: &mut [PadView]) {
        for (pad, (saved, playing)) in v.iter_mut().zip(sampler::pad_state()) {
            pad.set = saved;
            if playing {
                pad.color = bright([230, 80, 120]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DECKS, sync};
    use super::*;
    use crate::app::{App, DeckLive, DeckScroll, DeckView, headless};
    use openfader::{
        library::{Library, own::Own},
        store::Store,
    };
    use slint::VecModel;
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    #[test]
    fn a_controller_pad_releases_only_its_own_sampler_slot() {
        headless::init();
        let app = App::new().unwrap();
        let model = Rc::new(VecModel::from(vec![DeckView::default(); DECKS]));
        let scroll = Rc::new(VecModel::from(vec![DeckScroll::default(); DECKS]));
        let lib: Rc<dyn Library> = Rc::new(Own::new(Arc::new(Store::in_memory().unwrap())));
        let live = Rc::new(VecModel::from(vec![DeckLive::default(); DECKS]));
        let deck = Deck::new(&app, 0, model, scroll, live, Rc::new(sync::Link::default()), lib);
        let log = Rc::new(RefCell::new(Vec::new()));
        let st = app.global::<SamplerState>();
        let l = log.clone();
        st.on_pad_pressed(move |slot| l.borrow_mut().push(('+', slot)));
        let l = log.clone();
        st.on_pad_released(move |slot| l.borrow_mut().push(('-', slot)));
        let sampler = PadMode::Sampler as i32;
        deck.pad_hw(sampler, 0);
        deck.pad_hw(sampler, 1); // pad 1 pressed while pad 0 is held
        assert_eq!(*log.borrow(), [('+', 0), ('+', 1)], "pad 0 stays held");
        deck.pad_released_at(0);
        deck.pad_released_at(1);
        assert_eq!(
            *log.borrow(),
            [('+', 0), ('+', 1), ('-', 0), ('-', 1)],
            "each release ends its own slot"
        );
    }
}
