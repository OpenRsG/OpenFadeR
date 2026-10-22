//! EXPORT mode ↔ ui/exportmode*.slint (global `ExportModeState`). The players are decks A and B of
//! [`super::deck`]: their transport, waveforms, loops and cues are the PERFORMANCE ones (`DeckState`), so nothing of the deck
//! engine is copied. This module adds what the EXPORT screen has on top: the GRID panel's undo / redo / widen / narrow / range /
//! typed BPM / Analysis Lock (`Deck::grid_*` in `deck/exportedit.rs`), the hot cue buttons and the MEMORY / HOT CUE / INFO panels'
//! data ([`info`]), DUAL CONTROL (PLAY on one player starts both), pitch bend, the ≡ menu's ANALYZE TRACK and [Matching] (a pair
//! of tracks registered as related, `store::matching`). The mixer parts of the DUAL PLAYER layout (crossfader, AUTO GAIN, 3 BAND
//! EQ) act on `MixerState` directly, the layout selector and the device dock are placed by ui/app.slint.
//! The PHRASE EDIT row of the GRID panel is the child [`phrase`].
//!
//! One timer (100 ms) copies the cheap per-deck state into `ExportModeState.decks` while the mode is on; a row is written only
//! when something in it changed.

mod info;
mod phrase;

use super::{
    App, DeckState, ExportDeck, ExportModeState, HotCue, analyze,
    deck::{Deck, Decks},
    prefs, toast,
};
use openfader::{library::Track, store};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

/// Players shown: decks A and B.
const PLAYERS: usize = 2;
/// DUAL PLAYER is layout 2 of `ExportModeState.layout`.
const DUAL: i32 = 2;

/// Keeps the refresh timer (and with it the whole mode) alive for as long as the window runs.
pub struct Guard {
    _timer: Timer,
}

/// A BPM typed into the GRID panel: 30-300, a comma counts as the decimal point.
fn parse_bpm(text: &str) -> Option<f64> {
    let v: f64 = text.trim().replace(',', ".").parse().ok()?;
    (30.0..=300.0).contains(&v).then_some(v)
}

/// What the loaded-track part of a row was last read for: the deck's load counter.
#[derive(Default, Clone, Copy, PartialEq)]
struct Loads([Option<u32>; PLAYERS]);

/// The 8 hot cue buttons of `deck`, each with its row in the cue list (`cues` = the deck's `DeckView.cues`).
fn hot_cues(deck: &Deck, cues: Option<&ModelRc<super::CueRowView>>) -> Vec<HotCue> {
    deck.hot_views()
        .into_iter()
        .map(|p| {
            let row = cues.and_then(|c| {
                (0..c.row_count()).find(|&r| c.row_data(r).is_some_and(|x| x.tag == p.label))
            });
            let cue = row.and_then(|r| cues.and_then(|c| c.row_data(r)));
            HotCue {
                row: row.map_or(-1, |r| r as i32),
                comment: cue.as_ref().map(|c| c.comment.clone()).unwrap_or_default(),
                len: cue
                    .filter(|c| c.kind == "loop")
                    .map(|c| c.len)
                    .unwrap_or_default(),
                label: p.label,
                color: p.color,
                set: p.set,
                time: p.time,
            }
        })
        .collect()
}

/// The two loaded tracks, when both players have one.
fn pair(decks: &Decks) -> Option<(Track, Track)> {
    Some((decks[0].track()?.1, decks[1].track()?.1))
}

/// Copies what changed of each player into its `ExportDeck` row. The track-dependent parts (info panel, MATCHING flag)
/// are read only when a deck was reloaded.
fn refresh(app: &App, decks: &Decks, model: &VecModel<ExportDeck>, seen: &mut Loads) {
    let views = app.global::<DeckState>().get_decks();
    let serials = Loads(std::array::from_fn(|i| decks[i].serial()));
    let reloaded = serials != *seen;
    for (i, deck) in decks.iter().take(PLAYERS).enumerate() {
        let Some(mut row) = model.row_data(i) else {
            continue;
        };
        let before = row.clone();
        let g = deck.grid_info();
        (row.can_undo, row.can_redo, row.from_here, row.grid_locked) =
            (g.can_undo, g.can_redo, g.from_here, g.locked);
        row.grid_bpm = g.bpm.into();
        phrase::fill(deck, &mut row, g.locked);
        let cues = views.row_data(i).map(|v| v.cues);
        let hot = hot_cues(deck, cues.as_ref());
        if !row.hot.iter().eq(hot.iter().cloned()) {
            row.hot = ModelRc::new(VecModel::from(hot));
        }
        if seen.0[i] != serials.0[i] {
            seen.0[i] = serials.0[i];
            let Some((_, track)) = decks[i].track() else {
                continue;
            };
            let t = info::of(&track);
            row.has_art = t.art.is_some();
            row.artwork = t.art.unwrap_or_default();
            row.rating = t.rating;
            row.has_color = t.color.is_some();
            row.color = t.color.map(|c| c.1).unwrap_or_default();
            row.color_name = t.color.map_or("–", |c| c.0).into();
            row.comment = t.comment.into();
            row.file_type = t.file_type.into();
            row.file_size = t.size.into();
            row.sample_rate = t.sample_rate.into();
            row.bit_rate = t.bit_rate.into();
        }
        if i == 0 && reloaded {
            row.matched = pair(decks).is_some_and(|(a, b)| {
                store::global()
                    .is_matched(&store::key(&a), &store::key(&b))
                    .unwrap_or(false)
            });
        }
        if row != before {
            model.set_row_data(i, row);
        }
    }
}

/// PLAY of player `i`; with DUAL CONTROL both players end up in the same state (the other one only toggles when it was in
/// the state `i` was in).
fn play(decks: &Decks, i: usize, dual: bool) {
    let (was, other_was) = (decks[i].playing(), decks[1 - i].playing());
    decks[i].play_pause();
    if dual && was == other_was {
        decks[1 - i].play_pause();
    }
}

pub fn wire(app: &App, decks: Decks) -> Guard {
    let state = app.global::<ExportModeState>();
    let model = Rc::new(VecModel::from(vec![ExportDeck::default(); PLAYERS]));
    state.set_decks(ModelRc::from(model.clone()));
    phrase::wire(app, &decks, &model);
    let weak = app.as_weak();
    let players: [Rc<Deck>; PLAYERS] = std::array::from_fn(|i| decks[i].clone());
    on!(state, at players, on_hot_cue, |d, p| d.hot_cue(p.clamp(0, 7) as usize));
    on!(state, at players, on_cue_remove, |d, row| if row >= 0 {
        d.cue_remove(row as usize)
    });
    on!(state, at players, on_cue_step, |d, dir| d.cue_call(dir.signum()));
    on!(state, at players, on_memory_delete, |d| d.memory_delete());
    on!(state, at players, on_bend, |d, v| d.bend(v));
    on!(state, at players, on_grid_undo, |d| d.grid_undo());
    on!(state, at players, on_grid_redo, |d| d.grid_redo());
    on!(state, at players, on_grid_stretch, |d, ms| d.grid_stretch(ms));
    on!(state, at players, on_grid_range, |d, from_here| d.grid_range(from_here));
    on!(state, at players, on_grid_lock, |d| d.toggle_grid_lock());
    {
        let w = weak.clone();
        on!(state, at players, on_grid_bpm, |d, text| match parse_bpm(&text) {
            Some(bpm) => d.grid_set_bpm(bpm),
            None => {
                if let Some(app) = w.upgrade() {
                    toast::show(&app, tr!("BPM: type a tempo between 30 and 300"));
                }
            }
        });
    }
    {
        let (w, decks) = (weak.clone(), decks.clone());
        state.on_play(move |i| {
            let dual = w.upgrade().is_some_and(|a| {
                let s = a.global::<ExportModeState>();
                s.get_layout() == DUAL && s.get_dual_control()
            });
            play(&decks, i as usize % PLAYERS, dual);
        });
    }
    {
        let (w, decks) = (weak.clone(), decks.clone());
        state.on_analyze(move |i| {
            let Some(app) = w.upgrade() else { return };
            let d = &decks[i as usize % PLAYERS];
            let msg = match d.track() {
                None => tr!("load a track first").to_owned(),
                Some(_) if d.grid_locked() => {
                    tr!("this track has an Analysis Lock: unlock it first").to_owned()
                }
                Some((_, t)) => {
                    analyze::request(std::slice::from_ref(&t));
                    tr!(
                        "analysing “{}”…{}",
                        t.title,
                        analyze::library_grid_notice(&t)
                    )
                }
            };
            toast::show(&app, &msg);
        });
    }
    {
        let (w, decks, model) = (weak.clone(), decks.clone(), model.clone());
        state.on_matching(move || {
            let Some(app) = w.upgrade() else { return };
            let Some((a, b)) = pair(&decks) else {
                return toast::show(&app, tr!("load a track in both players first"));
            };
            let (ka, kb) = (store::key(&a), store::key(&b));
            let s = store::global();
            let on = !s.is_matched(&ka, &kb).unwrap_or(false);
            let done = if on {
                s.add_match(&ka, &kb)
            } else {
                s.remove_match(&ka, &kb)
            };
            let msg = match (&done, on) {
                (Err(e), _) => tr!("matching: {}", e),
                (_, true) => tr!("“{}” and “{}” are now related tracks", a.title, b.title),
                (_, false) => tr!("“{}” and “{}” are no longer related", a.title, b.title),
            };
            toast::show(&app, &msg);
            if let Some(mut row) = model.row_data(0) {
                row.matched = on == done.is_ok();
                model.set_row_data(0, row);
            }
        });
    }
    // The settings the panels show: the metronome volume's level and the REC source.
    let mirror = {
        let w = weak.clone();
        move |p: &prefs::Prefs| {
            if let Some(app) = w.upgrade() {
                let s = app.global::<ExportModeState>();
                s.set_metro_level(match p.audio.metronome_volume {
                    0..=45 => 0,
                    46..=80 => 1,
                    _ => 2,
                });
                s.set_rec_source(p.audio.rec_source as i32);
            }
        }
    };
    prefs::with(&mirror);
    prefs::on_change(mirror);

    let (w, seen) = (weak, RefCell::new(Loads::default()));
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        if let Some(app) = w
            .upgrade()
            .filter(|a| a.global::<ExportModeState>().get_visible())
        {
            refresh(&app, &decks, &model, &mut seen.borrow_mut());
        }
    });
    Guard { _timer: timer }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_bpm_is_a_tempo_in_range() {
        assert_eq!(parse_bpm(" 128 "), Some(128.0));
        assert_eq!(parse_bpm("114,5"), Some(114.5));
        assert_eq!(
            [
                parse_bpm("29.9"),
                parse_bpm("300.1"),
                parse_bpm("abc"),
                parse_bpm("")
            ],
            [None; 4]
        );
    }
}
