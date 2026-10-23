//! One channel strip: knobs, EQ / STEM ISO mode, CFX type, fader, CUE and crossfader assignment, pushed to the deck's player.

use super::{
    auto_db, crossfade_gains, cue_gain, curve, gain, map, side_gain, App, Mixer, MixerState, DECKS,
};
use crate::app::{ChannelView, prefs::controller::EqType};
use openfader::audio::{
    cfx::{self, Kind},
    EQ_KILL_DB,
};
use slint::{ComponentHandle, Model};

/// The STEM mode puts INST on the HIGH knob and VOCAL on MID (top to bottom: INST, VOCAL, DRUMS), the player's gains
/// and the controller's PART ISOLATOR are VOCAL, INST, DRUMS: knob slot of each part (HIGH, MID, LOW).
const SLOT: [usize; 3] = [1, 0, 2];

/// Three values in knob order (INST, VOCAL, DRUMS) as part order (VOCAL, INST, DRUMS), or back: the swap is its own inverse.
/// BASS has no knob: with 4 stems it rides the INST knob (`deck::stems::gains`).
fn swap_vocal_inst(v: [f32; 3]) -> [f32; 3] {
    SLOT.map(|i| v[i])
}

/// The EQ the player runs, `[low, mid, high]` in dB, from the EQ knobs alone: STEM ISO only changes what the HIGH / MID / LOW
/// knobs show, so a band the DJ cut stays cut and the stem levels apply on top. `low_cut` is the AUTO LOW CUT kill.
fn eq_dbs(row: &ChannelView, eq: EqType, low_cut: bool) -> [f32; 3] {
    let db = |band| map::eq_db(row.knobs.row_data(band).unwrap_or(map::CENTRE), eq);
    let low = if low_cut { EQ_KILL_DB } else { db(map::LOW) };
    [low, db(map::MID), db(map::HI)]
}

/// Knob `knob` of channel `ch` as the strip shows it, 0..1: in STEM mode HI / MID / LOW show the ISO gains ([`Mixer::set_knob`]).
pub(in crate::app) fn knob_at(app: &App, ch: i32, knob: usize) -> Option<f32> {
    let row = app
        .global::<MixerState>()
        .get_channels()
        .row_data(usize::try_from(ch).ok()?)?;
    if row.iso && (map::HI..=map::LOW).contains(&knob) {
        row.iso_knobs.row_data(knob - map::HI)
    } else {
        row.knobs.row_data(knob)
    }
}

/// STEM ISOLATOR `part` (0 VOCAL, 1 INST, 2 DRUMS) of channel `ch` as the strip holds it, 0..1, whatever the channel's mode.
pub(in crate::app) fn iso_at(app: &App, ch: i32, part: usize) -> Option<f32> {
    let row = app
        .global::<MixerState>()
        .get_channels()
        .row_data(usize::try_from(ch).ok()?)?;
    row.iso_knobs.row_data(*SLOT.get(part)?)
}

impl Mixer {
    /// The CFX type for the knob readout: a plugin effect reads like any type but FILTER (L / R sides).
    fn kind(&self) -> Kind {
        self.app.upgrade().map_or(Kind::Filter, |a| {
            Kind::from_index(a.global::<MixerState>().get_cfx_type() as usize)
        })
    }

    /// The CFX type's place in the selector: the [`Kind`]s, then the COLOR plugin effects.
    fn cfx_type(&self) -> usize {
        self.app.upgrade().map_or(0, |a| {
            a.global::<MixerState>().get_cfx_type().max(0) as usize
        })
    }

    pub(super) fn set_knob(&self, ch: usize, knob: usize, v: f32) {
        let row = self.row(ch);
        if row.iso && (map::HI..=map::LOW).contains(&knob) {
            let v = v.clamp(0.0, 1.0);
            row.iso_knobs.set_row_data(knob - map::HI, v);
            row.readouts.set_row_data(knob, map::iso_readout(v).into());
        } else {
            let v = map::snap(v);
            row.knobs.set_row_data(knob, v);
            row.readouts
                .set_row_data(knob, self.readout(knob, v).into());
        }
        self.apply(ch);
    }

    /// The DUAL PLAYER AUTO GAIN knob: the deck stores the offset (live and saved for the track), the channel is re-applied.
    pub(super) fn set_auto_gain(&self, ch: usize, v: f32) {
        self.decks[ch].set_gain_adjust(map::auto_adjust_db(v));
        self.show_auto(ch);
        self.apply(ch);
    }

    /// Copies the loaded track's AUTO GAIN (knob position, effective dB) into the channel row when it changed;
    /// returns the effective dB.
    pub(super) fn show_auto(&self, ch: usize) -> f32 {
        let now = (
            self.decks[ch].auto_gain_db(),
            self.decks[ch].gain_adjust_db(),
        );
        if self.shown[ch].replace(now) != now {
            self.update(ch, |row| {
                row.auto_gain = map::auto_knob(now.1);
                row.auto_gain_text = map::auto_readout(now.0).into();
            });
        }
        now.0
    }

    /// A CFX type chip: all channels switch type, and the knob readouts name the new type's sides.
    pub(super) fn select_cfx(&self, i: i32) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.global::<MixerState>()
            .set_cfx_type(i.clamp(0, cfx::names().len() as i32 - 1));
        for ch in 0..DECKS {
            let row = self.row(ch);
            let k = row.knobs.row_data(map::CFX).unwrap_or(map::CENTRE);
            row.readouts
                .set_row_data(map::CFX, self.readout(map::CFX, k).into());
            self.apply(ch);
        }
    }

    /// Pushes channel `ch`'s knobs, fader, CUE and crossfade side to its player.
    pub(super) fn apply(&self, ch: usize) {
        let row = self.row(ch);
        let cfg = self.cfg.get();
        let k = |i| row.knobs.row_data(i).unwrap_or(map::CENTRE);
        let (side, low_cut, held) = self.app.upgrade().map_or((1.0, false, false), |a| {
            let s = a.global::<MixerState>();
            (
                side_gain(row.xf, crossfade_gains(&s)),
                (s.get_auto_low_cut() >> ch) & 1 == 1,
                s.get_release_held(),
            )
        });
        self.release.set(held);
        let auto = auto_db(cfg.auto_gain, self.decks[ch].auto_gain_db());
        self.auto[ch].set(auto);
        let (trim_db, sel) = (map::trim_db(k(map::TRIM)) + auto, self.cfx_type());
        // STEM ISO: the HIGH / MID / LOW knobs are the stem levels (INST, VOCAL, DRUMS); the EQ keeps running underneath.
        let iso = row
            .iso
            .then(|| swap_vocal_inst([0, 1, 2].map(|i| row.iso_knobs.row_data(i).unwrap_or(1.0))));
        self.decks[ch].set_isolators(iso);
        // RELEASE FX held with "Apply RELEASE FX on CFX": the COLOR FX knob acts as centre meanwhile.
        let cfx = if cfg.release_cfx_off && held {
            0.0
        } else {
            map::cfx(k(map::CFX))
        };
        self.decks[ch].with_player(|p| {
            p.set_volume(gain(trim_db, curve::factor(cfg.fader, row.fader), side));
            // A plugin curve is the fader itself (the voice runs it before this gain): the curve factor above is 1.
            let (on, index) = (
                cfg.fader.plugin().is_some(),
                cfg.fader.plugin().unwrap_or(0),
            );
            p.fader().set_on(on);
            p.fader().set_index(index);
            p.fader().set_knob(row.fader);
            p.set_cue_gain(cue_gain(trim_db, row.cue));
            let [low, mid, high] = eq_dbs(&row, cfg.eq, low_cut);
            p.set_eq(low, mid, high);
            p.cfx().set_index(sel);
            p.cfx().set_knob(cfx);
        });
    }

    /// Text under a knob, in the current CFX and EQ type.
    fn readout(&self, knob: usize, k: f32) -> String {
        map::readout(self.kind(), self.cfg.get().eq, knob, k)
    }

    /// The HI / MID / LOW readouts of channel `ch` for its mode: the EQ type's dB, or the STEM ISO gains.
    pub(super) fn show_eq(&self, ch: usize) {
        let row = self.row(ch);
        for knob in map::HI..=map::LOW {
            let text = if row.iso {
                map::iso_readout(row.iso_knobs.row_data(knob - map::HI).unwrap_or(1.0))
            } else {
                self.readout(knob, row.knobs.row_data(knob).unwrap_or(map::CENTRE))
            };
            row.readouts.set_row_data(knob, text.into());
        }
    }

    /// EQ ↔ STEM ISO of a channel: the HI / MID / LOW knobs and readouts show the other set (each keeps its own positions).
    pub(super) fn toggle_iso(&self, ch: usize) {
        self.update(ch, |row| row.iso = !row.iso);
        self.show_eq(ch);
        self.apply(ch);
    }

    /// A controller's PART ISOLATOR knob (`part` 0 VOCAL, 1 INST, 2 DRUMS): the controller is in STEM mode, so the channel goes there too. Ignored while STEMS is off.
    pub(super) fn set_iso_knob(&self, ch: usize, part: usize, v: f32) {
        let cfg = self.cfg.get();
        if !cfg.stems || !cfg.iso_shown || part > 2 || ch >= DECKS {
            return;
        }
        if !self.row(ch).iso {
            self.toggle_iso(ch);
        }
        self.set_knob(ch, map::HI + SLOT[part], v);
    }

    /// A deck's part button or pad moved the rotary of `part` (0 VOCAL, 1 INST, 2 DRUMS) to `v`: the strip stays in its mode (EQ or STEM ISO).
    pub(super) fn set_part_level(&self, ch: usize, part: usize, v: f32) {
        if part > 2 || ch >= DECKS {
            return;
        }
        self.row(ch)
            .iso_knobs
            .set_row_data(SLOT[part], v.clamp(0.0, 1.0));
        self.show_eq(ch);
        self.apply(ch);
    }

    pub(super) fn toggle_cue(&self, ch: usize) {
        self.update(ch, |row| row.cue = !row.cue);
        self.apply(ch);
    }

    /// Click on `side`'s assignment grid (0 = A, 2 = B): an unassigned deck moves to that side, an assigned one goes to THRU (1).
    pub(super) fn assign_xf(&self, ch: usize, side: i32) {
        self.update(ch, |row| row.xf = if row.xf == side { 1 } else { side });
        self.apply(ch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knobs_top_to_bottom_are_inst_vocal_drums_and_the_player_gets_vocal_inst_drums() {
        let (inst, vocal, drums) = (0.2, 0.5, 0.9);
        assert_eq!(swap_vocal_inst([inst, vocal, drums]), [vocal, inst, drums]);
        assert_eq!(
            swap_vocal_inst(swap_vocal_inst([inst, vocal, drums])),
            [inst, vocal, drums]
        );
        // a controller's PART ISOLATOR for VOCAL (part 0) lands on the MID knob, INST (part 1) on HIGH
        assert_eq!((SLOT[0], SLOT[1], SLOT[2]), (1, 0, 2));
    }

    #[test]
    fn stem_iso_keeps_the_eq_the_dj_set_and_back_again() {
        let knobs = [map::CENTRE, map::CENTRE, map::CENTRE, 0.0, map::CENTRE];
        let mut row = ChannelView {
            knobs: slint::ModelRc::new(slint::VecModel::from(knobs.to_vec())),
            iso_knobs: slint::ModelRc::new(slint::VecModel::from(vec![1.0; 3])),
            ..Default::default()
        }; // LOW killed
        let eq = EqType::Isolator;
        let in_eq = eq_dbs(&row, eq, false);
        assert_eq!(in_eq, [EQ_KILL_DB, 0.0, 0.0]);
        row.iso = true;
        assert_eq!(
            eq_dbs(&row, eq, false),
            in_eq,
            "STEM ISO leaves the band cut"
        );
        row.iso = false;
        assert_eq!(
            eq_dbs(&row, eq, false),
            in_eq,
            "and back to EQ shows the same"
        );
    }
}
