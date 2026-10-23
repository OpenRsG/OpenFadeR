//! Recipe controls applied to the live mixer. Save values, not model rows: Slint's inner models are shared.
use super::fade::Rig;
use crate::app::{deck::Decks, mixer, App, FxSlot, FxState, MixerState};
use openfader::{
    audio::fx::BEATS,
    mixstyle::{Ctl, Event, FxStep, Side, State},
};
use slint::{ComponentHandle, Model};

pub(super) struct Live {
    app: slint::Weak<App>,
    decks: Decks,
    channels: [usize; 2],
    saved: [[Option<f32>; 5]; 2],
    cfx: Option<i32>,
    fx: Option<(i32, i32, FxSlot)>,
    loops: [bool; 2],
}

impl Live {
    pub fn new(app: &App, decks: Decks, out: usize, inc: usize) -> Self {
        Self {
            app: app.as_weak(),
            decks,
            channels: [out, inc],
            saved: [[None; 5]; 2],
            cfx: None,
            fx: None,
            loops: [false; 2],
        }
    }

    fn effect(&mut self, app: &App, side: Side, f: &FxStep) {
        let s = app.global::<FxState>();
        let Some(u) = s.get_units().row_data(1) else {
            return;
        };
        let Some(slot) = u.slots.row_data(0) else {
            return;
        };
        self.fx.get_or_insert((u.assign, u.target, slot.clone()));
        s.invoke_assign_selected(1, self.channels[side as usize] as i32);
        s.invoke_target_selected(1, 0);
        if let Some(name) = &f.effect {
            if let Some(i) = s
                .get_effects()
                .iter()
                .position(|e| e.eq_ignore_ascii_case(name))
            {
                s.invoke_effect_selected(1, 0, i as i32);
            }
        }
        if let Some(b) = f.beat {
            let i = BEATS
                .iter()
                .enumerate()
                .min_by(|a, b_| (a.1 - b).abs().total_cmp(&(b_.1 - b).abs()))
                .map_or(5, |(i, _)| i);
            s.invoke_beat_step(1, i as i32 - slot.beat);
        }
        if let Some(level) = f.level {
            s.invoke_level_edited(1, 0, level);
        }
        s.invoke_held(1, 0, f.on);
    }
}

fn knob(c: Ctl) -> Option<usize> {
    match c {
        Ctl::Fader => None,
        Ctl::Low => Some(mixer::LOW),
        Ctl::Mid => Some(mixer::MID),
        Ctl::High => Some(mixer::HI),
        Ctl::Filter => Some(mixer::CFX),
    }
}

impl Rig for Live {
    fn show(&mut self, state: &State) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let s = app.global::<MixerState>();
        let rows = s.get_channels();
        for side in 0..2 {
            let Some(mut row) = rows.row_data(self.channels[side]) else {
                continue;
            };
            for c in Ctl::ALL {
                let Some(mut v) = state.deck[side][c as usize] else {
                    continue;
                };
                let old = if let Some(k) = knob(c) {
                    row.knobs.row_data(k).unwrap_or(0.5)
                } else {
                    row.fader
                };
                let base = *self.saved[side][c as usize].get_or_insert(old);
                if c == Ctl::Fader {
                    row.fader = base * v;
                } else {
                    if c == Ctl::Filter {
                        self.cfx.get_or_insert(s.get_cfx_type());
                        s.set_cfx_type(0);
                        v = (v + 1.0) * 0.5;
                    }
                    row.knobs.set_row_data(knob(c).unwrap(), v);
                }
            }
            rows.set_row_data(self.channels[side], row);
        }
        s.set_auto_fade(true);
        let pos = if self.channels[1] % 2 == 1 {
            state.crossfader
        } else {
            1.0 - state.crossfader
        };
        s.set_crossfader(pos);
        s.invoke_crossfade(pos);
    }

    fn fire(&mut self, side: Side, event: &Event, beat_secs: f64) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let d = self.channels[side as usize];
        match event {
            Event::Fx(f) => self.effect(&app, side, f),
            Event::Loop(n) => {
                self.decks[d].loop_off();
                if *n > 0.0 {
                    self.decks[d].beat_loop(*n);
                }
                self.loops[side as usize] = true;
            }
            Event::Brake(n) => {
                self.decks[d].with_player(|p| p.brake((n * beat_secs) as f32));
            }
        }
    }

    fn restore(&mut self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let s = app.global::<MixerState>();
        let rows = s.get_channels();
        for side in 0..2 {
            if let Some(mut row) = rows.row_data(self.channels[side]) {
                for c in Ctl::ALL {
                    if let Some(v) = self.saved[side][c as usize] {
                        if let Some(k) = knob(c) {
                            row.knobs.set_row_data(k, v);
                        } else {
                            row.fader = v;
                        }
                    }
                }
                rows.set_row_data(self.channels[side], row);
            }
            if self.loops[side] {
                self.decks[self.channels[side]].loop_off();
            }
        }
        if let Some(c) = self.cfx {
            s.invoke_cfx_selected(c);
        }
        if let Some((assign, target, slot)) = &self.fx {
            let f = app.global::<FxState>();
            let current = f
                .get_units()
                .row_data(1)
                .and_then(|u| u.slots.row_data(0))
                .map_or(5, |s| s.beat);
            f.invoke_assign_selected(1, *assign);
            f.invoke_target_selected(1, 0);
            f.invoke_effect_selected(1, 0, slot.effect);
            f.invoke_beat_step(1, slot.beat - current);
            f.invoke_level_edited(1, 0, slot.level);
            f.invoke_held(1, 0, slot.on);
            f.invoke_target_selected(1, *target);
        }
        s.invoke_crossfade(s.get_crossfader());
    }
}
