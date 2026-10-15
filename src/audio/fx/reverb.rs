//! REVERB: Freeverb ([`Reverb`], in the plugin SDK's `dsp` module so plugins share it). The BEAT selector sets
//! the room size (1/8 beat = small room, 16 = long tail): comb feedback `0.7 + 0.28 × room` with
//! `room = 0.6 + 0.04 × (log2 beats + 3)`, capped at 0.9 (the beat → room mapping is this port's choice). Fed
//! `(L + R) × 0.015 × g`: at OFF the tail rings out.

use super::{Ctx, Frame, Stage};
pub(super) use super::Reverb; // `Unit` builds it as `reverb::Reverb`

impl Stage for Reverb {
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        Reverb::tick(self, x, c)
    }

    fn reset(&mut self) {
        self.clear();
    }
}
