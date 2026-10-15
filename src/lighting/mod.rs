//! LIGHTING mode core (no Slint): DMX fixtures, scenes picked by song phrase, beat-synced looks and the Art-Net / sACN output.
//!
//! Scenes are named `<MOOD> <PHRASE> <PALETTE>` (palettes COOL NATURAL HOT SUBTLE WARM VIVID CLUB 1 CLUB 2), each a
//! 32-beat macro per fixture group (par, bar, moving head, strobe, mirror ball, effect, laser).
//! The scenes, fixture profiles and DMX layouts here are our own, and the look maths are ours.
//!
//! - [`fixtures`]: built-in profiles (par RGB / RGBW, moving head, strobe) and user JSON profiles from the config dir.
//! - [`scene`]: palettes, the scene for each phrase kind and mood, and the beat maths of each pattern.
//! - [`rig`]: the patch ([`Rig`]), the controls (auto / ambient / solid / blackout, strobe) and one frame of looks → DMX.
//! - [`artnet`], [`sacn`]: packet encoders; [`output`]: the UDP sender; [`config`]: the saved settings.

pub mod artnet;
pub mod config;
pub mod fixtures;
pub mod output;
pub mod rig;
pub mod sacn;
pub mod scene;

pub use rig::{Control, Input, Mode, Rig, Strobe};

/// DMX slots in one universe.
pub const SLOTS: usize = 512;
/// Strobe rate (Hz) a fixture's strobe channel reaches at 255.
pub const MAX_STROBE_HZ: f32 = 25.0;

pub type Rgb = [u8; 3];
pub type Universe = [u8; SLOTS];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FixtureKind {
    Par,
    MovingHead,
    Strobe,
}

impl FixtureKind {
    pub const ALL: [Self; 3] = [Self::Par, Self::MovingHead, Self::Strobe];

    pub fn label(self) -> &'static str {
        match self {
            Self::Par => "par",
            Self::MovingHead => "moving head",
            Self::Strobe => "strobe",
        }
    }
}

/// What one DMX slot of a fixture does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chan {
    Dimmer,
    Red,
    Green,
    Blue,
    White,
    Strobe,
    Pan,
    Tilt,
    Fixed(u8),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Profile {
    pub name: String,
    pub kind: FixtureKind,
    pub channels: Vec<Chan>,
}

impl Profile {
    fn has(&self, c: Chan) -> bool {
        self.channels.contains(&c)
    }
}

/// A profile patched at DMX `address` (1-based) of `universe` (0-based on the wire).
#[derive(Clone, PartialEq, Debug)]
pub struct Fixture {
    pub name: String,
    pub profile: Profile,
    pub universe: u16,
    pub address: u16,
}

/// What a fixture should do right now. `strobe_hz` 0 = off; `gate` is the flash state of a beat-synced strobe
/// (true = lit), used for fixtures without a strobe channel and by the simulator.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Look {
    pub rgb: Rgb,
    pub dimmer: f32,
    pub strobe_hz: f32,
    pub gate: bool,
    pub pan: f32,
    pub tilt: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            rgb: [0; 3],
            dimmer: 0.0,
            strobe_hz: 0.0,
            gate: true,
            pan: 0.5,
            tilt: 0.5,
        }
    }
}

impl Look {
    /// The colour the stage simulator shows (dimmer and strobe gate applied).
    pub fn shown(&self) -> Rgb {
        let k = if self.gate {
            self.dimmer.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.rgb.map(|c| (f32::from(c) * k).round() as u8)
    }
}

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// DMX value of a strobe channel for `hz`: 0 = open (off), else 10..=255 (below 10 most fixtures still read "no strobe").
fn strobe_dmx(hz: f32) -> u8 {
    if hz <= 0.0 {
        0
    } else {
        (10.0 + (hz / MAX_STROBE_HZ).min(1.0) * 245.0).round() as u8
    }
}

impl Fixture {
    pub fn footprint(&self) -> u16 {
        self.profile.channels.len() as u16
    }

    /// Writes `look` into the fixture's slots of `u`. Fixtures without a dimmer slot get the dimmer baked into the colour,
    /// ones without a strobe slot get the gate baked into the dimmer; RGBW fixtures take the common part of R, G, B as white.
    pub fn write(&self, look: &Look, u: &mut Universe) {
        let p = &self.profile;
        let (dim, strobe) = (look.dimmer.clamp(0.0, 1.0), p.has(Chan::Strobe));
        let level = if strobe || look.gate { dim } else { 0.0 };
        let mut rgb = look.rgb;
        let white = if p.has(Chan::White) {
            *rgb.iter().min().unwrap()
        } else {
            0
        };
        rgb = rgb.map(|c| c - white);
        let scale = |c: u8| {
            if p.has(Chan::Dimmer) {
                c
            } else {
                (f32::from(c) * level).round() as u8
            }
        };
        for (i, ch) in p.channels.iter().enumerate() {
            let Some(slot) = u.get_mut(usize::from(self.address) - 1 + i) else {
                break;
            };
            *slot = match ch {
                Chan::Dimmer => byte(level),
                Chan::Red => scale(rgb[0]),
                Chan::Green => scale(rgb[1]),
                Chan::Blue => scale(rgb[2]),
                Chan::White => scale(white),
                Chan::Strobe => strobe_dmx(look.strobe_hz),
                Chan::Pan => byte(look.pan),
                Chan::Tilt => byte(look.tilt),
                Chan::Fixed(v) => *v,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(kind: FixtureKind, ch: &[Chan]) -> Fixture {
        Fixture {
            name: "t".into(),
            profile: Profile {
                name: "t".into(),
                kind,
                channels: ch.to_vec(),
            },
            universe: 0,
            address: 3,
        }
    }

    #[test]
    fn dimmerless_rgb_gets_the_dimmer_baked_in() {
        let mut u = [0; SLOTS];
        fx(FixtureKind::Par, &[Chan::Red, Chan::Green, Chan::Blue]).write(
            &Look {
                rgb: [200, 100, 50],
                dimmer: 0.5,
                ..Look::default()
            },
            &mut u,
        );
        assert_eq!(u[2..5], [100, 50, 25]);
        assert_eq!(u[..2], [0, 0]);
    }

    #[test]
    fn rgbw_takes_the_common_part_as_white_and_a_closed_gate_blacks_out() {
        let mut u = [0; SLOTS];
        let f = fx(
            FixtureKind::Par,
            &[
                Chan::Dimmer,
                Chan::Red,
                Chan::Green,
                Chan::Blue,
                Chan::White,
                Chan::Fixed(7),
            ],
        );
        f.write(
            &Look {
                rgb: [255, 100, 100],
                dimmer: 1.0,
                ..Look::default()
            },
            &mut u,
        );
        assert_eq!(u[2..8], [255, 155, 0, 0, 100, 7]);
        f.write(
            &Look {
                rgb: [255, 100, 100],
                dimmer: 1.0,
                gate: false,
                ..Look::default()
            },
            &mut u,
        );
        assert_eq!(
            u[2..8],
            [0, 155, 0, 0, 100, 7],
            "dimmer slot closes; colour slots stay (the dimmer does the blackout)"
        );
    }

    #[test]
    fn strobe_slot_takes_hz_and_ignores_the_gate() {
        let mut u = [0; SLOTS];
        let f = fx(FixtureKind::Strobe, &[Chan::Dimmer, Chan::Strobe]);
        f.write(
            &Look {
                dimmer: 1.0,
                strobe_hz: MAX_STROBE_HZ,
                gate: false,
                ..Look::default()
            },
            &mut u,
        );
        assert_eq!(u[2..4], [255, 255]);
        f.write(
            &Look {
                dimmer: 1.0,
                ..Look::default()
            },
            &mut u,
        );
        assert_eq!(u[3], 0);
    }

    #[test]
    fn moving_head_writes_position() {
        let mut u = [0; SLOTS];
        fx(FixtureKind::MovingHead, &[Chan::Pan, Chan::Tilt]).write(
            &Look {
                pan: 1.0,
                tilt: 0.0,
                ..Look::default()
            },
            &mut u,
        );
        assert_eq!(u[2..4], [255, 0]);
    }
}
