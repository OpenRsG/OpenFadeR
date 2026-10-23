//! The pure decisions of the FX panel (no Slint, no audio): channel assignment, FX PART SELECT masks, RELEASE and MERGE FX rules, the pad colour.

use super::{FxSlot, FxUnit};
use crate::app::{deck::DECKS, prefs::view::PadFxColor};
use openfader::audio::{
    fx::{Effect, BEATS, BEAT_TEXT},
    merge::PRESETS,
    BASS, DRUMS, INST, VOCAL,
};
use slint::Model;
use std::cell::Cell;

/// A unit's assignment from a selector index (0..3 = CH1..CH4, 4 = MASTER, 5 = AUX): a channel the layout shows (the first
/// `decks`), MASTER, or AUX while the AUX input is enabled (`aux`), else `None`.
pub(super) fn assignment(i: i32, decks: usize, aux: bool) -> Option<i32> {
    (i == 4 || (i == 5 && aux) || (0..decks.min(DECKS) as i32).contains(&i)).then_some(i)
}

/// FX PART SELECT: the stem part a unit's effect is applied to. The selector value is 0 ALL (the whole channel), 1 VOCAL, 2 INST, 3 BASS,
/// 4 DRUMS; the audio side takes a mask of stems ([`Controls::set_part`](openfader::audio::fx::Controls::set_part)), 0 = whole channel.
/// With 3 STEMS bass is part of INST (INST covers both, BASS is not offered).
pub(super) fn part_mask(part: i32, four: bool) -> u8 {
    match part {
        1 => 1 << VOCAL,
        2 if four => 1 << INST,
        2 => 1 << INST | 1 << BASS,
        3 if four => 1 << BASS,
        4 => 1 << DRUMS,
        _ => 0,
    }
}

/// The part after `part` in ALL → VOCAL → INST → (BASS) → DRUMS → ALL.
pub(super) fn next_part(part: i32, four: bool) -> i32 {
    match (part + 1).rem_euclid(5) {
        3 if !four => 4,
        n => n,
    }
}

/// A controller's FX PART SELECT button for `part`: it selects the part, pressed again it goes back to ALL; BASS needs 4 STEMS.
pub(super) fn toggle_part(cur: i32, part: i32, four: bool) -> i32 {
    if !(1..=4).contains(&part) || (part == 3 && !four) || cur == part {
        0
    } else {
        part
    }
}

pub(super) fn bpm_text(bpm: f32) -> slint::SharedString {
    let digits = crate::app::prefs::with(|p| p.view.tempo_decimals.digits());
    format!("{bpm:.*} BPM", digits).into()
}

/// Runs `f` on slot `i` of `u` (out of range: nothing).
pub(super) fn edit_slot(u: &FxUnit, i: usize, f: impl FnOnce(&mut FxSlot)) {
    if let Some(mut s) = u.slots.row_data(i) {
        f(&mut s);
        u.slots.set_row_data(i, s);
    }
}

/// Slot `s` at beat position `b` (clamped to [`BEATS`]).
pub(super) fn set_beat(s: &mut FxSlot, b: i32) {
    s.beat = b.clamp(0, BEATS.len() as i32 - 1);
    s.beat_text = BEAT_TEXT[s.beat as usize].into();
}

/// RELEASE pressed / let go on `u`: every slot off and slot 1 ECHO at full level on, then slot 1 off too (the tail rings out).
pub(super) fn release(u: &mut FxUnit, held: bool) {
    u.releasing = held;
    for i in 0..u.slots.row_count() {
        edit_slot(u, i, |s| {
            s.on = held && i == 0;
            if s.on {
                (s.effect, s.level) = (Effect::Echo.index() as i32, 1.0);
            }
        });
    }
}

/// The assignment a unit's effect really runs on: 1 = RELEASE FX UNITS "master" sends a held RELEASE to MASTER (4), 2 keeps the unit's own.
pub(super) fn effective_assign(assign: i32, releasing: bool, release_units: u8) -> i32 {
    if releasing && release_units == 1 {
        4
    } else {
        assign
    }
}

/// "Apply RELEASE FX on CFX": the COLOR FX knobs act as centre while any unit's RELEASE is held.
pub(super) fn release_cfx_off(units: impl IntoIterator<Item = FxUnit>, on: bool) -> bool {
    on && units.into_iter().any(|u| u.releasing)
}

/// The BPM the effects follow. `decks`: per deck (playing, effective BPM, 0 = unknown); `master` the deck started
/// last: it is asked first, then A, B, C, D in order.
pub(in crate::app) fn tempo(master: i32, decks: [(bool, f32); DECKS]) -> Option<f32> {
    let first = usize::try_from(master).ok().filter(|&m| m < DECKS);
    first
        .into_iter()
        .chain((0..DECKS).filter(|&i| Some(i) != first))
        .map(|i| decks[i])
        .find(|&(playing, bpm)| playing && bpm > 0.0)
        .map(|(_, bpm)| bpm)
}

/// The tempo a unit's source gives: CH (`master_tempo` false) the deck on the unit's channel `assign` (0..3) while it plays at a
/// known tempo; MST, or CH on MASTER (4), the `master` tempo of [`tempo`]. `None` while there is nothing to follow.
pub(super) fn follow(
    assign: i32,
    master_tempo: bool,
    master: Option<f32>,
    decks: [(bool, f32); DECKS],
) -> Option<f32> {
    match usize::try_from(assign).ok().filter(|&d| d < DECKS) {
        Some(d) if !master_tempo => Some(decks[d])
            .filter(|&(p, b)| p && b > 0.0)
            .map(|(_, b)| b),
        _ => master,
    }
}

/// What a unit follows now: its source's tempo while that plays, else (the deck paused) the one the unit had, so a tail keeps
/// ringing at the tempo it started at. Only a unit that never had a tempo runs at the 120 BPM fallback.
pub(super) fn held_tempo(last: &Cell<Option<f32>>, playing: Option<f32>) -> Option<f32> {
    let t = playing.or(last.get());
    last.set(t);
    t
}

/// The PAD FX pads' colour (VIEW > Color): the RGB of the `Theme` token (accent, play, cue, danger, sampler) so the pads and the panels agree.
pub(in crate::app) fn pad_fx_rgb(c: PadFxColor) -> [u8; 3] {
    match c {
        PadFxColor::Blue => [20, 115, 235],
        PadFxColor::Green => [45, 215, 40],
        PadFxColor::Orange => [255, 185, 55],
        PadFxColor::Red => [230, 40, 40],
        PadFxColor::Purple => [180, 76, 255],
    }
}

/// The tempo a unit runs at and whether it is only the fallback: `over` (TAP), else the `followed` tempo of its CH / MST source.
pub(super) fn unit_bpm(over: Option<f32>, followed: Option<f32>) -> (f32, bool) {
    (
        over.or(followed).unwrap_or(super::FALLBACK_BPM),
        over.is_none() && followed.is_none(),
    )
}

/// Steps `u`'s MERGE FX preset by `d` (wraps both ways) and shows its name.
pub(super) fn merge_step(u: &mut FxUnit, d: i32) {
    let i = (u.merge_preset + d).rem_euclid(PRESETS.len() as i32);
    (u.merge_preset, u.merge_name) = (i, PRESETS[i as usize].name.into());
}
