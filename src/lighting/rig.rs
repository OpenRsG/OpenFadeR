//! The patch ([`Rig`]), the live controls ([`Control`]: auto / ambient / solid colour / blackout plus strobe) and the step
//! from controls + beat position to per-fixture [`Look`]s and DMX universes.

use super::scene::{self, AMBIENT, Palette, PhraseCtx};
use super::{Fixture, FixtureKind, Look, Profile, Rgb, SLOTS, Universe};
use crate::analysis::{Mood, PhraseKind};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// Scene of the master deck's current phrase.
    Auto,
    /// The palette's slow glow whatever plays (the pad's COOL … CLUB 2).
    Ambient,
    /// Every fixture one colour (the pad's RED / GREEN / BLUE / WHITE).
    Solid(Rgb),
    BlackOut,
}

/// Manual strobe over whatever the mode shows (the pad's STROBE FAST / SLOW / OFF).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strobe {
    Off,
    /// One flash per beat.
    Slow,
    /// Four flashes per beat.
    Fast,
}

impl Strobe {
    fn per_beat(self) -> Option<f64> {
        match self {
            Self::Off => None,
            Self::Slow => Some(1.0),
            Self::Fast => Some(4.0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Control {
    pub mode: Mode,
    pub palette: Palette,
    pub strobe: Strobe,
    /// Grand master, 0..1.
    pub master: f32,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            palette: Palette::Vivid,
            strobe: Strobe::Off,
            master: 1.0,
        }
    }
}

/// What the master deck says this instant.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Input {
    /// Beat position, 0-based fractional grid index (free-running when nothing plays).
    pub beat: f64,
    pub bpm: f64,
    pub phrase: Option<PhraseCtx>,
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Rig {
    pub fixtures: Vec<Fixture>,
}

impl Rig {
    /// Patches `profile` at the first free address after the existing fixtures (spilling into the next universe), named
    /// "<profile name> <n>"; `None` when the profile cannot fit a universe.
    pub fn add(&mut self, profile: &Profile) -> Option<usize> {
        let fp = profile.channels.len();
        let (universe, address) = (0u16..).take(64).find_map(|u| {
            let next = self
                .fixtures
                .iter()
                .filter(|f| f.universe == u)
                .map(|f| usize::from(f.address + f.footprint()))
                .max()
                .unwrap_or(1);
            (next + fp - 1 <= SLOTS).then_some((u, next as u16))
        })?;
        let n = self
            .fixtures
            .iter()
            .filter(|f| f.profile.name == profile.name)
            .count()
            + 1;
        self.fixtures.push(Fixture {
            name: format!("{} {n}", profile.name),
            profile: profile.clone(),
            universe,
            address,
        });
        Some(self.fixtures.len() - 1)
    }

    /// Moves fixture `i` to `address` of `universe`; the whole footprint must fit the universe.
    pub fn set_address(&mut self, i: usize, universe: u16, address: u16) -> Result<(), String> {
        let f = self.fixtures.get_mut(i).ok_or("no such fixture")?;
        if address == 0 || usize::from(address) + usize::from(f.footprint()) - 1 > SLOTS {
            return Err(format!(
                "{} channels from {address} do not fit a universe",
                f.footprint()
            ));
        }
        (f.universe, f.address) = (universe, address);
        Ok(())
    }

    /// Fixtures sharing a slot with an earlier one (an overlap is allowed for twin fixtures, but the UI flags it).
    pub fn conflicts(&self) -> Vec<bool> {
        let span = |f: &Fixture| (f.universe, f.address, f.address + f.footprint());
        (0..self.fixtures.len())
            .map(|i| {
                let (u, a, e) = span(&self.fixtures[i]);
                self.fixtures[..i]
                    .iter()
                    .map(span)
                    .any(|(u2, a2, e2)| u == u2 && a < e2 && a2 < e)
            })
            .collect()
    }

    /// One look per fixture for `ctl` at `inp`.
    pub fn looks(&self, ctl: &Control, inp: &Input) -> Vec<Look> {
        let ctx = inp.phrase.filter(|_| ctl.mode == Mode::Auto);
        let scene = ctx.map_or(AMBIENT, |p| scene::scene(p.kind, p.mood));
        let ctx = ctx.unwrap_or(PhraseCtx {
            kind: PhraseKind::Intro,
            mood: Mood::Mid,
            progress: 0.0,
            to_end: f64::INFINITY,
        });
        let strobe = ctl.strobe.per_beat().or_else(|| {
            scene
                .strobe_tail
                .filter(|&(tail, _)| ctx.to_end <= tail)
                .map(|(_, per)| per)
        });
        let strobe = strobe.filter(|_| ctl.mode != Mode::BlackOut);
        let mut count = [0usize; FixtureKind::ALL.len()];
        for f in &self.fixtures {
            count[f.profile.kind as usize] += 1;
        }
        let mut seen = [0usize; FixtureKind::ALL.len()];
        self.fixtures
            .iter()
            .map(|f| {
                let kind = f.profile.kind;
                let (i, n) = (seen[kind as usize], count[kind as usize]);
                seen[kind as usize] += 1;
                let mut l = match ctl.mode {
                    Mode::BlackOut => Look::default(),
                    Mode::Solid(rgb) => Look {
                        rgb,
                        dimmer: 1.0,
                        ..Look::default()
                    },
                    Mode::Auto | Mode::Ambient => {
                        let s = if ctl.mode == Mode::Ambient {
                            AMBIENT
                        } else {
                            scene
                        };
                        scene::look(&s, ctl.palette, kind, (i, n), inp.beat, &ctx)
                    }
                };
                if kind == FixtureKind::Strobe {
                    l.rgb = [255; 3];
                    l.dimmer = if strobe.is_some() { 1.0 } else { 0.0 };
                }
                if let Some(per_beat) = strobe {
                    l.strobe_hz = (per_beat * inp.bpm / 60.0) as f32;
                    l.gate = scene::strobe_gate(inp.beat, per_beat);
                    l.dimmer = l.dimmer.max(0.7);
                }
                l.dimmer *= ctl.master.clamp(0.0, 1.0);
                l
            })
            .collect()
    }

    /// DMX universes (by universe number) of `looks` (one per fixture, as from [`Rig::looks`]).
    pub fn frames(&self, looks: &[Look]) -> BTreeMap<u16, Universe> {
        let mut out = BTreeMap::new();
        for (f, l) in self.fixtures.iter().zip(looks) {
            f.write(l, out.entry(f.universe).or_insert([0; SLOTS]));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Mood, PhraseKind as Kind};
    use crate::lighting::fixtures::builtin;

    fn rig() -> Rig {
        let b = builtin();
        let mut r = Rig::default();
        for _ in 0..3 {
            r.add(&b[0]); // 3-channel RGB pars
        }
        r.add(&b[4]); // strobe
        r
    }

    fn input(beat: f64, kind: Option<(Kind, f64)>) -> Input {
        Input {
            beat,
            bpm: 120.0,
            phrase: kind.map(|(kind, to_end)| PhraseCtx {
                kind,
                mood: Mood::High,
                progress: 0.5,
                to_end,
            }),
        }
    }

    #[test]
    fn addresses_pack_one_after_another_and_spill_into_the_next_universe() {
        let r = rig();
        assert_eq!(
            r.fixtures
                .iter()
                .map(|f| (f.universe, f.address))
                .collect::<Vec<_>>(),
            [(0, 1), (0, 4), (0, 7), (0, 10)]
        );
        let mut big = Rig::default();
        let wide = Profile {
            name: "wide".into(),
            kind: FixtureKind::Par,
            channels: vec![crate::lighting::Chan::Dimmer; 300],
        };
        for _ in 0..3 {
            big.add(&wide);
        }
        assert_eq!(
            big.fixtures
                .iter()
                .map(|f| (f.universe, f.address))
                .collect::<Vec<_>>(),
            [(0, 1), (1, 1), (2, 1)]
        );
        assert_eq!(big.fixtures[1].name, "wide 2");
    }

    #[test]
    fn set_address_checks_the_footprint_and_conflicts_are_flagged() {
        let mut r = rig();
        assert!(r.set_address(0, 0, 511).is_err() && r.set_address(0, 0, 0).is_err());
        assert!(r.set_address(0, 0, 510).is_ok());
        assert_eq!(r.conflicts(), [false; 4]);
        r.set_address(1, 0, 510).unwrap();
        assert_eq!(r.conflicts(), [false, true, false, false]);
    }

    #[test]
    fn auto_uses_the_phrase_scene_and_ambient_ignores_it() {
        let r = rig();
        let auto = Control {
            master: 1.0,
            ..Control::default()
        };
        let intro = r.looks(&auto, &input(0.0, Some((Kind::Intro, 99.0))));
        let chorus = r.looks(&auto, &input(0.0, Some((Kind::Chorus, 99.0))));
        assert_ne!(
            intro[0].dimmer, chorus[0].dimmer,
            "Breathe starts dim, Chase's first fixture is at peak"
        );
        let amb = Control {
            mode: Mode::Ambient,
            ..auto
        };
        assert_eq!(
            r.looks(&amb, &input(0.0, Some((Kind::Chorus, 99.0)))),
            r.looks(&amb, &input(0.0, None))
        );
        assert_eq!(
            r.looks(&auto, &input(0.0, None)),
            r.looks(&amb, &input(0.0, None)),
            "no phrase = ambient"
        );
    }

    #[test]
    fn solid_blackout_and_master() {
        let r = rig();
        let i = input(3.0, None);
        let solid = r.looks(
            &Control {
                mode: Mode::Solid([255, 0, 0]),
                master: 0.5,
                ..Control::default()
            },
            &i,
        );
        assert_eq!(
            (solid[0].rgb, solid[0].dimmer, solid[3].dimmer),
            ([255, 0, 0], 0.5, 0.0),
            "the strobe fixture stays dark"
        );
        let black = r.looks(
            &Control {
                mode: Mode::BlackOut,
                strobe: Strobe::Fast,
                ..Control::default()
            },
            &i,
        );
        assert!(black.iter().all(|l| l.dimmer == 0.0 && l.strobe_hz == 0.0));
        let f = r.frames(&solid);
        assert_eq!(f[&0][..12], [128, 0, 0, 128, 0, 0, 128, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn strobe_runs_from_the_pad_or_the_end_of_an_up_phrase_in_beat_time() {
        let r = rig();
        let base = Control {
            mode: Mode::Solid([0, 0, 255]),
            ..Control::default()
        };
        let fast = r.looks(
            &Control {
                strobe: Strobe::Fast,
                ..base
            },
            &input(0.0, None),
        );
        assert!(
            fast[3].dimmer > 0.0 && fast[3].strobe_hz == 8.0,
            "4 flashes per beat at 120 BPM = 8 Hz"
        );
        let gates = [0.0, 0.125, 0.25, 0.375].map(|b| {
            r.looks(
                &Control {
                    strobe: Strobe::Fast,
                    ..base
                },
                &input(b, None),
            )[0]
            .gate
        });
        assert_eq!(gates, [true, false, true, false]);
        let auto = Control::default();
        let early = r.looks(&auto, &input(10.0, Some((Kind::Up, 8.0))));
        let late = r.looks(&auto, &input(14.0, Some((Kind::Up, 3.0))));
        assert!(
            early[3].strobe_hz == 0.0 && late[3].strobe_hz == 4.0,
            "Up strobes 2 per beat in its last 4 beats"
        );
    }
}
