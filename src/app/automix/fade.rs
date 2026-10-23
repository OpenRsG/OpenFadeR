//! One running transition: a style's [`Timeline`] played on a [`Rig`] as beats go by. The beat clock is the driver's (wall
//! time at the outgoing deck's tempo), not the outgoing deck's position, so a style that loops, brakes or stops that deck
//! keeps its time.

use openfader::mixstyle::{Event, Side, State, Timeline};

/// What a transition moves. [`super::rig::Live`] is the mixer, the decks and the effect unit; the tests use a recorder.
pub(super) trait Rig {
    /// Puts the controls `s` names on the mixer. The first time a control is touched, the DJ's own setting is remembered.
    fn show(&mut self, s: &State);
    /// A one-shot move on one deck; `beat_secs` is how long a beat lasts now.
    fn fire(&mut self, side: Side, e: &Event, beat_secs: f64);
    /// Gives back what was taken: the DJ's knobs and fader, the COLOR FX type, the effect unit, the loops.
    fn restore(&mut self);
}

pub(super) struct Fade<R> {
    tl: Timeline,
    /// Beats played since the transition began.
    beat: f64,
    /// The first one-shot move not made yet.
    next: usize,
    last: Option<State>,
    rig: R,
}

impl<R: Rig> Fade<R> {
    /// A transition of `tl`, `beat` beats in already (the driver notices the mix point a tick late).
    pub fn new(tl: Timeline, beat: f64, rig: R) -> Self {
        Fade {
            tl,
            beat,
            next: 0,
            last: None,
            rig,
        }
    }

    /// Plays `beats` more: moves the controls to where they stand now and makes the one-shot moves that came due. `true`
    /// once the transition's length has passed.
    pub fn advance(&mut self, beats: f64, beat_secs: f64) -> bool {
        self.beat += beats;
        let at = self.beat.min(self.tl.len());
        let s = self.tl.at(at);
        if self.last != Some(s) {
            self.rig.show(&s);
            self.last = Some(s);
        }
        while let Some((b, side, e)) = self.tl.events().get(self.next) {
            if *b > at {
                break;
            }
            self.rig.fire(*side, e, beat_secs);
            self.next += 1;
        }
        self.beat >= self.tl.len()
    }

    /// How far the transition is, 0..1.
    pub fn progress(&self) -> f64 {
        (self.beat / self.tl.len()).clamp(0.0, 1.0)
    }

    /// The transition is over (or abandoned): everything goes back to the DJ.
    pub fn end(mut self) {
        self.rig.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::{mixstyle::Ctl, plugin::Manifest};

    /// Records what the transition did.
    #[derive(Default)]
    struct Sim {
        shown: Vec<State>,
        fired: Vec<(Side, Event, f64)>,
        restored: u32,
    }

    impl Rig for &mut Sim {
        fn show(&mut self, s: &State) {
            self.shown.push(*s);
        }
        fn fire(&mut self, side: Side, e: &Event, beat_secs: f64) {
            self.fired.push((side, e.clone(), beat_secs));
        }
        fn restore(&mut self) {
            self.restored += 1;
        }
    }

    /// A plugin.json with one style, read as a plugin's manifest is.
    fn plugin_style() -> openfader::mixstyle::Recipe {
        let m: Manifest = serde_json::from_str(
            r#"{"name": "demo", "automix": [{"name": "THROW", "bars": 2, "steps": [
                {"at": 0, "deck": "out", "loop": 2},
                {"at": 0, "over": "50%", "deck": "out", "fader": 0},
                {"at": "50%", "deck": "in", "high": 1},
                {"at": -1, "deck": "out", "brake": 1},
                {"at": 0, "over": "100%", "crossfader": 1}]}]}"#,
        )
        .unwrap();
        openfader::mixstyle::validate_all(&m.automix).unwrap();
        m.automix[0].clone()
    }

    #[test]
    fn a_plugin_recipe_runs_its_controls_and_one_shots_on_time_and_hands_everything_back() {
        let r = plugin_style();
        let mut sim = Sim::default();
        let mut fade = Fade::new(r.timeline(8.0), 0.0, &mut sim);
        let mut done_at = None;
        for beat in 1..=10 {
            if fade.advance(1.0, 0.5) && done_at.is_none() {
                done_at = Some(beat);
            }
        }
        assert_eq!(done_at, Some(8), "over once the 8 beats have passed");
        fade.end();
        assert_eq!(sim.restored, 1);
        // each one-shot exactly once, in time order, with the beat length of that moment
        assert_eq!(
            sim.fired,
            [
                (Side::Out, Event::Loop(2.0), 0.5),
                (Side::Out, Event::Brake(1.0), 0.5)
            ]
        );
        // the controls followed the recipe: the outgoing fader closes over the first 4 beats, the incoming highs open at 4
        let at = |n: usize| sim.shown[n];
        assert_eq!(at(0).get(Side::Out, Ctl::Fader), Some(0.75));
        assert_eq!(at(3).get(Side::Out, Ctl::Fader), Some(0.0));
        assert_eq!(at(2).get(Side::In, Ctl::High), Some(0.5));
        assert_eq!(at(3).get(Side::In, Ctl::High), Some(1.0));
        assert_eq!(sim.shown.last().unwrap().crossfader, 1.0);
    }

    #[test]
    fn a_one_shot_at_the_first_beat_fires_on_the_first_step_and_a_late_start_catches_up() {
        let r = plugin_style();
        let mut sim = Sim::default();
        let mut fade = Fade::new(r.timeline(8.0), 0.0, &mut sim);
        assert!(!fade.advance(0.02, 0.5));
        assert_eq!(fade.progress(), 0.0025);
        fade.end();
        assert_eq!(sim.fired.len(), 1, "the loop at beat 0");
        // started 7.5 beats late: the brake at the last beat is not skipped
        let mut late = Sim::default();
        let mut fade = Fade::new(r.timeline(8.0), 7.5, &mut late);
        assert!(fade.advance(0.6, 0.5));
        assert_eq!(late.fired.len(), 2);
    }

    #[test]
    fn unchanged_controls_are_not_written_again() {
        let r = plugin_style();
        let mut sim = Sim::default();
        let mut fade = Fade::new(r.timeline(8.0), 4.0, &mut sim);
        fade.advance(0.0, 0.5);
        fade.advance(0.0, 0.5); // the fader finished closing, the crossfader still moves: no change in a still beat
        let mut still = Sim::default();
        let cut = openfader::mixstyle::builtin()
            .iter()
            .find(|r| r.name == "CUT")
            .unwrap();
        let mut fade = Fade::new(cut.timeline(4.0), 0.0, &mut still);
        for _ in 0..3 {
            fade.advance(0.5, 0.5);
        }
        assert_eq!(still.shown.len(), 1, "CUT is one move");
    }
}
