//! The AUX strip (`MixerState.channels[`[`AUX_CH`]`]`): its knobs, LEVEL, CUE and ON go to [`auxin::controls`]; ON also opens and
//! closes the input pair PREFS > AUDIO > AUX INPUT names (the pair stays closed while the strip is off, so an unused AUX never
//! holds a device). The strip shows while "Enable AUX input" is on.

use super::{channel::eq_dbs, curve, map, Mixer, MixerState, AUX_CH};
use crate::app::prefs::{
    self,
    controller::{Fader, FaderCurve},
    Prefs,
};
use openfader::audio::auxin;
use slint::{ComponentHandle, Model};

/// LEVEL of the strip at fader position `f`: the channel fader curve, or a linear one while a plugin curve is selected
/// (a plugin curve runs in a deck's voice; the AUX has none).
fn level(sel: Fader, f: f32) -> f32 {
    curve::fader(sel.curve().unwrap_or(FaderCurve::Linear), f)
}

impl Mixer {
    /// Pushes the strip's knobs, LEVEL, CUE and ON to the audio.
    pub(super) fn apply_aux(&self) {
        let row = self.row(AUX_CH);
        let cfg = self.cfg.get();
        let k = |i| row.knobs.row_data(i).unwrap_or(map::CENTRE);
        let aux = auxin::controls();
        aux.set_strip(
            map::db_to_gain(map::trim_db(k(map::TRIM))),
            eq_dbs(&row, cfg.eq),
            map::cfx(k(map::CFX)),
            level(cfg.fader, row.fader),
        );
        aux.set_cue(row.cue);
        aux.set_on(row.on);
    }

    /// The strip follows the settings: shown while AUX input is enabled (off with it), its input pair open while the strip is ON.
    pub(super) fn sync_aux(&self, p: &Prefs) {
        let a = &p.audio;
        if let Some(app) = self.app.upgrade() {
            app.global::<MixerState>().set_aux_shown(a.aux_on);
        }
        if !a.aux_on && self.row(AUX_CH).on {
            self.update(AUX_CH, |row| row.on = false);
        }
        let (device, pair) = a.aux_source();
        auxin::open(self.row(AUX_CH).on, device, pair);
        let problem = auxin::problem();
        if problem != *self.aux_err.borrow() {
            match &problem {
                Some(e) => self.set_status(tr!("AUX: {}", e)),
                None => self.edit_bus(|b| b.status = Default::default()),
            }
            *self.aux_err.borrow_mut() = problem;
        }
        self.apply_aux();
    }

    /// The ON switch (and a controller's AUX ON).
    pub(super) fn toggle_aux(&self) {
        self.update(AUX_CH, |row| row.on = !row.on);
        prefs::with(|p| self.sync_aux(p));
    }

    /// The strip's meter, from the peak the audio thread leaves (`dt` seconds since the last tick).
    pub(super) fn tick_aux(&self, dt: f32) {
        let mut vu = self.aux_vu.borrow_mut();
        vu.update(auxin::controls().peak(), dt);
        let shown = (vu.level(), vu.peak());
        if self.shown_aux.replace(shown) != shown {
            self.update(AUX_CH, |row| (row.level, row.peak) = shown);
        }
    }
}
