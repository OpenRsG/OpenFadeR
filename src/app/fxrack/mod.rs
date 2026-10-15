//! SEND FX, LEVER FX and MIC FX ↔ ui/fxrack.slint (global `FxRackState`, FX panel tabs 3 to 5). Each rack is one row of
//! `FxRackState.racks` (0 SEND, 1 LEVER, 2 MIC) and is mirrored into its [`audio::fx::Controls`] on the bus (`Bus::send`,
//! `Bus::lever`, `Bus::mic_fx`); the effects come from the plugins of the slot (`plugin::registry().slot(..)`), so a rack with
//! no plugin shows a disabled "no plugin" entry. SEND also has one amount knob per channel (`Bus::set_send`). The LEVER fader
//! springs back to 0 when released; the lever is ON while it is above 0 and its position is the plugin's knob. Selections are
//! live state only (not saved). A 50 ms timer gives every rack the master tempo ([`super::fx::tempo`]; no deck gives one = the
//! 120 BPM fallback).

use super::{App, DeckState, FxRackState, RackView, deck::Decks, fx::tempo};
use openfader::{
    audio::{
        Bus, CHANNELS,
        fx::{BEAT_TEXT, BEATS, Controls, DEFAULT_BEAT, FALLBACK_BPM},
    },
    plugin::{effect::Slot, registry},
};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

const SLOTS: [Slot; 3] = [Slot::Send, Slot::Lever, Slot::Mic];

fn controls(bus: &Bus, rack: usize) -> &Controls {
    [&bus.send, &bus.lever, &bus.mic_fx][rack]
}

struct Rack {
    app: slint::Weak<App>,
    /// None when no deck has audio: the panel works but drives nothing.
    bus: Option<Arc<Bus>>,
    /// BEAT selector position per rack.
    beat: [Cell<usize>; 3],
    timer: Timer,
}

impl Rack {
    /// Mirrors rack `i` (and, for the lever, its position) into the bus.
    fn push(&self, i: usize, r: &RackView, lever: f32) {
        let Some(bus) = &self.bus else {
            return;
        };
        let c = controls(bus, i);
        c.set_index(r.effect.max(0) as usize);
        c.set_beat(self.beat[i].get());
        if i == 1 {
            c.set_level(1.0);
            c.set_knob(lever);
            c.set_on(r.has && lever > 0.0);
        } else {
            c.set_level(r.level);
            c.set_knob(r.param);
            c.set_on(r.has && r.on);
        }
    }

    /// Runs `f` on rack `i`'s row, then pushes it.
    fn edit(&self, i: i32, f: impl FnOnce(&mut RackView)) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let i = i.clamp(0, 2) as usize;
        let s = app.global::<FxRackState>();
        let rows = s.get_racks();
        if let Some(mut r) = rows.row_data(i) {
            f(&mut r);
            self.push(i, &r, s.get_lever());
            rows.set_row_data(i, r);
        }
    }

    fn lever(&self, v: f32) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let s = app.global::<FxRackState>();
        s.set_lever(v.clamp(0.0, 1.0));
        if let Some(r) = s.get_racks().row_data(1) {
            self.push(1, &r, s.get_lever());
        }
    }

    /// The master tempo into every rack.
    fn tick(&self) {
        let (Some(app), Some(bus)) = (self.app.upgrade(), &self.bus) else {
            return;
        };
        let decks = app.global::<DeckState>();
        let rows = decks.get_decks();
        let bpm = tempo(
            decks.get_master(),
            std::array::from_fn(|i| {
                rows.row_data(i)
                    .map_or((false, 0.0), |v| (v.playing, v.bpm))
            }),
        )
        .unwrap_or(FALLBACK_BPM);
        for i in 0..3 {
            controls(bus, i).set_bpm(bpm);
        }
    }
}

pub fn wire(app: &App, decks: &Decks) {
    let s = app.global::<FxRackState>();
    let view = |slot: Slot| {
        let names: Vec<slint::SharedString> = registry()
            .slot(slot)
            .iter()
            .map(|e| e.name.as_str().into())
            .collect();
        RackView {
            has: !names.is_empty(),
            names: ModelRc::new(VecModel::from(names)),
            level: 0.5,
            param: 0.5,
            beat_text: BEAT_TEXT[DEFAULT_BEAT].into(),
            ..RackView::default()
        }
    };
    s.set_racks(ModelRc::new(VecModel::from(SLOTS.map(view).to_vec())));
    s.set_sends(ModelRc::new(VecModel::from(vec![0.0; CHANNELS])));
    let rack = Rc::new(Rack {
        app: app.as_weak(),
        bus: decks.iter().find_map(|d| d.with_player(|p| p.bus())),
        beat: std::array::from_fn(|_| Cell::new(DEFAULT_BEAT)),
        timer: Timer::default(),
    });
    for (i, r) in s.get_racks().iter().enumerate() {
        rack.push(i, &r, 0.0);
    }

    let r = rack.clone();
    s.on_effect_selected(move |n, i| r.edit(n, |v| v.effect = i));
    let r = rack.clone();
    s.on_toggled(move |n| r.edit(n, |v| v.on = !v.on));
    let r = rack.clone();
    s.on_level_edited(move |n, x| r.edit(n, |v| v.level = x.clamp(0.0, 1.0)));
    let r = rack.clone();
    s.on_param_edited(move |n, x| r.edit(n, |v| v.param = x.clamp(0.0, 1.0)));
    let r = rack.clone();
    s.on_beat_step(move |n, d| {
        let b = &r.beat[n.clamp(0, 2) as usize];
        b.set((b.get() as i32 + d).clamp(0, BEATS.len() as i32 - 1) as usize);
        let text = BEAT_TEXT[b.get()].into();
        r.edit(n, |v| v.beat_text = text);
    });
    let r = rack.clone();
    s.on_send_edited(move |ch, x| {
        if let (Some(bus), Some(app)) = (&r.bus, r.app.upgrade()) {
            bus.set_send(ch.max(0) as usize, x);
            let sends = app.global::<FxRackState>().get_sends();
            sends.set_row_data(ch.max(0) as usize, x.clamp(0.0, 1.0));
        }
    });
    let r = rack.clone();
    s.on_lever_edited(move |v| r.lever(v));

    rack.tick();
    let weak = Rc::downgrade(&rack);
    rack.timer
        .start(TimerMode::Repeated, Duration::from_millis(50), move || {
            if let Some(r) = weak.upgrade() {
                r.tick();
            }
        });
}
