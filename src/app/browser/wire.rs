//! Builds the [`View`] and connects the `BrowserState` callbacks to it.

use super::{
    cover, editor,
    filter::Filter,
    history, palette,
    playlists::{self, Panel},
    preview, session,
    sidebar::{Entry, TAG_ROW},
    stages, strip, suggest,
    table::Reference,
    tagging, track_at, Home, View, DOUBLE_CLICK,
};
use crate::app::{
    App, AutomixState, BrowserState, DeckState, analyze::detected, automix, deck::Decks,
    library::settings, prefs, trackinfo,
};
use openfader::{
    library::{Library, Track},
    store,
};
use slint::{ComponentHandle, Model, Timer, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::Instant,
};

/// Layout > Media Browser "Related Tracks": the RELATED tab and button. "Track Suggestion" has its own SUGGEST tab ([`suggest`]).
fn related_shown(p: &prefs::Prefs) -> bool {
    p.view.media_related
}

/// Chooses `t` (track panel, the deck-side callback) and loads it into the first deck that is not playing.
pub(super) fn load_to_deck(app: &App, on_track: &dyn Fn(&Track), t: &Track) {
    trackinfo::show(app, t);
    on_track(t);
    let decks = app.global::<DeckState>();
    match decks
        .get_decks()
        .iter()
        .take(decks.get_count() as usize)
        .position(|d| !d.playing)
    {
        Some(d) => decks.invoke_load(d as i32),
        None => app
            .global::<BrowserState>()
            .set_status(tr!("every deck is playing; pause one to load").into()),
    }
}

/// The chosen row became `t`: the panels that follow it, then the deck-side hook.
fn chosen(v: &Rc<RefCell<View>>, app: &App, t: &Track, on_selected: &dyn Fn(&Track)) {
    v.borrow().suggest_selected(app);
    v.borrow().health_selected(app, t);
    trackinfo::show(app, t);
    on_selected(t);
}

/// Fills the playlist tree, lists all tracks, and calls `on_track` when a track row is chosen.
pub fn wire(
    app: &App,
    lib: Rc<dyn Library>,
    decks: Decks,
    on_track: impl Fn(&Track) + 'static,
) -> Result<(), String> {
    let lib_playlists = lib.playlists()?;
    trackinfo::use_library(lib.clone());
    let state = app.global::<BrowserState>();
    let sidebar = Rc::new(VecModel::default());
    state.set_playlists(sidebar.clone().into());
    state.set_tag_row(TAG_ROW as i32);
    let panel_rows = Rc::new(VecModel::default());
    state.set_panel_rows(panel_rows.clone().into());
    let automix = automix::wire(app, decks.clone(), lib.clone());
    let store = store::global();
    let played = super::played::load(&store);
    let rows = Rc::new(super::table::Rows::new(played));
    state.set_tracks(rows.clone().into());
    let marks = Rc::new(VecModel::default());
    state.set_selected_rows(marks.clone().into());
    tagging::ensure_defaults(&store);
    let view = Rc::new(RefCell::new(View {
        lib,
        store,
        rows,
        sidebar,
        panel_rows,
        list: Rc::default(),
        shown: Vec::new(),
        query: String::new(),
        filter: Filter::default(),
        tag_filter: Vec::new(),
        saved: Vec::new(),
        sort: None,
        home: Home::Playlist(None),
        related: None,
        tags: Vec::new(),
        history: Vec::new(),
        selected: None,
        sel: Default::default(),
        sel_entry: Entry::All,
        marks,
        status_base: String::new(),
        reference: Reference::default(),
        detected_ver: detected().version(),
        sel_at: None,
        entries: Vec::new(),
        entry: Entry::All,
        open: Vec::new(),
        lib_playlists,
        nodes: Vec::new(),
        cats: Vec::new(),
        mytags: Vec::new(),
        banks: Vec::new(),
        decks: decks.clone(),
        tag_map: HashMap::new(),
        cache: None,
        slots: Vec::new(),
        dupe_groups: 0,
        health: super::health::Monitor::new(),
        ask: None,
        panel: Panel::Closed,
        editing: None,
        confirm: None,
        sources: Vec::new(),
        strips: strip::Strips::new(app),
        stages: stages::Stages::default(),
        covers: cover::Covers::new(app),
        _timer: Timer::default(),
        _preview: Timer::default(),
        suggest: suggest::State::default(),
    }));
    {
        let mut v = view.borrow_mut();
        v.reload_tag_map(app);
        v.rebuild(app);
        v.show(app);
        v.restore_tag_list(app);
        v.refresh_sources(app);
    }
    let preview = preview::wire(app, &view);
    view.borrow_mut()._preview = preview;
    let (v, weak) = (view.clone(), app.as_weak());
    prefs::on_action("played_reset", move || {
        v.borrow_mut().reset_played(&weak.unwrap())
    });
    session::start_poll(app, &view, decks);
    wire_prefs(app, &view);
    super::health::wire(app, &view);
    super::saved::wire(app, &view);
    wire_list(app, &view);
    wire_selection(app, &view, automix, on_track);
    Ok(())
}

/// The settings that change what the rows or the tree show: key names, the traffic light, the played colour, the
/// playlist-tree panels (All Tracks, CUE Analysis), and where a chosen row scrolls to.
fn wire_prefs(app: &App, view: &Rc<RefCell<View>>) {
    let shape = |p: &prefs::Prefs| {
        (
            p.key_notation,
            p.view.traffic_light,
            p.view.played_color,
            p.view.tempo_decimals,
        )
    };
    let panels = |p: &prefs::Prefs| {
        (
            p.view.panel_all_tracks,
            p.view.panel_cue_analysis,
            p.view.media_hot_cue_bank,
        )
    };
    let (v, weak, shown, tree) = (
        view.clone(),
        app.as_weak(),
        Cell::new(prefs::with(shape)),
        Cell::new(prefs::with(panels)),
    );
    prefs::on_change(move |p| {
        let Some(app) = weak.upgrade() else { return };
        let state = app.global::<BrowserState>();
        state.set_related_enabled(related_shown(p));
        state.set_center_selected(p.view.center_selected);
        let mut v = v.borrow_mut();
        if !related_shown(p) && v.related.take().is_some() {
            v.load(&app); // the Related view vanished with its setting
        }
        if tree.replace(panels(p)) != panels(p) {
            let before = v.entry.clone();
            v.rebuild(&app);
            if v.entry != before {
                v.show(&app); // the chosen entry vanished with its panel
            }
        }
        if shown.replace(shape(p)) != shape(p) {
            v.refresh(&app);
        }
    });
    let state = app.global::<BrowserState>();
    let apply = |app: &App, l: &settings::Lib| {
        let s = app.global::<BrowserState>();
        s.set_hold_menu(l.dropdown);
        s.set_edit_library(l.edit);
    };
    prefs::with(|p| {
        state.set_related_enabled(related_shown(p));
        state.set_center_selected(p.view.center_selected);
        apply(app, &settings::Lib::of(p));
    });
    let weak = app.as_weak();
    prefs::on_change(move |p| {
        if let Some(app) = weak.upgrade() {
            apply(&app, &settings::Lib::of(p));
        }
    });
}

/// Search, filter, sort, Related, the Tag List and the queue edits of the track list.
fn wire_list(app: &App, view: &Rc<RefCell<View>>) {
    let state = app.global::<BrowserState>();
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_history_save(move || {
        let msg = match history::save(&v.borrow().list) {
            Ok((path, n)) => tr_n!(
                "saved {n} track to {}",
                "saved {n} tracks to {}",
                n,
                path.display()
            ),
            Err(e) => tr!("save failed: {}", e),
        };
        weak.unwrap()
            .global::<BrowserState>()
            .set_status(msg.into());
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_playlist_selected(move |i| {
        if let Ok(i) = usize::try_from(i) {
            v.borrow_mut().select(&weak.unwrap(), i);
        }
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_tree_toggle(move |i| {
        if let Ok(i) = usize::try_from(i) {
            v.borrow_mut().toggle(&weak.unwrap(), i);
        }
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_search_edited(move |text| {
        let mut v = v.borrow_mut();
        v.query = text.to_string();
        v.refresh(&weak.unwrap());
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_search_accepted(move |text| v.borrow_mut().search_online(&weak.unwrap(), &text));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_filter_changed(move || {
        let app = weak.unwrap();
        let s = app.global::<BrowserState>();
        // a decimal comma is what the field takes in most UI languages
        let bound = |t: slint::SharedString| {
            t.trim()
                .replace(',', ".")
                .parse::<f64>()
                .unwrap_or(0.0)
                .max(0.0)
        };
        let mut v = v.borrow_mut();
        v.filter = Filter {
            bpm_min: bound(s.get_bpm_min()),
            bpm_max: bound(s.get_bpm_max()),
            around_master: s.get_bpm_around(),
            key_match: s.get_key_match(),
            min_rating: s.get_min_rating().clamp(0, 5) as u8,
        };
        v.refresh(&app);
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_sort(move |col, asc| {
        let mut v = v.borrow_mut();
        v.sort = Some((col as usize, asc));
        v.refresh(&weak.unwrap());
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_related_toggled(move |on| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        let on = on && prefs::with(related_shown);
        if on {
            v.suggest_off();
        }
        v.related = if on { v.selected.clone() } else { None };
        if on && v.related.is_none() {
            app.global::<BrowserState>()
                .set_status(tr!("select a track to see what relates to it").into());
        }
        v.load(&app);
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_tag_add(move || {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        let msg = match v.selection().as_slice() {
            [] => tr!("select a track to tag").to_owned(),
            [t] if v.tags.contains(t) => tr!("“{}” is already in the Tag List", t.title),
            [t] => {
                let msg = tr!(
                    "tagged “{}” ({} in the Tag List)",
                    t.title,
                    v.tags.len() + 1
                );
                v.tags.push(t.clone());
                v.tags_changed(&app);
                msg
            }
            many => {
                let fresh: Vec<Track> = many
                    .iter()
                    .filter(|t| !v.tags.contains(t))
                    .cloned()
                    .collect();
                let n = fresh.len();
                v.tags.extend(fresh);
                v.tags_changed(&app);
                tr!("{} tracks added to the Tag List", n)
            }
        };
        app.global::<BrowserState>().set_status(msg.into());
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_list_move(move |d| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        // Swap with the neighbour as shown (a search/filter may hide queue entries in between).
        let at = v
            .cursor(&app)
            .and_then(|i| v.shown.iter().position(|&s| s == i));
        let other = at
            .and_then(|n| n.checked_add_signed(d as isize))
            .and_then(|n| v.shown.get(n))
            .copied();
        if let (Some(a), Some(o)) = (v.cursor(&app), other) {
            v.sel_at = Some(o); // the chosen entry lands where the neighbour was
            v.move_entry(&app, a, o);
        }
    });
}

/// Choosing, loading and removing tracks, and the sections that hang off the table (playlists, palette, strips, AUTOMIX).
fn wire_selection(
    app: &App,
    view: &Rc<RefCell<View>>,
    automix: Rc<automix::Automix>,
    on_track: impl Fn(&Track) + 'static,
) {
    let state = app.global::<BrowserState>();
    let v = view.clone();
    app.global::<AutomixState>().on_toggled(move || {
        if automix.running() {
            automix.stop(tr!("AUTOMIX stopped"));
        } else {
            automix.start(v.borrow().queue());
        }
    });
    let on_track = Rc::new(on_track);
    let (v, weak, on_removed) = (view.clone(), app.as_weak(), on_track.clone());
    state.on_list_remove(move || {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        let Some(pos) = v.cursor(&app) else { return };
        // Every selected entry goes, last first so the positions still to remove stay valid.
        let mut rows = v.sel.ordered(&v.shown);
        if rows.len() < 2 {
            rows = vec![pos];
        }
        rows.sort_unstable();
        v.sel.clear();
        let removed = rows
            .iter()
            .rev()
            .take_while(|&&at| v.remove_at(&app, at))
            .count();
        if removed == 0 {
            return;
        }
        let pos = rows[rows.len() - removed];
        // The neighbour inherits the selection so the arrow keys keep working down the queue.
        v.sel_at = Some(if pos < v.list.len() {
            pos
        } else {
            pos.saturating_sub(1)
        });
        v.selected = v
            .list
            .get(pos)
            .or_else(|| pos.checked_sub(1).and_then(|p| v.list.get(p)))
            .cloned();
        let (app, v) = (&app, &mut *v);
        v.refresh(app);
        if let Some(t) = &v.selected {
            trackinfo::show(app, t);
            on_removed(t);
        }
    });
    let (v, weak, on_selected) = (view.clone(), app.as_weak(), on_track.clone());
    // A plain click or key: only this row is selected.
    state.on_track_selected({
        let on_selected = on_selected.clone();
        let (v, weak) = (v.clone(), weak.clone());
        move |i| {
            if let Some(t) = track_at(&v, i) {
                let app = weak.unwrap();
                {
                    let mut view = v.borrow_mut();
                    view.sel_at = usize::try_from(i)
                        .ok()
                        .and_then(|i| view.shown.get(i).copied());
                    view.selected = Some(t.clone());
                    if let Some(at) = view.sel_at {
                        view.sel.pick(at);
                    }
                    view.publish_selection(&app);
                }
                chosen(&v, &app, &t, &*on_selected);
            }
        }
    });
    // Ctrl+click (mode 1) and Shift+click / Shift+arrows (mode 2): the selection grows or shrinks.
    let (vv, weak2) = (v.clone(), weak.clone());
    state.on_track_picked(move |i, mode| {
        let app = weak2.unwrap();
        let t = {
            let mut view = vv.borrow_mut();
            let view = &mut *view;
            let Some((row, &at)) = usize::try_from(i)
                .ok()
                .and_then(|i| view.shown.get(i).map(|s| (i, s)))
            else {
                return;
            };
            let was_chosen = view.sel_at == Some(at);
            if mode == 1 {
                view.sel.toggle(at);
            } else {
                view.sel.range(&view.shown, at);
            }
            let row = if view.sel.contains(at) {
                row
            } else if !was_chosen {
                return view.publish_selection(&app); // another row left; the chosen one stays
            } else {
                // The chosen row left: the nearest selected row takes over (the last selected row cannot leave).
                let rows: Vec<usize> = (0..view.shown.len())
                    .filter(|&r| view.sel.contains(view.shown[r]))
                    .collect();
                match rows.iter().rev().find(|&&r| r < row).or(rows.first()) {
                    Some(&r) => r,
                    None => {
                        view.sel.add(at);
                        row
                    }
                }
            };
            view.sel_at = Some(view.shown[row]);
            let t = view.list[view.shown[row]].clone();
            view.selected = Some(t.clone());
            view.publish_selection(&app);
            app.global::<BrowserState>().set_current_row(row as i32);
            t
        };
        chosen(&vv, &app, &t, &*on_selected);
    });
    let (vv, weak2) = (v.clone(), weak.clone());
    state.on_select_all(move || {
        let mut view = vv.borrow_mut();
        let view = &mut *view;
        view.sel.all(&view.shown);
        view.publish_selection(&weak2.unwrap());
    });
    let (vv, weak2) = (v.clone(), weak.clone());
    state.on_select_collapse(move || {
        let mut view = vv.borrow_mut();
        match view.sel_at {
            Some(at) => view.sel.pick(at),
            None => view.sel.clear(),
        }
        view.publish_selection(&weak2.unwrap());
    });
    playlists::wire(app, view);
    strip::wire(app, view);
    cover::wire(app, view);
    suggest::wire(app, view);
    palette::wire(app, view, on_track.clone());
    super::VIEW.with(|v| *v.borrow_mut() = Some(Rc::downgrade(view)));
    super::super::deck::set_next_track({
        let v = view.clone();
        move |t| v.borrow().next_after(t)
    });
    let (v, weak, on_reloaded) = (view.clone(), app.as_weak(), on_track.clone());
    state.on_reload(move |select| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        v.cache = None;
        v.reread_metadata();
        v.lib_playlists = v.lib.playlists().unwrap_or_default();
        if select >= 0 {
            v.entry = Entry::User(i64::from(select));
        }
        v.rebuild(&app);
        v.show(&app);
        if let Some(t) = &v.selected {
            trackinfo::show(&app, t);
            on_reloaded(t);
        }
    });
    let (v, weak, on_found) = (view.clone(), app.as_weak(), on_track.clone());
    state.on_select_file(move |file| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        (v.entry, v.query) = (Entry::All, String::new());
        v.show(&app);
        let found = v
            .list
            .iter()
            .find(|t| t.file.as_deref() == Some(std::path::Path::new(file.as_str())))
            .cloned();
        if let Some(t) = found {
            v.selected = Some(t.clone());
            v.sel.clear();
            v.refresh(&app);
            trackinfo::show(&app, &t);
            on_found(&t);
        }
    });
    editor::wire(app, view);
    let (v, weak, on_edited) = (view.clone(), app.as_weak(), on_track.clone());
    trackinfo::on_edit(move |t| {
        if let Some(app) = weak.upgrade() {
            v.borrow_mut().track_edited(&app, t);
            if let Some(selected) = &v.borrow().selected {
                on_edited(selected);
            }
        }
    });
    // Two left presses on one row within DOUBLE_CLICK select it and load it into the first deck that is not playing.
    let (last, weak, view) = (
        Cell::new(None::<(Instant, i32)>),
        app.as_weak(),
        view.clone(),
    );
    state.on_track_pressed(move |i| {
        let now = Instant::now();
        let double = last
            .get()
            .is_some_and(|(t, row)| row == i && now - t < DOUBLE_CLICK);
        last.set((!double).then_some((now, i)));
        let Some(t) = track_at(&view, i).filter(|_| double) else {
            return;
        };
        load_to_deck(&weak.unwrap(), &*on_track, &t);
    });
}
