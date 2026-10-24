//! Beat FX panel ↔ ui/fx.slint (global `FxState`): two units, FX1 and FX2, each assignable to CH A to D (C and D in the 4-deck layout) or MASTER,
//! each with three effect slots (FX1-1 … FX1-3, every slot its own effect, beat, LEVEL / DEPTH and ON, all on the
//! unit's assignment and tempo; one of them is the BEAT target, the slot the BEAT ◁ / ▷ buttons and the controller's beat display follow, which
//! a controller's FX SELECT picks). Each unit's values live in its
//! `FxState.units` row; every user action edits one row and [`Fx::push`] mirrors it into that unit's
//! [`audio::fx::Controls`] on the audio bus (`Bus::fx[unit]`; the effects themselves are in `src/audio/fx/`).
//! A 50 ms timer picks each unit's tempo: its TAP override if there is one, else its source (the CH / MST chip, saved in the settings
//! like FX1-1's beat): CH (the default) follows the effective BPM of the deck on the unit's channel, MST the master deck's
//! (the deck started last, `DeckState.master`, else the other decks in order); a unit on MASTER follows the master deck whatever
//! the chip says. A deck that is not playing at a known tempo gives none: the unit keeps the tempo it had (a ringing tail does not
//! re-time), and a unit that never had one shows "no BPM" and runs at the 120 BPM fallback. The bpm text names the source in use
//! (CH2, MST, TAP). TAP (average of the last taps, see
//! [`grid::tap`]) sets the override; the chip clears it (a controller's AUTO does too), else it switches CH / MST. RELEASE (momentary): while held the unit plays ECHO at full level, on release its feed stops and the echo
//! tail rings out (the unit's trails); the unit then stays on ECHO / off. FX1-1's last effect and beat, and FX1's CH / MST source,
//! are saved in the settings ([`prefs`]; FX2 starts with the same source). Slots 2 and 3 start on REVERB and FLANGER (our choice).
//!
//! MERGE FX (MFX1 = deck A, MFX2 = deck B; `src/audio/merge/`): side `i` lives in `FxState.units[i]`'s `merge-*` fields and
//! goes to `Bus::merge[i]`. GO / the controller's ON is held (release = the drop), ABORT is instant, the preset steps
//! through [`PRESETS`], and each side follows its own deck's tempo. CONTROLLER > Effect: MERGE FX can be switched off (the panel greys and the
//! controller's MFX buttons do nothing), and "stop the operating deck" pauses the side's deck when its run ends (GO let go or ABORT).
//!
//! The panel and the keyboard edit `FxState.unit`; a controller names the unit in every callback
//! (`FxState::invoke_toggled(unit)` …).
//!
//! FX PART SELECT (STEMS on, unit on CH A–D): the unit's PART chip (or a controller's part button) makes the effect run on one stem part of the
//! assigned deck only, the rest stays dry (`Controls::set_part`, mixed in the deck's voice, see `audio::fx`); MASTER has no stems, so the part
//! is ignored there. With 3 STEMS, INST covers bass. The PART chip is hidden (and no part is applied) when EXTENSIONS > STEMS hides STEM FX.
//!
//! RELEASE FX UNITS: 1 (the default) plays a held RELEASE on the master output whatever the unit's assignment; 2 on the unit's own assignment.
//! "Apply RELEASE FX on CFX" sets `MixerState.release-held` while any unit's RELEASE is held (the mixer then centres the COLOR FX knobs).
//! VIEW > Effect Panel decides which tab (`FxState.view`: BEAT FX, RELEASE FX, MERGE FX) the panel shows; it is applied when the setting changes.

mod logic;

use super::{
    App, DeckState, FxSlot, FxState, FxUnit, MixerState,
    deck::{self, Decks, grid},
    prefs,
};
use logic::{
    assignment, bpm_text, edit_slot, effective_assign, follow, held_tempo, merge_step, next_part,
    part_mask, release, release_cfx_off, set_beat, toggle_part, unit_bpm,
};
pub(super) use logic::{pad_fx_rgb, tempo};
use openfader::audio::{
    fx::{Assign, Effect, BEAT_TEXT, FALLBACK_BPM},
    merge::{DEFAULT_VOLUME, PRESETS},
    Bus, FX_SLOTS, FX_UNITS,
};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

fn unit_index(n: i32) -> usize {
    n.clamp(0, FX_UNITS as i32 - 1) as usize
}

fn slot_index(n: i32) -> usize {
    n.clamp(0, FX_SLOTS as i32 - 1) as usize
}

struct Fx {
    app: slint::Weak<App>,
    /// None when no deck has audio: the panel works but drives nothing.
    bus: Option<Arc<Bus>>,
    decks: Decks,
    timer: Timer,
    /// Per unit: tap times (ms) and the tempo they set.
    taps: RefCell<[Vec<f64>; FX_UNITS]>,
    over: [Cell<Option<f32>>; FX_UNITS],
    start: Instant,
    /// The `FxState.view` last taken from VIEW > Effect Panel (-1 before the first).
    view: Cell<i32>,
    /// Per unit: the tempo it followed while its source last played.
    last_tempo: [Cell<Option<f32>>; FX_UNITS],
}

impl Fx {
    /// Runs `f` on unit `unit`'s row, then sends the row to the audio thread.
    fn edit(&self, unit: i32, f: impl FnOnce(&mut FxUnit)) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let unit = unit_index(unit);
        let units = app.global::<FxState>().get_units();
        let Some(mut u) = units.row_data(unit) else {
            return;
        };
        f(&mut u);
        self.push(unit, &u);
        units.set_row_data(unit, u);
        app.global::<MixerState>().set_release_held(release_cfx_off(
            units.iter(),
            prefs::with(|p| p.controller.release_cfx_off),
        ));
    }

    /// The settings the panel shows: MERGE FX enabled, STEM FX shown, and, when VIEW > Effect Panel changed, the tab.
    fn show_prefs(&self, p: &prefs::Prefs) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let s = app.global::<FxState>();
        s.set_merge_enabled(p.controller.merge_enabled);
        s.set_stem_fx(p.extensions.stem_displays()[2]);
        let v = p.view.effect_panel as i32;
        if self.view.replace(v) != v {
            s.set_view(v);
        }
    }

    /// A MERGE FX run on side `n` ends (GO let go, ABORT, or MERGE FX switched off): it stops; `abort` also drops the tail. With
    /// "stop the operating deck" the side's deck (MFX1 = A, MFX2 = B) pauses when it was running.
    fn merge_end(&self, n: i32, abort: bool) {
        let side = unit_index(n);
        if let (true, Some(bus)) = (abort, &self.bus) {
            bus.merge[side].abort();
        }
        let mut was = false;
        self.edit(n, |u| (was, u.merge_on) = (u.merge_on, false));
        let playing = self
            .app
            .upgrade()
            .and_then(|a| a.global::<DeckState>().get_decks().row_data(side))
            .is_some_and(|v| v.playing);
        if was && playing && prefs::with(|p| p.controller.merge_stop_deck) {
            self.decks[side].play_pause();
        }
    }

    /// `edit` while MERGE FX is enabled in the settings.
    fn merge_edit(&self, unit: i32, f: impl FnOnce(&mut FxUnit)) {
        if prefs::with(|p| p.controller.merge_enabled) {
            self.edit(unit, f);
        }
    }

    fn push(&self, unit: usize, u: &FxUnit) {
        let Some(bus) = &self.bus else {
            return;
        };
        let (release, stems_on, four, merge_on) = prefs::with(|p| {
            (
                p.controller.release_units(),
                p.stems && p.extensions.stem_displays()[2],
                p.stems_four,
                p.controller.merge_enabled,
            )
        });
        let assign = effective_assign(u.assign, u.releasing, release);
        // MASTER has no stems.
        let part = if assign < 4 && stems_on {
            part_mask(u.part, four)
        } else {
            0
        };
        for (c, s) in bus.fx[unit].iter().zip(u.slots.iter()) {
            c.set_effect(Effect::from_index(s.effect.max(0) as usize));
            c.set_beat(s.beat.max(0) as usize);
            c.set_level(s.level);
            c.set_on(s.on);
            c.set_assign(Assign::from_index(assign.max(0) as usize));
            c.set_part(part);
        }
        let m = &bus.merge[unit];
        m.set_preset(u.merge_preset.max(0) as usize);
        m.set_knob(u.merge_knob);
        m.set_volume(u.merge_volume);
        m.set_on(u.merge_on && merge_on);
    }

    fn tap(&self, unit: i32) {
        let unit = unit_index(unit);
        let now = self.start.elapsed().as_secs_f64() * 1000.0;
        if let Some(bpm) = grid::tap(&mut self.taps.borrow_mut()[unit], now) {
            self.set_over(unit, Some(bpm as f32));
        }
    }

    fn set_over(&self, unit: usize, v: Option<f32>) {
        self.over[unit].set(v);
        self.tick();
    }

    /// Master tempo into each unit's row and Controls.
    fn tick(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let decks = app.global::<DeckState>();
        let deck_rows = decks.get_decks();
        let row = |i| {
            deck_rows
                .row_data(i)
                .map_or((false, 0.0), |v| (v.playing, v.bpm))
        };
        let decks_now: [(bool, f32); deck::DECKS] = std::array::from_fn(row);
        let master = tempo(decks.get_master(), decks_now);
        let units = app.global::<FxState>().get_units();
        for (i, over) in self.over.iter().map(Cell::get).enumerate() {
            let followed = units
                .row_data(i)
                .and_then(|u| follow(u.assign, u.master_tempo, master, decks_now));
            let (bpm, no_bpm) = unit_bpm(over, held_tempo(&self.last_tempo[i], followed));
            if let Some(mut u) = units.row_data(i) {
                let bpm_text = bpm_text(bpm);
                let tapped = over.is_some();
                if (u.no_bpm, u.tapped, &u.bpm_text) != (no_bpm, tapped, &bpm_text) {
                    (u.no_bpm, u.tapped, u.bpm_text) = (no_bpm, tapped, bpm_text);
                    units.set_row_data(i, u);
                }
            }
            if let Some(bus) = &self.bus {
                bus.fx[i].iter().for_each(|c| c.set_bpm(bpm));
                bus.merge[i].set_bpm(row(i).1);
            }
        }
    }
}

pub fn wire(app: &App, decks: &Decks) {
    let s = app.global::<FxState>();
    let saved = prefs::get();
    let slot = |effect: usize, beat: usize| FxSlot {
        effect: effect as i32,
        beat: beat as i32,
        beat_text: BEAT_TEXT[beat].into(),
        level: 0.5,
        on: false,
    };
    let unit = |first: usize, beat: usize| FxUnit {
        slots: ModelRc::new(VecModel::from(vec![
            slot(first, beat),
            slot(Effect::Reverb.index(), beat),
            slot(Effect::Flanger.index(), beat),
        ])),
        assign: 4, // MASTER
        master_tempo: saved.fx_tempo_master,
        no_bpm: true,
        bpm_text: bpm_text(FALLBACK_BPM),
        merge_name: PRESETS[0].name.into(),
        merge_volume: DEFAULT_VOLUME,
        ..FxUnit::default()
    };
    let rows = [unit(saved.fx_effect, saved.fx_beat), unit(0, saved.fx_beat)];
    s.set_effects(ModelRc::new(VecModel::from(
        Effect::names()
            .into_iter()
            .map(Into::into)
            .collect::<Vec<slint::SharedString>>(),
    )));
    s.set_units(ModelRc::new(VecModel::from(rows.to_vec())));
    let fx = Rc::new(Fx {
        app: app.as_weak(),
        bus: decks.iter().find_map(|d| d.with_player(|p| p.bus())),
        decks: decks.clone(),
        timer: Timer::default(),
        taps: RefCell::default(),
        over: Default::default(),
        start: Instant::now(),
        view: Cell::new(-1),
        last_tempo: Default::default(),
    });
    fx.show_prefs(&saved);
    for (i, u) in rows.iter().enumerate() {
        fx.push(i, u);
    }

    let f = fx.clone();
    s.on_effect_selected(move |n, i, e| {
        f.edit(n, |u| edit_slot(u, slot_index(i), |s| s.effect = e));
        if n == 0 && i == 0 {
            prefs::update(|p| p.fx_effect = e.max(0) as usize);
        }
    });
    let f = fx.clone();
    s.on_beat_step(move |n, d| {
        f.edit(n, |u| {
            let t = slot_index(u.target);
            edit_slot(u, t, |s| {
                set_beat(s, s.beat + d);
                if n == 0 && t == 0 {
                    prefs::update(|p| p.fx_beat = s.beat as usize);
                }
            });
        })
    });
    let f = fx.clone();
    s.on_target_selected(move |n, i| f.edit(n, |u| u.target = slot_index(i) as i32));
    let f = fx.clone();
    s.on_level_edited(move |n, i, v| {
        f.edit(n, |u| {
            edit_slot(u, slot_index(i), |s| s.level = v.clamp(0.0, 1.0))
        })
    });
    let f = fx.clone();
    s.on_toggled(move |n, i| f.edit(n, |u| edit_slot(u, slot_index(i), |s| s.on = !s.on)));
    let f = fx.clone();
    s.on_held(move |n, i, on| f.edit(n, |u| edit_slot(u, slot_index(i), |s| s.on = on)));
    let f = fx.clone();
    s.on_assign_selected(move |n, i| {
        if let Some(a) = assignment(i, deck::active(), prefs::with(|p| p.audio.aux_on)) {
            f.edit(n, |u| u.assign = a);
        }
    });
    let f = fx.clone();
    s.on_part_step(move |n| {
        f.edit(n, |u| {
            u.part = if u.assign < 4 {
                next_part(u.part, prefs::with(|p| p.stems_four))
            } else {
                0
            }
        })
    });
    let f = fx.clone();
    s.on_part_selected(move |n, part| {
        f.edit(n, |u| {
            if u.assign < 4 {
                u.part = toggle_part(u.part, part, prefs::with(|p| p.stems_four))
            }
        })
    });
    // Back to the 2-deck layout: a unit on CH3 / CH4 goes to MASTER (those channels are silent) unless the controller reached them.
    let f = fx.clone();
    prefs::on_change(move |p| {
        f.show_prefs(p);
        for n in 0..FX_UNITS as i32 {
            if !p.controller.merge_enabled {
                f.merge_end(n, true);
            }
            f.edit(n, |u| {
                if (u.assign >= deck::active() as i32 && u.assign < 4)
                    || (u.assign == 5 && !p.audio.aux_on)
                {
                    u.assign = 4;
                }
                if u.part == 3 && !p.stems_four {
                    u.part = 0; // no BASS part with 3 STEMS
                }
            });
        }
    });
    let f = fx.clone();
    s.on_tapped(move |n| f.tap(n));
    let f = fx.clone();
    s.on_auto_beat(move |n| {
        let n = unit_index(n);
        f.taps.borrow_mut()[n].clear();
        f.set_over(n, None);
    });
    let f = fx.clone();
    s.on_tempo_source(move |n| {
        let mut master = false;
        f.edit(n, |u| {
            (u.master_tempo, master) = (!u.master_tempo, !u.master_tempo)
        });
        if n == 0 {
            prefs::update(|p| p.fx_tempo_master = master);
        }
        f.tick();
    });
    let f = fx.clone();
    s.on_release(move |n, held| f.edit(n, |u| release(u, held)));
    let f = fx.clone();
    s.on_merge_on(move |n, held| {
        if !held {
            f.merge_end(n, false);
        } else {
            f.merge_edit(n, |u| u.merge_on = true);
        }
    });
    let f = fx.clone();
    s.on_merge_abort(move |n| f.merge_end(n, true));
    let f = fx.clone();
    s.on_merge_preset(move |n, d| f.merge_edit(n, |u| merge_step(u, d)));
    let f = fx.clone();
    s.on_merge_knob(move |n, v| f.merge_edit(n, |u| u.merge_knob = v.clamp(0.0, 1.0)));
    let f = fx.clone();
    s.on_merge_volume(move |n, v| f.merge_edit(n, |u| u.merge_volume = v.clamp(0.0, 1.0)));

    fx.tick();
    let weak = Rc::downgrade(&fx);
    fx.timer
        .start(TimerMode::Repeated, Duration::from_millis(50), move || {
            if let Some(f) = weak.upgrade() {
                f.tick();
            }
        });
}

#[cfg(test)]
mod tests;
