//! Palettes, the scene for each phrase, and the beat maths of each pattern. Everything here is a pure function of the
//! beat position, so a look is reproducible from the master deck's beat grid alone.
//!
//! Scene choice (ours): the phrase kind picks the pattern, the mood (High / Mid / Low) scales its brightness.

use super::{FixtureKind, Look, Rgb};
use crate::analysis::{Mood, PhraseKind as Kind, Phrases};
use std::f64::consts::TAU;

/// The pad names of `pad/Lighting.pad.csv`; the colours are ours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Palette {
    Cool,
    Natural,
    Hot,
    Subtle,
    Warm,
    Vivid,
    Club1,
    Club2,
}

impl Palette {
    pub const ALL: [Self; 8] = [
        Self::Cool,
        Self::Natural,
        Self::Hot,
        Self::Subtle,
        Self::Warm,
        Self::Vivid,
        Self::Club1,
        Self::Club2,
    ];

    pub fn label(self) -> &'static str {
        [
            "COOL", "NATURAL", "HOT", "SUBTLE", "WARM", "VIVID", "CLUB 1", "CLUB 2",
        ][self as usize]
    }

    pub fn colours(self) -> [Rgb; 3] {
        match self {
            Self::Cool => [[0, 90, 255], [0, 200, 255], [120, 80, 255]],
            Self::Natural => [[255, 190, 120], [255, 240, 220], [120, 200, 140]],
            Self::Hot => [[255, 30, 0], [255, 120, 0], [255, 0, 90]],
            Self::Subtle => [[110, 90, 160], [90, 130, 150], [150, 110, 120]],
            Self::Warm => [[255, 140, 30], [255, 70, 20], [255, 200, 90]],
            Self::Vivid => [[255, 0, 120], [0, 230, 120], [40, 80, 255]],
            Self::Club1 => [[170, 0, 255], [0, 60, 255], [255, 0, 150]],
            Self::Club2 => [[0, 255, 60], [255, 255, 0], [255, 0, 40]],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    /// Slow swell over 8 beats.
    Breathe,
    /// Every fixture flashes on the beat and decays.
    Pulse,
    /// Even and odd fixtures take turns each beat.
    Alternate,
    /// One fixture lit per eighth note, with a tail.
    Chase,
    /// A wave running across the fixtures.
    Sweep,
    /// Brightness grows over the phrase, flashing on the beat.
    Build,
    /// Steady glow fading out over the phrase.
    Fade,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Scene {
    pub pattern: Pattern,
    /// Peak brightness, 0..1.
    pub energy: f32,
    /// Strobe over the last `.0` beats of the phrase, `.1` flashes per beat (the fill-in / build-up).
    pub strobe_tail: Option<(f64, f64)>,
}

/// The scene for a phrase: `kind` picks the pattern, `mood` the brightness.
pub fn scene(kind: Kind, mood: Mood) -> Scene {
    let (pattern, energy, strobe_tail) = match kind {
        Kind::Intro => (Pattern::Breathe, 0.5, None),
        Kind::Verse => (Pattern::Alternate, 0.7, None),
        Kind::Bridge => (Pattern::Sweep, 0.7, None),
        Kind::Up => (Pattern::Build, 0.85, Some((4.0, 2.0))),
        Kind::Chorus => (Pattern::Chase, 1.0, None),
        Kind::Down => (Pattern::Pulse, 1.0, None),
        Kind::Outro => (Pattern::Fade, 0.6, None),
    };
    let k = match mood {
        Mood::High => 1.0,
        Mood::Mid => 0.85,
        Mood::Low => 0.7,
    };
    Scene {
        pattern,
        energy: energy * k,
        strobe_tail,
    }
}

/// The scene shown without a phrase (no master track, or one without phrases).
pub const AMBIENT: Scene = Scene {
    pattern: Pattern::Breathe,
    energy: 0.6,
    strobe_tail: None,
};

/// Where the master deck is in its song structure.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PhraseCtx {
    pub kind: Kind,
    pub mood: Mood,
    /// 0..1 through the phrase.
    pub progress: f64,
    /// Beats left until the phrase ends.
    pub to_end: f64,
}

impl PhraseCtx {
    /// The phrase containing `beat` (0-based fractional index into the grid; phrase beats are 1-based), if any.
    pub fn at(phrases: &Phrases, beat: f64) -> Option<Self> {
        let b = beat + 1.0;
        let p = phrases
            .list
            .iter()
            .find(|p| p.beat as f64 <= b && b < p.end_beat as f64)?;
        let len = f64::from(p.end_beat - p.beat).max(1.0);
        Some(Self {
            kind: p.kind,
            mood: phrases.mood,
            progress: (b - f64::from(p.beat)) / len,
            to_end: f64::from(p.end_beat) - b,
        })
    }
}

/// Which fixture of `n` of one kind is lit by [`Pattern::Chase`] at `beat` (one step per eighth note).
pub fn chase_step(beat: f64, n: usize) -> usize {
    (beat * 2.0).floor().rem_euclid(n.max(1) as f64) as usize
}

/// The strobe flash state at `beat`: `per_beat` flashes per beat, lit for the first quarter of each.
pub fn strobe_gate(beat: f64, per_beat: f64) -> bool {
    (beat * per_beat).rem_euclid(1.0) < 0.25
}

/// The look of fixture `i` of `n` (of `kind`) in `scene` with `palette` at `beat`; `ctx` is the phrase position (progress 0, far
/// from its end when there is none).
pub fn look(
    scene: &Scene,
    palette: Palette,
    kind: FixtureKind,
    (i, n): (usize, usize),
    beat: f64,
    ctx: &PhraseCtx,
) -> Look {
    let pos = i as f64 / n.max(1) as f64;
    let frac = beat.rem_euclid(1.0);
    let hit = (1.0 - frac).powi(2);
    let e = f64::from(scene.energy);
    let level = e * match scene.pattern {
        Pattern::Breathe => 0.25 + 0.75 * (0.5 + 0.5 * (TAU * beat / 8.0 - TAU / 4.0).sin()),
        Pattern::Pulse => 0.2 + 0.8 * hit,
        Pattern::Alternate => {
            if (beat.floor() as i64 + i as i64) % 2 == 0 {
                0.25 + 0.75 * hit
            } else {
                0.1
            }
        }
        Pattern::Chase => {
            let steps_ago = (chase_step(beat, n) + n - i % n) % n;
            (1.0 - 0.35 * steps_ago as f64).max(0.05)
        }
        Pattern::Sweep => 0.15 + 0.85 * (0.5 + 0.5 * (TAU * (beat / 4.0 - pos)).sin()),
        Pattern::Build => (0.3 + 0.7 * ctx.progress) * (0.6 + 0.4 * hit),
        Pattern::Fade => 1.0 - ctx.progress,
    };
    let colours = palette.colours();
    let rgb = colours[((beat / 8.0).floor() as i64 + i as i64).rem_euclid(3) as usize];
    let mut l = Look {
        rgb,
        dimmer: level as f32,
        ..Look::default()
    };
    if kind == FixtureKind::MovingHead {
        l.pan = (0.5 + 0.3 * (TAU * (beat / 8.0 + pos)).sin()) as f32;
        l.tilt = (0.5 + 0.2 * (TAU * beat / 4.0).cos()) as f32;
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Phrase;

    fn phrase(beat: u32, end_beat: u32, kind: Kind) -> Phrase {
        Phrase {
            beat,
            end_beat,
            kind,
            variant: 0,
            fill: None,
        }
    }

    #[test]
    fn scene_follows_the_phrase_kind_and_mood_scales_it() {
        let want = [
            (Kind::Intro, Pattern::Breathe),
            (Kind::Verse, Pattern::Alternate),
            (Kind::Bridge, Pattern::Sweep),
            (Kind::Up, Pattern::Build),
            (Kind::Chorus, Pattern::Chase),
            (Kind::Down, Pattern::Pulse),
            (Kind::Outro, Pattern::Fade),
        ];
        for (k, p) in want {
            assert_eq!(scene(k, Mood::High).pattern, p);
        }
        assert!(scene(Kind::Chorus, Mood::High).energy > scene(Kind::Chorus, Mood::Low).energy);
        assert!(
            scene(Kind::Up, Mood::Mid).strobe_tail.is_some()
                && scene(Kind::Down, Mood::Mid).strobe_tail.is_none()
        );
    }

    #[test]
    fn phrase_lookup_maps_grid_beats_to_one_based_phrases() {
        let ps = Phrases {
            mood: Mood::Mid,
            list: vec![phrase(1, 33, Kind::Intro), phrase(33, 65, Kind::Chorus)],
        };
        let at = |b| PhraseCtx::at(&ps, b);
        assert_eq!(at(0.0).unwrap().kind, Kind::Intro);
        assert_eq!(at(31.9).unwrap().kind, Kind::Intro);
        assert_eq!(
            at(32.0).unwrap().kind,
            Kind::Chorus,
            "grid index 32 is beat 33"
        );
        let c = at(48.0).unwrap();
        assert!((c.progress - 0.5).abs() < 1e-9 && (c.to_end - 16.0).abs() < 1e-9);
        assert!(at(64.0).is_none() && at(-2.0).is_none());
    }

    #[test]
    fn chase_advances_one_fixture_per_eighth_note_at_the_grid_tempo() {
        // 120 BPM: 500 ms a beat, so a step every 250 ms; 4 fixtures wrap after 1 s.
        let lit = |ms: f64| chase_step(ms / 500.0, 4);
        assert_eq!(
            [0.0, 249.0, 250.0, 500.0, 750.0, 999.0, 1000.0].map(lit),
            [0, 0, 1, 2, 3, 3, 0]
        );
        // the same beats at 90 BPM (667 ms) stretch the step to 333 ms
        assert_eq!(chase_step(333.0 / (60_000.0 / 90.0), 4), 0);
        assert_eq!(chase_step(334.0 / (60_000.0 / 90.0), 4), 1);
        let ctx = PhraseCtx {
            kind: Kind::Chorus,
            mood: Mood::High,
            progress: 0.0,
            to_end: 99.0,
        };
        let s = scene(Kind::Chorus, Mood::High);
        let d = |i, beat| look(&s, Palette::Cool, FixtureKind::Par, (i, 4), beat, &ctx).dimmer;
        assert!(
            d(1, 0.5) > d(0, 0.5) && d(1, 0.5) > d(2, 0.5),
            "the lit fixture is the brightest, the one before it its tail"
        );
        assert_eq!(d(1, 0.5), s.energy);
    }

    #[test]
    fn strobe_flashes_in_step_with_the_beat() {
        assert!(
            strobe_gate(0.0, 2.0)
                && !strobe_gate(0.25, 2.0)
                && strobe_gate(0.5, 2.0)
                && strobe_gate(3.0, 2.0)
        );
        assert!(strobe_gate(0.2, 1.0) && !strobe_gate(0.3, 1.0));
    }

    #[test]
    fn pulse_peaks_on_the_beat_and_moving_heads_move() {
        let ctx = PhraseCtx {
            kind: Kind::Down,
            mood: Mood::High,
            progress: 0.0,
            to_end: 9.0,
        };
        let s = scene(Kind::Down, Mood::High);
        let l = |kind, beat| look(&s, Palette::Hot, kind, (0, 2), beat, &ctx);
        assert!(l(FixtureKind::Par, 4.0).dimmer > l(FixtureKind::Par, 4.9).dimmer);
        assert_ne!(
            l(FixtureKind::MovingHead, 1.0).pan,
            l(FixtureKind::MovingHead, 2.0).pan
        );
        assert_eq!(l(FixtureKind::Par, 1.0).pan, 0.5);
    }
}
