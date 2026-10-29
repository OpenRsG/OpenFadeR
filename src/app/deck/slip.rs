//! SLIP, REVERSE and BRAKE (vinyl stop). The audio engine does the work (`Player::set_slip` keeps
//! a shadow playhead running at normal speed, `set_reverse`, `brake`); the deck mirrors the
//! toggles into its view and `tick` draws the shadow playhead as a ghost line.

use super::{Deck, pads::PadMode, prefs};

impl Deck {
    pub fn toggle_slip(&self) {
        if !self.ready() {
            return;
        }
        let want = !self.view.borrow().slip;
        self.rev_slip.set(false); // the button now owns slip: REVERSE off leaves it alone
        let on = self
            .with_player(|p| {
                p.set_slip(want);
                p.slip()
            })
            .unwrap_or(false);
        self.show(|v| v.slip = on);
    }

    /// REVERSE. With Q and QUANTIZE REVERSE on it engages slip until released, so REVERSE off returns
    /// to where forward play would be (the player returns to the shadow playhead by itself when reverse ends).
    pub fn toggle_reverse(&self) {
        if !self.ready() {
            return;
        }
        let on = self
            .with_player(|p| {
                let on = !p.reverse();
                if on && !p.slip() && self.reverse_quantized() {
                    p.set_slip(true);
                    self.rev_slip.set(true);
                }
                p.set_reverse(on);
                if !on && self.rev_slip.take() {
                    p.slip_off_after_return();
                }
                p.reverse()
            })
            .unwrap_or(false);
        self.show(|v| v.reverse = on);
    }

    /// Vinyl stop: slows to zero over the Touch/Brake time of Vinyl Speed Adjust, then pauses.
    pub fn brake(&self) {
        if self.ready() {
            let s = prefs::with(|p| p.controller.vinyl_seconds().0);
            self.with_player(|p| p.brake(s));
        }
    }

    /// Vinyl Speed Adjust: the jog touch / release ramps are a quarter of the BRAKE / start times (the fraction is ours).
    pub(super) fn apply_vinyl(&self) {
        let (brake, start) = prefs::with(|p| p.controller.vinyl_seconds());
        self.with_player(|p| p.set_vinyl(brake / 4.0, start / 4.0));
    }

    /// A pad was released: a plugin pad runs its mode's `release` steps; a held PAD FX pad switches its effect off; a gated hot
    /// cue (HOT CUE gate,) goes back to its cue and pauses; a hot-cue / beat-jump pad in slip mode returns playback
    /// to the shadow playhead. Beat-loop, keyboard and key-shift pads need no return (a loop hands back by itself).
    pub fn pad_released(&self) {
        self.pad_owner.set(None);
        self.plugin_release();
        self.pad_fx_off();
        self.slicer_release();
        self.sampler_release();
        if let Some(ms) = self.gate.take() {
            self.pause_at(ms);
        } else if matches!(self.pad_mode(), PadMode::HotCue | PadMode::BeatJump) {
            self.beat_jump_released();
        }
    }

    pub fn beat_jump_released(&self) {
        self.with_player(|p| {
            if p.slip() {
                p.slip_return();
            }
        });
    }
}
