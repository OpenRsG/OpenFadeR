//! The actions only the controller mapping files reach: not learnable, each row brings its own constant.

use super::{Action, App, Kind, arg, on_deck};
use crate::app::{BrowserState, DeckState, FxState, GrooveState, SamplerState, groove};
use slint::{ComponentHandle, Model};

/// `arg` calling `State::cb(deck, constant)`, no release.
macro_rules! arg_cb {
    ($name:literal, $state:ident::$cb:ident) => {
        arg($name, |a, d, n| a.global::<$state>().$cb(d, n), None)
    };
}
/// `arg` calling `State::cb(deck)`, no release.
macro_rules! arg_deck {
    ($name:literal, $state:ident::$cb:ident) => {
        arg($name, |a, d, _| a.global::<$state>().$cb(d), None)
    };
}
/// A sampler pad callback on slot `constant` of the visible bank.
macro_rules! sampler_pad {
    ($cb:ident) => {
        |a: &App, _: i32, slot: i32| {
            let s = a.global::<SamplerState>();
            s.$cb(s.get_bank() * 8 + slot)
        }
    };
}

/// DRUM ROLL (`K` = 1) / DRUM TRANS (2) held at pad length `l` (0..3 = 1/8, 1/4, 1/2, 1 beat).
fn drum_hold<const K: i32>(a: &App, d: i32, l: i32) {
    drum_fx::<K>(a, d, l, true);
}
fn drum_release<const K: i32>(a: &App, d: i32, l: i32) {
    drum_fx::<K>(a, d, l, false);
}
fn drum_fx<const K: i32>(a: &App, d: i32, l: i32, held: bool) {
    let beats = groove::PAD_BEATS[l.clamp(0, 3) as usize] as i32;
    a.global::<GrooveState>().invoke_fx_held(d, K, beats, held);
}
/// Slot `i` of unit `u`.
fn fx_slot(a: &App, u: i32, i: i32) -> Option<crate::app::FxSlot> {
    let units = a.global::<FxState>().get_units();
    units
        .row_data(u.max(0) as usize)?
        .slots
        .row_data(i.max(0) as usize)
}
/// Steps slot `i` of unit `u` to the effect `d` places on in the selector (wraps).
fn fx_select_step(a: &App, u: i32, i: i32, d: i32) {
    let s = a.global::<FxState>();
    let n = s.get_effects().row_count().max(1) as i32;
    let cur = fx_slot(a, u, i).map_or(0, |x| x.effect);
    s.invoke_effect_selected(u, i, (cur + d).rem_euclid(n));
}

pub(super) const TABLE: &[Action] = &[
    Action::new("hw-deck-select", Kind::Layer),
    // arg = pad mode (0 hot cue, 1 beat jump, 2 keyboard, 3 beat loop, 4 pad FX, 5 key shift, 6 sample scratch, 7 stems) * 16 + slot
    // (PAD FX slots 8..16 are its SHIFT page, effects I to P)
    on_deck(
        "hw-pad",
        |d, a| d.pad_hw(a / 16, (a % 16) as usize),
        Some(|d, a| d.pad_released_at((a % 16) as usize)),
    ),
    on_deck("hw-pad-delete", |d, a| d.pad_delete(a as usize), None),
    on_deck("hw-pad-stop", |d, a| d.pad_stop(a as usize), None),
    // SHIFT + SAMPLE SCRATCH pad: unload the deck's track
    on_deck("hw-pad-unload", |d, _| d.unload(), None),
    // arg = side 0 left (decks A + C) / 1 right (B + D): DUAL DECK on / off, see `Midi::toggle_dual`
    Action::new("hw-dual", Kind::Dual),
    on_deck("hw-pad-mode", |d, a| d.set_pad_mode(a), None),
    on_deck("hw-cue-call", |d, a| d.cue_call(a), None),
    on_deck("hw-master", |d, _| d.master_toggle(), None),
    on_deck("hw-track-start", |d, _| d.jump_to_start(), None),
    // arg = slot in the visible bank
    arg(
        "hw-sampler",
        sampler_pad!(invoke_pad_pressed),
        Some(sampler_pad!(invoke_pad_released)),
    ),
    // The FX actions take the unit (0 FX1, 1 FX2) as their deck. arg = 0..3 CH1..CH4, 4 MASTER
    arg_cb!("hw-fx-assign", FxState::invoke_assign_selected),
    // arg = slot 0..2: the unit's BEAT target (the controller's FX SELECT)
    arg_cb!("hw-fx-target", FxState::invoke_target_selected),
    // arg = slot 0..2: that slot ON / OFF, or ON while held
    arg_cb!("hw-fx-on", FxState::invoke_toggled),
    arg(
        "hw-fx-hold",
        |a, u, i| a.global::<FxState>().invoke_held(u, i, true),
        Some(|a, u, i| a.global::<FxState>().invoke_held(u, i, false)),
    ),
    // arg = slot * 2 + 1 back: the slot's effect steps to the next (`FX1-1Select`) / previous (`FX1-1SelectBack`) one
    arg(
        "hw-fx-select",
        |a, u, n| fx_select_step(a, u, n / 2, if n % 2 == 1 { -1 } else { 1 }),
        None,
    ),
    // arg = slot * 64 + effect index: the slot gets that effect (`FX1-1Select.ECHO`)
    arg(
        "hw-fx-pick",
        |a, u, n| {
            a.global::<FxState>()
                .invoke_effect_selected(u, n / 64, n % 64)
        },
        None,
    ),
    // arg = slot * 64 + effect index: that effect ON on the slot, or OFF when the slot already plays it (`FX1-1On.ECHO`)
    arg(
        "hw-fx-pick-on",
        |a, u, n| {
            let (i, e) = (n / 64, n % 64);
            let s = a.global::<FxState>();
            if fx_slot(a, u, i).is_some_and(|x| x.on && x.effect == e) {
                s.invoke_held(u, i, false);
            } else {
                s.invoke_effect_selected(u, i, e);
                s.invoke_held(u, i, true);
            }
        },
        None,
    ),
    // arg = slot * 2 + 1 down: the slot's beat; the slot becomes the BEAT target
    arg(
        "hw-fx-slot-beat",
        |a, u, n| {
            let s = a.global::<FxState>();
            s.invoke_target_selected(u, n / 2);
            s.invoke_beat_step(u, if n % 2 == 1 { -1 } else { 1 });
        },
        None,
    ),
    // arg = channel * 3 + slot: the unit goes to that channel, the slot is ON while held
    arg(
        "hw-fx-ch-hold",
        |a, u, n| {
            let s = a.global::<FxState>();
            s.invoke_assign_selected(u, n / 3);
            s.invoke_held(u, n % 3, true);
        },
        Some(|a, u, n| a.global::<FxState>().invoke_held(u, n % 3, false)),
    ),
    arg_deck!("hw-fx-auto", FxState::invoke_auto_beat),
    arg_deck!("hw-fx-tap", FxState::invoke_tapped),
    // arg = 0 AUTO / 1 TAP on both units (`FxBpmAutoMode` / `FxBpmTapMode`)
    arg(
        "hw-fx-bpm",
        |a, _, tap| {
            let s = a.global::<FxState>();
            for u in 0..2 {
                if tap == 1 {
                    s.invoke_tapped(u);
                } else {
                    s.invoke_auto_beat(u);
                }
            }
        },
        None,
    ),
    // the FX panel shown / hidden (`FxPanel`)
    arg(
        "hw-fx-panel",
        |a, _, _| {
            let s = a.global::<FxState>();
            s.set_open(!s.get_open());
        },
        None,
    ),
    // Momentary: held = echo-out running.
    arg(
        "hw-fx-release",
        |a, u, _| a.global::<FxState>().invoke_release(u, true),
        Some(|a, u, _| a.global::<FxState>().invoke_release(u, false)),
    ),
    // arg = stem part 0 VOCAL, 1 INST, 2 BASS, 3 DRUMS: STEM INSTANT DOUBLES from the other deck (the deck column is the pressed deck).
    arg_cb!("hw-stem-double", DeckState::invoke_stem_double),
    // arg = FX PART SELECT 1 VOCAL, 2 INST, 3 BASS, 4 DRUMS on the unit the panel has selected (the rows name no unit): select, again = ALL.
    arg(
        "hw-fx-part",
        |a, _, part| {
            let s = a.global::<FxState>();
            s.invoke_part_selected(s.get_unit(), part);
        },
        None,
    ),
    arg("hw-sampler-stop", sampler_pad!(invoke_pad_stopped), None),
    arg(
        "hw-related",
        |a, _, _| {
            let b = a.global::<BrowserState>();
            b.invoke_related_toggled(!b.get_related());
        },
        None,
    ),
    on_deck("hw-memory-delete", |d, _| d.memory_delete(), None),
    // arg = ±1
    on_deck("hw-pad-page", |d, a| d.pad_page_step(a), None),
    // arg = browser column (0 title, 1 artist, 3 key, 4 BPM); pressing again flips the direction
    arg("hw-sort", |a, _, col| a.invoke_sort_by(col), None),
    on_deck(
        "hw-start-play",
        |d, _| d.fader_start(true, false),
        Some(|d, _| d.fader_start(false, false)),
    ),
    on_deck(
        "hw-start-cue",
        |d, _| d.fader_start(true, true),
        Some(|d, _| d.fader_start(false, true)),
    ),
    // MERGE FX: the deck column is the MFX side (A = MFX1, B = MFX2).
    arg(
        "merge-on",
        |a, s, _| a.global::<FxState>().invoke_merge_on(s, true),
        Some(|a, s, _| a.global::<FxState>().invoke_merge_on(s, false)),
    ),
    arg_cb!("merge-preset", FxState::invoke_merge_preset),
    // BEAT LAB: arg = DRUM SWAP pad 0..3 of the deck's bank in view; play / stop its slot.
    arg(
        "hw-drum-swap",
        |a, d, n| {
            let g = a.global::<GrooveState>();
            let bank = slint::Model::row_data(&g.get_decks(), d.max(0) as usize)
                .map_or(0, |v| v.bank.max(0) as usize);
            g.invoke_slot_pressed(d, groove::pad_slot(bank, n.max(0) as usize) as i32);
        },
        None,
    ),
    // arg = 0 BANK A, 1 BANK B; 0 SINGLE, 1 MULTI
    arg_cb!("hw-drum-bank", GrooveState::invoke_bank_selected),
    arg(
        "hw-drum-mode",
        |a, d, m| a.global::<GrooveState>().invoke_multi_selected(d, m == 1),
        None,
    ),
    arg_deck!("hw-drum-capture", GrooveState::invoke_capture_clicked),
    arg_deck!("hw-drum-range", GrooveState::invoke_range_clicked),
    // Held: DRUM RELEASE FX, and DRUM ROLL / DRUM TRANS at arg = 1/8, 1/4, 1/2, 1 beat.
    arg(
        "hw-drum-release",
        |a, d, _| a.global::<GrooveState>().invoke_fx_held(d, 0, 0, true),
        Some(|a, d, _| a.global::<GrooveState>().invoke_fx_held(d, 0, 0, false)),
    ),
    arg("hw-drum-roll", drum_hold::<1>, Some(drum_release::<1>)),
    arg("hw-drum-trans", drum_hold::<2>, Some(drum_release::<2>)),
    // arg = ±1: the DRUM RELEASE FX effect
    arg_cb!("hw-drum-fx", GrooveState::invoke_fx_step),
];
