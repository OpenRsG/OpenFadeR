//! The actions MIDI LEARN offers and midi.map accepts.

use super::{Action, App, JogMode, Kind, jog, knob, press, rel};
use crate::app::prefs;
use crate::app::{
    mixer::{iso_at, knob_at, AUX_CH},
    BrowserState, DeckState, FxSlot, FxState, GrooveState, KeysState, MixerState,
};
use openfader::audio::fx::BEATS;
use slint::{ComponentHandle, Model};

/// `press` calling `State::cb(deck, extra...)`; the deck column is the deck, FX unit or MERGE side.
macro_rules! press_cb {
    ($name:literal, $state:ident::$cb:ident $(, $extra:expr)*) => {
        press($name, |a, d| a.global::<$state>().$cb(d $(, $extra)*))
    };
}
/// `press` calling `State::cb()`, whatever the deck column.
macro_rules! press_any {
    ($name:literal, $state:ident::$cb:ident) => {
        press($name, |a, _| a.global::<$state>().$cb())
    };
}
/// `knob` calling `State::cb(deck, value)`.
macro_rules! knob_cb {
    ($name:literal, $state:ident::$cb:ident) => {
        knob($name, |a, d, v| a.global::<$state>().$cb(d, v))
    };
}

fn pad<const N: i32>(a: &App, d: i32) {
    a.global::<DeckState>().invoke_pad(d, N);
}
fn pad_up(a: &App, d: i32, _: i32) {
    a.global::<DeckState>().invoke_pad_released(d);
}
/// A hot-cue pad: the release hands back to the slip playhead.
const fn hot(name: &'static str, f: fn(&App, i32)) -> Action {
    press(name, f).released_by(pad_up)
}

fn mix_knob<const K: i32>(a: &App, ch: i32, v: f32) {
    a.global::<MixerState>().invoke_knob(ch, K, v);
}
fn bus_knob<const K: i32>(a: &App, _: i32, v: f32) {
    a.global::<MixerState>().invoke_bus_knob(K, v);
}
/// A STEM ISOLATOR knob (0 VOCAL, 1 INST, 2 DRUMS) of channel `ch`.
fn iso_knob<const P: i32>(a: &App, ch: i32, v: f32) {
    a.global::<MixerState>().invoke_iso_knob(ch, P, v);
}

fn mix_knob_at<const K: i32>(a: &App, ch: i32) -> Option<f32> {
    knob_at(a, ch, K as usize)
}
fn bus_knob_at<const K: i32>(a: &App, _: i32) -> Option<f32> {
    a.global::<MixerState>()
        .get_bus()
        .knobs
        .row_data(K as usize)
}
fn iso_knob_at<const P: usize>(a: &App, ch: i32) -> Option<f32> {
    iso_at(a, ch, P)
}
fn fader_at(a: &App, ch: i32) -> Option<f32> {
    a.global::<MixerState>()
        .get_channels()
        .row_data(ch as usize)
        .map(|c| c.fader)
}

/// Unit `u`'s BEAT target slot.
fn fx_target(a: &App, u: i32) -> i32 {
    let units = a.global::<FxState>().get_units();
    units.row_data(u.max(0) as usize).map_or(0, |r| r.target)
}
/// Slot `i` of unit `u`.
fn fx_slot(a: &App, u: i32, i: i32) -> Option<FxSlot> {
    let units = a.global::<FxState>().get_units();
    units
        .row_data(u.max(0) as usize)?
        .slots
        .row_data(i.max(0) as usize)
}
/// LEVEL / DEPTH of slot `S` of the unit in the deck column.
fn fx_level<const S: i32>(a: &App, u: i32, v: f32) {
    a.global::<FxState>().invoke_level_edited(u, S, v);
}
fn fx_level_at<const S: i32>(a: &App, u: i32) -> Option<f32> {
    fx_slot(a, u, S).map(|s| s.level)
}
fn fx_target_level_at(a: &App, u: i32) -> Option<f32> {
    fx_slot(a, u, fx_target(a, u)).map(|s| s.level)
}
/// MERGE FX build-up, or with `VOLUME` the sample volume, of the unit in the deck column.
fn merge_at<const VOLUME: bool>(a: &App, u: i32) -> Option<f32> {
    let unit = a
        .global::<FxState>()
        .get_units()
        .row_data(u.max(0) as usize)?;
    Some(if VOLUME { unit.merge_volume } else { unit.merge_knob })
}

/// Fader position for a hardware tempo value `v`: such faders report 0 at the top (the + end:
/// rate = 1 − v/8192; not checked on this hardware), so `invert` (CONTROLLER > Invert hardware
/// tempo fader) passes `v` through for the ones that report 1 at the top.
fn hw_tempo_pos(v: f32, invert: bool) -> f32 {
    if invert { v } else { 1.0 - v }
}

fn tempo_hw(a: &App, d: i32, v: f32) {
    let invert = prefs::with(|p| p.controller.tempo_invert);
    a.global::<DeckState>()
        .invoke_tempo(d, hw_tempo_pos(v, invert));
}
/// The tempo fader's position on screen; `None` while SYNC follows the master (the fader is ignored then).
fn tempo_at(a: &App, d: i32) -> Option<f32> {
    let decks = a.global::<DeckState>();
    let v = decks.get_decks().row_data(d as usize)?;
    (!(v.synced && decks.get_master() != d)).then_some(v.tempo)
}
fn tempo_hw_at(a: &App, d: i32) -> Option<f32> {
    let invert = prefs::with(|p| p.controller.tempo_invert);
    tempo_at(a, d).map(|p| hw_tempo_pos(p, invert))
}

pub(super) const TABLE: &[Action] = &[
    press_cb!("play", DeckState::invoke_play_pause),
    press_cb!("cue", DeckState::invoke_cue)
        .released_by(|a, d, _| a.global::<DeckState>().invoke_cue_released(d)),
    press_cb!("load", DeckState::invoke_load),
    press_cb!("sync", DeckState::invoke_sync),
    press_cb!("key-lock", DeckState::invoke_key_lock),
    press_cb!("quantize", DeckState::invoke_quantize),
    press_cb!("tempo-range", DeckState::invoke_tempo_range),
    press_cb!("zoom-in", DeckState::invoke_zoom, 1),
    press_cb!("zoom-out", DeckState::invoke_zoom, -1),
    press_cb!("loop-in", DeckState::invoke_loop_in),
    press_cb!("loop-out", DeckState::invoke_loop_out),
    press_cb!("reloop", DeckState::invoke_reloop),
    press_cb!("loop-halve", DeckState::invoke_loop_halve),
    press_cb!("loop-double", DeckState::invoke_loop_double),
    press_cb!("loop-1", DeckState::invoke_auto_loop, 1),
    press_cb!("loop-2", DeckState::invoke_auto_loop, 2),
    press_cb!("loop-4", DeckState::invoke_auto_loop, 0), // the controller's auto beat loop: the length of Preferences
    press_cb!("loop-8", DeckState::invoke_auto_loop, 8),
    press_cb!("loop-16", DeckState::invoke_auto_loop, 16),
    press_cb!("jump-back-4", DeckState::invoke_beat_jump, -4),
    press_cb!("jump-fwd-4", DeckState::invoke_beat_jump, 4),
    press_cb!("jump-back-1", DeckState::invoke_beat_jump, -1),
    press_cb!("jump-fwd-1", DeckState::invoke_beat_jump, 1),
    press_cb!("key-up", DeckState::invoke_key_step, 1),
    press_cb!("key-down", DeckState::invoke_key_step, -1),
    press_cb!("key-reset", DeckState::invoke_key_reset),
    press_cb!("key-sync", DeckState::invoke_key_sync),
    press_cb!("slip", DeckState::invoke_slip),
    press_cb!("reverse", DeckState::invoke_reverse),
    press_cb!("brake", DeckState::invoke_brake),
    press_cb!("grid-edit", DeckState::invoke_grid_toggle),
    press_cb!("grid-beat1", DeckState::invoke_grid_beat1),
    press_cb!("grid-double", DeckState::invoke_grid_double),
    press_cb!("grid-halve", DeckState::invoke_grid_halve),
    press_cb!("grid-tap", DeckState::invoke_grid_tap),
    press_cb!("grid-reset", DeckState::invoke_grid_reset),
    press_cb!("memory-cue", DeckState::invoke_memory_cue),
    press("pad-mode", |a, d| {
        let decks = a.global::<DeckState>();
        let mode = slint::Model::row_data(&decks.get_decks(), d as usize).map_or(0, |v| v.pad_mode);
        decks.invoke_pad_mode(d, (mode + 1) % crate::app::deck::PadMode::count());
    }),
    hot("pad1", pad::<0>),
    hot("pad2", pad::<1>),
    hot("pad3", pad::<2>),
    hot("pad4", pad::<3>),
    hot("pad5", pad::<4>),
    hot("pad6", pad::<5>),
    hot("pad7", pad::<6>),
    hot("pad8", pad::<7>),
    press_cb!("cue-monitor", MixerState::invoke_cue_toggled),
    press_any!("split", MixerState::invoke_split_toggled),
    press_any!("rec", MixerState::invoke_rec_toggled),
    press_any!("tag", BrowserState::invoke_tag_add),
    // STEMS: ACTIVE PART (mute or solo per PREFS), PART ISOLATOR on/off and its three knobs, the MUTE / SOLO setting.
    press_cb!("stem-vocal", DeckState::invoke_stem, 0),
    press_cb!("stem-inst", DeckState::invoke_stem, 1),
    press_cb!("stem-bass", DeckState::invoke_stem, 2),
    press_cb!("stem-drums", DeckState::invoke_stem, 3),
    press("stem-mode", |_, _| {
        crate::app::prefs::update(|p| p.active_part_solo = !p.active_part_solo)
    }),
    press_cb!("iso-toggle", MixerState::invoke_iso_toggled),
    knob("iso-vocal", iso_knob::<0>).read_by(iso_knob_at::<0>),
    knob("iso-inst", iso_knob::<1>).read_by(iso_knob_at::<1>),
    knob("iso-drums", iso_knob::<2>).read_by(iso_knob_at::<2>),
    // BEAT LAB: DRUM SWAP GAIN of the deck (the channel nibble).
    knob_cb!("drum-gain", GrooveState::invoke_master_edited),
    // FX: the deck column is the Beat FX unit (A = FX1, B = FX2); without a slot number the unit's BEAT target slot.
    press("fx-toggle", |a, u| {
        let s = a.global::<FxState>();
        s.invoke_toggled(u, fx_target(a, u))
    }),
    press_cb!("fx-beat-up", FxState::invoke_beat_step, 1),
    press_cb!("fx-beat-down", FxState::invoke_beat_step, -1),
    press_cb!("merge-abort", FxState::invoke_merge_abort),
    // The controller's SAMPLER button: the deck's pads show the SAMPLER mode, and Auto Panel Mode opens the sampler.
    press("sampler-open", |a, d| {
        a.global::<DeckState>()
            .invoke_pad_mode(d, crate::app::deck::PadMode::Sampler as i32);
        crate::app::sampler::controller_button(a)
    }),
    press("help", |a, _| {
        let k = a.global::<KeysState>();
        k.set_help_visible(!k.get_help_visible());
    }),
    knob_cb!("tempo", DeckState::invoke_tempo).read_by(tempo_at),
    knob("tempo-hw", tempo_hw).read_by(tempo_hw_at),
    knob_cb!("fader", MixerState::invoke_fader).read_by(fader_at),
    knob("crossfader", |a, _, v| {
        let m = a.global::<MixerState>();
        m.set_crossfader(v);
        m.invoke_crossfade(v);
    })
    .read_by(|a, _| Some(a.global::<MixerState>().get_crossfader())),
    knob("trim", mix_knob::<0>).read_by(mix_knob_at::<0>),
    knob("eq-hi", mix_knob::<1>).read_by(mix_knob_at::<1>),
    knob("eq-mid", mix_knob::<2>).read_by(mix_knob_at::<2>),
    knob("eq-low", mix_knob::<3>).read_by(mix_knob_at::<3>),
    knob("filter", mix_knob::<4>).read_by(mix_knob_at::<4>),
    knob("master", bus_knob::<0>).read_by(bus_knob_at::<0>),
    knob("hp-mix", bus_knob::<1>).read_by(bus_knob_at::<1>),
    knob("hp-level", bus_knob::<2>).read_by(bus_knob_at::<2>),
    // The AUX strip (the deck column is ignored): its LEVEL fader, headphone CUE and ON switch.
    knob("aux-level", |a, _, v| {
        a.global::<MixerState>().invoke_fader(AUX_CH as i32, v)
    })
    .read_by(|a, _| fader_at(a, AUX_CH as i32)),
    press("aux-cue", |a, _| {
        a.global::<MixerState>().invoke_cue_toggled(AUX_CH as i32)
    }),
    press("aux-on", |a, _| {
        a.global::<MixerState>().invoke_aux_toggled()
    }),
    knob("fx-level", |a, u, v| {
        a.global::<FxState>()
            .invoke_level_edited(u, fx_target(a, u), v)
    })
    .read_by(fx_target_level_at),
    knob("fx-level-1", fx_level::<0>).read_by(fx_level_at::<0>),
    knob("fx-level-2", fx_level::<1>).read_by(fx_level_at::<1>),
    knob("fx-level-3", fx_level::<2>).read_by(fx_level_at::<2>),
    // An absolute BEAT knob: its travel spans the beat list.
    knob("fx-beat", |a, u, v| {
        let s = a.global::<FxState>();
        let to = (v.clamp(0.0, 1.0) * (BEATS.len() - 1) as f32).round() as i32;
        let at = fx_slot(a, u, fx_target(a, u)).map_or(to, |x| x.beat);
        if to != at {
            s.invoke_beat_step(u, to - at);
        }
    }),
    // A BEAT encoder: one beat step per detent.
    rel("fx-beat-rel", true, |a, u, t| {
        a.global::<FxState>().invoke_beat_step(u, t.signum())
    }),
    knob_cb!("merge-knob", FxState::invoke_merge_knob).read_by(merge_at::<false>),
    knob_cb!("merge-volume", FxState::invoke_merge_volume).read_by(merge_at::<true>),
    // Relative CC centred on 64 (jog wheels) / two's complement (encoders).
    jog("jog", false, JogMode::Scratch),
    jog("jog-2c", true, JogMode::Scratch),
    jog("jog-bend", false, JogMode::Bend),
    jog("jog-search", false, JogMode::Search),
    jog("jog-cut", false, JogMode::Cut),
    Action::new("jog-touch", Kind::Touch),
    Action::new("vinyl", Kind::Vinyl),
    Action::new("jog-cutter", Kind::Cutter),
    rel("browse", true, |a, _, t| a.invoke_browse(t.clamp(-8, 8))),
    press("browse-back", |a, _| a.invoke_browse_back()),
    press("browse-view", |a, _| a.invoke_browse_view()),
    press("browse-area", |a, _| a.invoke_browse_area()),
    // Tree focused: open it (move to the tracks); tracks focused: load the chosen track into the first idle deck.
    press("browse-push", |a, _| {
        if !a.invoke_browse_push() {
            return;
        }
        let decks = a.global::<DeckState>();
        let d = slint::Model::iter(&decks.get_decks())
            .take(decks.get_count() as usize)
            .position(|d| !d.playing)
            .unwrap_or(0);
        decks.invoke_load(d as i32);
    }),
    rel("zoom-rot", true, |a, _, t| {
        for d in 0..crate::app::deck::active() as i32 {
            a.global::<DeckState>().invoke_zoom(d, t.signum());
        }
    }),
];

#[cfg(test)]
mod tests {
    use super::hw_tempo_pos;

    #[test]
    fn hardware_tempo_faders_report_zero_at_the_top_unless_inverted() {
        // top of the fader (+ end, fastest) is position 1 in the app
        assert_eq!(hw_tempo_pos(0.0, false), 1.0);
        assert_eq!(hw_tempo_pos(1.0, false), 0.0);
        assert_eq!(hw_tempo_pos(0.0, true), 0.0);
        assert_eq!(hw_tempo_pos(1.0, true), 1.0);
        assert_eq!(hw_tempo_pos(0.5, false), 0.5);
    }
}
