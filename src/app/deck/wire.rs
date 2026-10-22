//! Creates the decks and routes the `DeckState` callbacks.

use super::{DECKS, Deck, Decks, active, frame, mixpoint, padopts, sync, tick};
use crate::app::{App, DeckScroll, DeckState, DeckView, prefs};
use openfader::{
    analysis,
    library::{Library, Track},
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};

/// Creates the decks and routes every `DeckState` callback to the deck it names. LOAD loads
/// whatever `selected` holds (the browser's current track), taking its analysis from `lib` first.
pub fn wire(app: &App, lib: Rc<dyn Library>, selected: Rc<RefCell<Option<Track>>>) -> Decks {
    let model = Rc::new(VecModel::from(vec![DeckView::default(); DECKS]));
    let state = app.global::<DeckState>();
    state.set_decks(ModelRc::from(model.clone()));
    let scroll = Rc::new(VecModel::from(vec![DeckScroll::default(); DECKS]));
    state.set_scroll(ModelRc::from(scroll.clone()));
    state.set_cue_palette(ModelRc::new(VecModel::from(
        analysis::picker()
            .map(|[r, g, b]| slint::Color::from_rgb_u8(r, g, b))
            .collect::<Vec<_>>(),
    )));
    state.set_pad_mode_names(super::pads::mode_names(false));
    state.set_pad_mode_names_stems(super::pads::mode_names(true));
    let link = Rc::new(sync::Link::default());
    let decks: Decks = std::array::from_fn(|i| {
        Deck::new(
            app,
            i,
            model.clone(),
            scroll.clone(),
            link.clone(),
            lib.clone(),
        )
    });
    sync::connect(&decks);
    tick::wire_view(app, &decks);
    frame::wire(app, &decks);
    padopts::wire(app, &decks);
    on!(state, at decks, on_detail_resized, |d, w, h| d.detail_size.set((w, h)));
    on!(state, at decks, on_overview_resized, |d, w, h| d.overview_size.set((w, h)));
    on!(state, at decks, on_load, |d| match selected.borrow().as_ref() {
        Some(t) => d.load(t, &*lib),
        None => d.say(tr!("select a track in the library first")),
    });
    on!(state, at decks, on_play_pause, |d| d.play_pause());
    on!(state, at decks, on_seek, |d, f| d.seek_frac(f));
    on!(state, at decks, on_tempo, |d, p| d.tempo(p));
    on!(state, at decks, on_tempo_range, |d| d.cycle_range());
    on!(state, at decks, on_key_lock, |d| d.toggle_key_lock());
    on!(state, at decks, on_zoom, |d, dir| d.zoom(dir));
    on!(state, at decks, on_quantize, |d| d.toggle_quantize());
    on!(state, at decks, on_beat_jump, |d, n| d.beat_jump(n));
    on!(state, at decks, on_pad, |d, p| d.pad(p as usize));
    on!(state, at decks, on_memory_cue, |d| d.memory_cue());
    on!(state, at decks, on_cue_delete, |d| d.toggle_delete());
    on!(state, at decks, on_auto_loop, |d, n| d.auto_loop(n.max(0) as u32)); // 0 = the length of PREFS > Auto Beat Loop
    on!(state, at decks, on_loop_in, |d| d.loop_in());
    on!(state, at decks, on_loop_out, |d| d.loop_out());
    on!(state, at decks, on_reloop, |d| d.reloop());
    on!(state, at decks, on_loop_halve, |d| d.loop_halve());
    on!(state, at decks, on_loop_double, |d| d.loop_double());
    on!(state, at decks, on_cue_jump, |d, row| d.cue_jump(row as usize));
    on!(state, at decks, on_save_loop, |d| d.save_loop());
    on!(state, at decks, on_cue_color, |d, row, c| d.cue_color(row as usize, c as usize)); // -1 = default colour (wraps out of range)
    on!(state, at decks, on_cue_comment, |d, row, t| d.cue_comment(row as usize, t.to_string()));
    on!(state, at decks, on_pad_mode, |d, m| d.set_pad_mode(m));
    on!(state, at decks, on_pad_released, |d| d.pad_released());
    on!(state, at decks, on_key_step, |d, n| d.key_step(n));
    on!(state, at decks, on_key_reset, |d| d.set_key_shift(0));
    on!(state, at decks, on_slip, |d| d.toggle_slip());
    on!(state, at decks, on_reverse, |d| d.toggle_reverse());
    on!(state, at decks, on_brake, |d| d.brake());
    on!(state, at decks, on_grid_toggle, |d| d.toggle_grid_edit());
    on!(state, at decks, on_grid_shift, |d, ms| d.grid_shift(ms));
    on!(state, at decks, on_grid_beat1, |d| d.grid_beat1());
    on!(state, at decks, on_grid_double, |d| d.grid_double());
    on!(state, at decks, on_grid_halve, |d| d.grid_halve());
    on!(state, at decks, on_grid_tap, |d| d.grid_tap());
    on!(state, at decks, on_grid_reset, |d| d.grid_reset());
    on!(state, at decks, on_metronome, |d| d.toggle_metronome());
    on!(state, at decks, on_cue, |d| d.cue_press());
    on!(state, at decks, on_cue_released, |d| d.cue_release());
    on!(state, at decks, on_sync, |d| d.sync_toggle());
    on!(state, at decks, on_master_toggle, |d| d.master_toggle());
    on!(state, at decks, on_scratch_begin, |d| d.scratch_begin());
    on!(state, at decks, on_scratch_move, |d, dx| d.scratch_drag(dx));
    on!(state, at decks, on_scratch_end, |d| d.scratch_end());
    on!(state, at decks, on_key_sync, |d| d.key_sync());
    on!(state, at decks, on_stem, |d, k| d.stem_press(k as usize));
    on!(state, at decks, on_stem_click, |d, k| d.stem_click(k as usize));
    on!(state, at decks, on_stem_double, |d, k| d.stem_double(k as usize));
    // The 2/4-deck layout: the UI shows `count` decks; decks C and D leave the layout paused.
    state.set_count(active() as i32);
    decks.iter().skip(active()).for_each(|d| d.park(active()));
    let (d, weak) = (decks.clone(), app.as_weak());
    prefs::on_change(move |p| {
        for deck in &d {
            deck.refresh_key();
            let grid = super::grid_bpm(&deck.loaded.borrow().analysis.beats);
            deck.show(|v| v.bpm_text = super::bpm_text(grid, deck.rate.get()));
            deck.rewave();
        }
        if let Some(app) = weak.upgrade() {
            app.global::<DeckState>().set_count(active() as i32);
        }
        d.iter().skip(p.decks).for_each(|deck| deck.park(p.decks));
        d.iter().for_each(|deck| deck.stems_pref_changed());
        d.iter().for_each(|deck| {
            deck.sync.refresh(&p.controller);
            deck.apply_vinyl();
            deck.show(|v| v.auto_beats = p.controller.loop_beats() as i32);
            deck.refresh_pads(); // pad colours and the sampler slot count
        });
    });
    mixpoint::wire(app, &decks);
    decks
}
