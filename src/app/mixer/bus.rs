//! The master / headphone section: MASTER, HP MIX and HP LEVEL knobs, SPLIT, pushed to the shared `audio::Bus`.

use super::{Mixer, MixerState, map};
use openfader::audio::{Bus, Headphones};
use slint::{ComponentHandle, Model};

impl Mixer {
    pub(super) fn set_bus_knob(&self, knob: usize, v: f32) {
        let v = map::snap(v);
        self.edit_bus(|b| {
            b.knobs.set_row_data(knob, v);
            b.readouts
                .set_row_data(knob, map::bus_readout(knob, v).into());
        });
        self.apply_bus();
    }

    pub(super) fn toggle_split(&self) {
        if matches!(
            self.bus.as_deref().map(Bus::headphones),
            Some(Headphones::Device(_))
        ) {
            return self.set_status(tr!("SPLIT is off while a headphone device is in use").into());
        }
        self.edit_bus(|b| b.split = !b.split);
        self.apply_bus();
    }

    /// Pushes MASTER, HP MIX, HP LEVEL and SPLIT to the audio bus.
    pub(super) fn apply_bus(&self) {
        let (Some(bus), Some(app)) = (&self.bus, self.app.upgrade()) else {
            return;
        };
        let view = app.global::<MixerState>().get_bus();
        let k = |i: usize| view.knobs.row_data(i).unwrap_or(map::BUS_START[i]);
        bus.set_master(map::db_to_gain(map::bus_db(k(map::MASTER))) * self.cfg.get().output);
        bus.set_headphone(
            k(map::HP_MIX),
            map::db_to_gain(map::bus_db(k(map::HP_LEVEL))),
        );
        bus.set_split(view.split);
    }
}
