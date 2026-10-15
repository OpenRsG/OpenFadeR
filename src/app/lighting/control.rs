//! Control chips: mode, strobe, palette and master level ↔ `LightingState`, and the ADD FIXTURE profile picker.

use super::{Lighting, index_of};
use openfader::lighting::{self, Control, Mode, Strobe, scene::Palette};

const SOLIDS: [lighting::Rgb; 4] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];

/// The pad / mode chip index of ui/lighting.slint → control mode.
pub(super) fn mode_of(i: i32) -> Mode {
    match i {
        1 => Mode::Ambient,
        2..=5 => Mode::Solid(SOLIDS[(i - 2) as usize]),
        6 => Mode::BlackOut,
        _ => Mode::Auto,
    }
}

pub(super) fn strobe_of(i: i32) -> Strobe {
    [Strobe::Off, Strobe::Slow, Strobe::Fast][i.clamp(0, 2) as usize]
}

impl Lighting {
    /// Applies `f` to the live control and mirrors it to the UI.
    pub(super) fn update_control(&self, f: impl FnOnce(&mut Control)) {
        let mut c = self.ctl.get();
        f(&mut c);
        self.ctl.set(c);
        self.show_controls();
    }

    pub(super) fn show_controls(&self) {
        let c = self.ctl.get();
        self.state(|s| {
            s.set_palette(index_of(&Palette::ALL, &c.palette));
            s.set_mode(match c.mode {
                Mode::Auto => 0,
                Mode::Ambient => 1,
                Mode::Solid(rgb) => 2 + index_of(&SOLIDS, &rgb),
                Mode::BlackOut => 6,
            });
            s.set_strobe(c.strobe as i32);
            s.set_master(c.master);
        });
    }

    pub(super) fn show_profile(&self) {
        let name = self.profiles[self.pick.get()].name.clone();
        self.state(|s| s.set_profile(name.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_indices_map_to_modes() {
        assert_eq!(
            [0, 1, 2, 5, 6, 9].map(mode_of),
            [
                Mode::Auto,
                Mode::Ambient,
                Mode::Solid([255, 0, 0]),
                Mode::Solid([255; 3]),
                Mode::BlackOut,
                Mode::Auto
            ]
        );
        assert_eq!(
            [0, 1, 2, 7].map(strobe_of),
            [Strobe::Off, Strobe::Slow, Strobe::Fast, Strobe::Fast]
        );
    }
}
