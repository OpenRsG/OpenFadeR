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
    /// The slot a held SAMPLER pad plays (released with the pad).
    held: Cell<Option<i32>>,
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
        self.sequencer.held.set(Some(slot));
        st.invoke_pad_pressed(slot);
    }

    /// The pad went up: a held SAMPLER slot is released (ends a GATE slot).
    pub(super) fn sampler_release(&self) {
        if let (Some(slot), Some(app)) = (self.sequencer.held.take(), self.app.upgrade()) {
            app.global::<SamplerState>().invoke_pad_released(slot);
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
        let (Some(app), Some(bus)) = (self.app.upgrade(), self.with_player(|p| p.bus())) else {
            return;
        };
        let (bank, slots) = (
            app.global::<SamplerState>().get_bank() as usize,
            prefs::with(|p| p.controller.slots()),
        );
        let rgb = pad_fx_rgb(PadFxColor::Purple);
        for (i, pad) in v.iter_mut().enumerate() {
            let slot = bank * 8 + i;
            let on = slot < slots && bus.sampler.slice(slot).is_some();
            pad.label = format!(
                "{}{}",
                slot + 1,
                if bus.sampler.seq_muted(slot) {
                    " M"
                } else {
                    ""
                }
            )
            .into();
            pad.set = on;
            pad.color = if on && bus.sampler.playing(slot) {
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
