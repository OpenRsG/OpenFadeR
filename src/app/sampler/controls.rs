//! Builds the [`Sampler`] and connects the `SamplerState` callbacks and the 50 ms timer to it.

use super::{
    App, BEATS, Cfg, DEFAULT_BEATS, Decks, ME, SLOTS, Sampler, SamplerState, SlotView, Track,
    VecModel, gain, prefs, seq,
};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

pub fn wire(app: &App, decks: &Decks, track: Rc<RefCell<Option<Track>>>) {
    let model = Rc::new(VecModel::from(vec![SlotView::default(); SLOTS]));
    let st = app.global::<SamplerState>();
    st.set_slots(ModelRc::from(model.clone()));
    st.set_beats_text(BEATS[DEFAULT_BEATS].to_string().into());
    let me = Rc::new(Sampler {
        app: app.as_weak(),
        bus: decks.iter().find_map(|d| d.with_player(|p| p.bus())),
        decks: decks.clone(),
        track,
        model,
        cfg: RefCell::default(),
        used: Cell::new(SLOTS),
        beats: Cell::new(DEFAULT_BEATS),
        pending: RefCell::default(),
        bank: RefCell::default(),
        timer: Timer::default(),
        seq: seq::Ui::default(),
    });
    if let Some(bus) = &me.bus {
        bus.sampler.set_master(gain(st.get_master()));
    }
    me.select(0);
    ME.with(|m| *m.borrow_mut() = Rc::downgrade(&me));
    prefs::with(|p| me.show_prefs(p));
    let weak = Rc::downgrade(&me);
    prefs::on_change(move |p| {
        if let Some(s) = weak.upgrade() {
            s.show_prefs(p);
        }
    });

    on!(st, me, on_pad_pressed, |s, i| {
        if let (Some(bus), true) = (&s.bus, (0..s.used.get() as i32).contains(&i)) {
            if s.seq_pad(i as usize) {
                return;
            }
            let i = i as usize;
            s.select(i);
            if !s.capture_loop(i) {
                bus.sampler.press(i);
            }
        }
    });
    on!(st, me, on_pad_released, |s, i| {
        if let Some(bus) = &s.bus {
            bus.sampler.release(i.max(0) as usize);
        }
    });
    on!(st, me, on_pad_stopped, |s, i| {
        if let Some(bus) = &s.bus {
            bus.sampler.stop(i.max(0) as usize);
        }
    });
    on!(st, me, on_bank_toggled, |s| {
        if let Some((app, sel)) = s.state() {
            let st = app.global::<SamplerState>();
            let bank = if s.used.get() > 8 {
                1 - st.get_bank()
            } else {
                0
            };
            st.set_bank(bank);
            s.select(bank as usize * 8 + sel % 8);
            s.decks.iter().for_each(|d| d.refresh_pads()); // the deck pads show the visible bank
        }
    });
    on!(st, me, on_loop_toggled, |s| s
        .edit(|c| c.looped = !c.looped));
    on!(st, me, on_gate_toggled, |s| s.edit(|c| c.gate = !c.gate));
    on!(st, me, on_sync_toggled, |s| {
        s.edit(|c| c.sync = !c.sync);
        if let Some((_, i)) = s.state() {
            let c = &s.cfg.borrow()[i];
            if c.sync && c.loaded && c.bpm <= 0.0 {
                s.say(tr!("this slice has no tempo: SYNC does nothing"));
            }
        }
    });
    on!(st, me, on_volume_edited, |s, v| s
        .edit(|c| c.volume = v.clamp(0.0, 1.0)));
    on!(st, me, on_master_edited, |s, v| {
        let v = v.clamp(0.0, 1.0);
        if let Some(app) = s.app.upgrade() {
            app.global::<SamplerState>().set_master(v);
        }
        if let Some(bus) = &s.bus {
            bus.sampler.set_master(gain(v));
        }
    });
    on!(st, me, on_cue_toggled, |s| {
        if let Some(app) = s.app.upgrade() {
            let st = app.global::<SamplerState>();
            st.set_cue(!st.get_cue());
            if let Some(bus) = &s.bus {
                bus.sampler.set_cue(st.get_cue());
            }
        }
    });
    on!(st, me, on_source_selected, |s, i| {
        if let Some(app) = s.app.upgrade() {
            app.global::<SamplerState>().set_source(i);
        }
    });
    on!(st, me, on_beats_step, |s, d| {
        let i = (s.beats.get() as i32 + d).clamp(0, BEATS.len() as i32 - 1) as usize;
        s.beats.set(i);
        if let Some(app) = s.app.upgrade() {
            app.global::<SamplerState>()
                .set_beats_text(BEATS[i].to_string().into());
        }
    });
    on!(st, me, on_load_clicked, |s| s.load());
    on!(st, me, on_clear_clicked, |s| {
        if let (Some((_, i)), Some(bus)) = (s.state(), &s.bus) {
            bus.sampler.clear(i);
            s.cfg.borrow_mut()[i] = Cfg::default();
            s.apply(i);
            s.select(i);
        }
    });
    on!(st, me, on_export_loop, |s, d| s.export_loop(d));
    seq::wire(&me, &st);
    let weak = Rc::downgrade(&me);
    me.timer
        .start(TimerMode::Repeated, Duration::from_millis(50), move || {
            if let Some(s) = weak.upgrade() {
                s.tick();
            }
        });
}
