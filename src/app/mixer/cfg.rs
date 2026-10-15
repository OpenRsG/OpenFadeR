//! The Preferences the mixer acts on (CONTROLLER > Mixer, EXTENSIONS > STEM ISO, STEMS), cached in [`Cfg`] so a knob move does
//! not clone all of `Prefs`, and the AUTO GAIN rule.

use crate::app::prefs::{
    self,
    controller::{EqType, Fader, KnobMode},
};
use openfader::audio::loudness::MAX_GAIN_DB;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Cfg {
    pub auto_gain: bool,
    pub eq: EqType,
    pub fader: Fader,
    pub cfx_knob: KnobMode,
    /// Output Level as a linear gain on the master bus.
    pub output: f32,
    pub release_cfx_off: bool,
    /// STEMS is on.
    pub stems: bool,
    /// The STEM ISO chip is shown.
    pub iso_shown: bool,
}

impl Cfg {
    pub fn of(p: &prefs::Prefs) -> Cfg {
        Cfg {
            auto_gain: p.controller.auto_gain,
            eq: p.controller.eq_type,
            fader: p.controller.fader_curve,
            cfx_knob: p.controller.cfx_knob,
            output: p.controller.output_gain(),
            release_cfx_off: p.controller.release_cfx_off,
            stems: p.stems,
            iso_shown: p.extensions.stem_displays()[1],
        }
    }

    /// `MixerState.cfx-stop`: the COLOR FX KNOB mode's stop, -1 for NORMAL.
    pub fn cfx_stop(&self) -> f32 {
        self.cfx_knob.stop().unwrap_or(-1.0)
    }
}

/// The gain in dB AUTO GAIN adds to a channel's trim: the loaded track's `deck_db`, within +-12 dB, or 0 when off.
pub(super) fn auto_db(on: bool, deck_db: f32) -> f32 {
    if on && deck_db.is_finite() {
        deck_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_gain_is_the_decks_value_within_12_db_and_only_when_on() {
        assert_eq!(auto_db(true, 5.0), 5.0);
        assert_eq!(auto_db(true, -30.0), -12.0);
        assert_eq!(auto_db(true, f32::NAN), 0.0);
        assert_eq!(auto_db(false, 5.0), 0.0);
    }
}
