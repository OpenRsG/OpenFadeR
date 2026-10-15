//! The 40 Hz loop: deck input → looks → DMX frames → stage simulator.

use super::Lighting;
use crate::app::{
    DeckState,
    deck::{DECKS, Decks},
};
use openfader::lighting::{Control, Input, Mode, scene};
use slint::{ComponentHandle, Model, SharedString};
use std::time::Duration;

pub(super) const TICK: Duration = Duration::from_millis(25);
/// Beat clock of the stage while no deck plays.
const IDLE_BPM: f64 = 120.0;

/// The first deck (master first, then A to D) that plays on a grid.
fn master_input(decks: &Decks, master: i32) -> Option<Input> {
    let first = usize::try_from(master).ok().filter(|&m| m < DECKS);
    first
        .into_iter()
        .chain(0..DECKS)
        .find_map(|i| decks[i].light_input())
}

fn scene_text(ctl: &Control, inp: &Input, playing: bool) -> String {
    match (ctl.mode, inp.phrase) {
        (Mode::Auto, Some(p)) => format!(
            "{:?} · {:?} · {:?} · {:.1} BPM",
            p.kind,
            p.mood,
            scene::scene(p.kind, p.mood).pattern,
            inp.bpm
        )
        .to_uppercase(),
        (Mode::Auto, None) if playing => "AUTO · the master track has no phrases: ambient".into(),
        (Mode::Auto, None) => "AUTO · no deck is playing: ambient".into(),
        (Mode::Ambient, _) => format!("AMBIENT · {}", ctl.palette.label()),
        (Mode::Solid(_), _) => "MANUAL COLOUR".into(),
        (Mode::BlackOut, _) => "BLACK OUT".into(),
    }
}

impl Lighting {
    pub(super) fn tick(&self) {
        if !self.state(|s| s.get_enabled()).unwrap_or(false) {
            return;
        }
        let master = self
            .app
            .upgrade()
            .map_or(-1, |a| a.global::<DeckState>().get_master());
        let live = master_input(&self.decks, master);
        let inp = live.unwrap_or(Input {
            beat: self.started.elapsed().as_secs_f64() * IDLE_BPM / 60.0,
            bpm: IDLE_BPM,
            phrase: None,
        });
        let ctl = self.ctl.get();
        let cfg = self.cfg.borrow();
        let looks = cfg.rig.looks(&ctl, &inp);
        let frames = cfg.rig.frames(&looks);
        drop(cfg);
        let sent = self.out.borrow_mut().as_mut().map(|o| o.send(&frames));
        if let Some(Err(e)) = &sent {
            self.out_err.replace(format!("send failed: {e}"));
        }
        if !self.state(|s| s.get_visible()).unwrap_or(false) {
            return;
        }
        for (i, l) in looks.iter().enumerate() {
            let [r, g, b] = l.shown();
            let colour = slint::Color::from_rgb_u8(r, g, b);
            if let Some(mut row) = self.model.row_data(i).filter(|row| row.colour != colour) {
                row.colour = colour;
                self.model.set_row_data(i, row);
            }
        }
        self.state(|s| s.set_scene(SharedString::from(scene_text(&ctl, &inp, live.is_some()))));
        if sent.is_some() {
            self.show_status(frames.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::{Mood, PhraseKind as Kind};

    #[test]
    fn scene_text_names_phrase_mood_pattern_and_tempo() {
        let p = scene::PhraseCtx {
            kind: Kind::Chorus,
            mood: Mood::High,
            progress: 0.0,
            to_end: 8.0,
        };
        let inp = Input {
            beat: 0.0,
            bpm: 128.0,
            phrase: Some(p),
        };
        assert_eq!(
            scene_text(&Control::default(), &inp, true),
            "CHORUS · HIGH · CHASE · 128.0 BPM"
        );
        assert!(
            scene_text(
                &Control::default(),
                &Input {
                    phrase: None,
                    ..inp
                },
                false
            )
            .contains("no deck")
        );
        assert!(
            scene_text(
                &Control {
                    mode: Mode::BlackOut,
                    ..Control::default()
                },
                &inp,
                true
            )
            .contains("BLACK")
        );
    }
}
