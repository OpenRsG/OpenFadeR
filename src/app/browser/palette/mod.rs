//! Playlist Palette: Preferences > View > Layout > Browser panel > **Playlist Palette** puts a toggle
//! button in the browser's right strip; it shows four palettes under the track list: the TAG LIST ([`View::tags`]) and three
//! that each show one of the user's playlists (My Playlists) with its tracks (ui/browser-palette.slint).
//!
//! - **Assign**: drag a playlist from the tree onto a palette (overwrites), or the ▾ of the palette's name bar lists the
//!   playlists to pick from (the drag-free path). The assignment is kept in the store ([`assign::Assign`]).
//! - **Add a track**: drag it from the track list or another palette onto a palette, or "Add to palette n" in the track menu.
//!   A track dropped on a blank palette makes "Untitled Playlist" in My Playlists and opens its name for typing. A palette's
//!   playlist is the stored one, so what is added here is what ADD TO PLAYLIST would add.
//! - **Name bar**: a click chooses the playlist (or the Tag List) in the tree; a double click renames it.
//! - **Tracks**: a click chooses the track (track panel), a double click or the menu loads it onto a deck, the menu removes it.
//!
//! The palettes mirror the store: a timer compares what each shows with the store ~3 times a second while the palette is
//! open, so ADD TO PLAYLIST, a rename, a delete or the deck's Tag List removal show up without a hook in every writer.
//!
//! Children: [`assign`] (which playlist each palette shows, kept in the store), [`drag`] (what a drag carries), [`edit`] (what a
//! drop, the name bar and the chooser do to the store), [`target`] (the other drop targets, the decks and the tree rows, and the
//! drag picture: `ui/dnd.slint`).

mod assign;
mod drag;
mod edit;
mod target;

use super::{
    table::{cell, BPM},
    wire::load_to_deck,
    View,
};
use crate::app::{App, BrowserState, DragState, prefs, trackinfo};
use assign::{Assign, SLOTS};
use drag::Drag;
use openfader::{library::Track, store};
use slint::{
    ComponentHandle, DataTransfer, ModelRc, SharedString, StandardListViewItem, Timer, TimerMode,
    VecModel,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

/// The Tag List holds up to 100 tracks.
const TAG_LIST_MAX: usize = 100;
/// Palettes on screen: the TAG LIST and one per assignable slot.
const PALETTES: usize = SLOTS + 1;
const BLANK: &str = "[blank list]";
const UNTITLED: &str = "Untitled Playlist";

/// The title, then the artist after an em dash (the title alone for a track without artist).
fn track_label(title: &str, artist: &str) -> String {
    if artist.is_empty() {
        title.to_owned()
    } else {
        format!("{title} — {artist}")
    }
}

/// One palette's line for a track: its label, then the BPM after a middle dot when it has one.
fn line(t: &Track) -> SharedString {
    let (bpm, mut s) = (cell(t, BPM), track_label(&t.title, &t.artist));
    if !bpm.is_empty() {
        s += &format!("  ·  {bpm}");
    }
    s.into()
}

struct Palette {
    view: Rc<RefCell<View>>,
    assign: RefCell<Assign>,
    names: Rc<VecModel<SharedString>>,
    tracks: Rc<VecModel<ModelRc<SharedString>>>,
    choices: Rc<VecModel<StandardListViewItem>>,
    /// Playlist (node id) of each row of `choices`; row 0 is the blank list.
    choice_ids: RefCell<Vec<Option<i64>>>,
    /// What each palette lists now, in row order; the rows of a drag or click are looked up here.
    shown: RefCell<Vec<Vec<Track>>>,
    /// Palette names, track keys, and (store generation, BPM precision, assignments, reads succeeded).
    seen: RefCell<(Vec<String>, Vec<Vec<String>>, (u64, usize, Assign, bool))>,
    on_track: Rc<dyn Fn(&Track)>,
    _timer: Timer,
    #[cfg(test)]
    dispatches: std::cell::Cell<u64>,
}

pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>, on_track: Rc<dyn Fn(&Track)>) {
    let state = app.global::<BrowserState>();
    let store = view.borrow().store.clone();
    let p = Rc::new(Palette {
        view: view.clone(),
        assign: RefCell::new(Assign::load(&store)),
        names: Rc::default(),
        tracks: Rc::default(),
        choices: Rc::default(),
        choice_ids: RefCell::default(),
        shown: RefCell::new(vec![Vec::new(); PALETTES]),
        seen: RefCell::default(),
        on_track,
        _timer: Timer::default(),
        #[cfg(test)]
        dispatches: std::cell::Cell::new(0),
    });
    state.set_palette_names(p.names.clone().into());
    state.set_palette_tracks(p.tracks.clone().into());
    state.set_palette_choices(p.choices.clone().into());

    state.on_drag_track(|r| DataTransfer::from(Drag::Track(r as usize).text()));
    state.on_drag_tree(|r| DataTransfer::from(Drag::Tree(r as usize).text()));
    state.on_drag_palette(|pal, r| {
        DataTransfer::from(Drag::Palette(pal as usize, r as usize).text())
    });

    let ds = app.global::<DragState>();
    let q = p.clone();
    ds.on_accepts_deck(move |_, d| d.plain_text().is_ok_and(|t| Palette::accepts_deck(&t)));
    let q2 = q.clone();
    ds.on_accepts_tree(move |row, d| {
        usize::try_from(row)
            .ok()
            .zip(d.plain_text().ok())
            .is_some_and(|(row, t)| q2.accepts_tree(row, &t))
    });
    let q3 = q.clone();
    ds.on_label_of(move |d| q3.label(&d.plain_text().unwrap_or_default()).into());
    let (weak, q4) = (app.as_weak(), q.clone());
    ds.on_deck_drop(move |deck, d| {
        if let (Some(app), Ok(deck), Ok(t)) =
            (weak.upgrade(), usize::try_from(deck), d.plain_text())
        {
            q4.deck_dropped(&app, deck, &t);
        }
    });
    let weak = app.as_weak();
    ds.on_tree_drop(move |row, d| {
        if let (Some(app), Ok(row), Ok(t)) = (weak.upgrade(), usize::try_from(row), d.plain_text())
        {
            q.tree_dropped(&app, row, &t);
        }
    });

    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_drop(move |pal, data| {
        let (Some(app), Ok(text), Ok(pal)) =
            (weak.upgrade(), data.plain_text(), usize::try_from(pal))
        else {
            return;
        };
        q.dropped(&app, pal, &text);
    });
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_add_chosen(move |pal| {
        let (Some(app), Ok(pal)) = (weak.upgrade(), usize::try_from(pal)) else {
            return;
        };
        let mut v = q.view.borrow_mut();
        match v.selection() {
            tracks if !tracks.is_empty() => q.add_tracks(&app, &mut v, pal, &tracks),
            _ => v.say(&app, tr!("select a track to add")),
        }
    });
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_header(move |pal| {
        if let (Some(app), Ok(pal)) = (weak.upgrade(), usize::try_from(pal)) {
            q.header(&app, pal);
        }
    });
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_rename(move |pal, name| {
        if let (Some(app), Ok(pal)) = (weak.upgrade(), usize::try_from(pal)) {
            q.rename(&app, pal, name.trim());
        }
    });
    let q = p.clone();
    state.on_palette_menu(move |_| q.fill_choices());
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_choose(move |pal, row| {
        if let (Some(app), Ok(pal), Ok(row)) =
            (weak.upgrade(), usize::try_from(pal), usize::try_from(row))
        {
            q.choose(&app, pal, row);
        }
    });
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_track_selected(move |pal, row| {
        let (Some(app), Some(t)) = (weak.upgrade(), q.track(pal, row)) else {
            return;
        };
        q.view.borrow_mut().choose_elsewhere(&app, &t);
        trackinfo::show(&app, &t);
        (q.on_track)(&t);
    });
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_track_load(move |pal, row| {
        if let (Some(app), Some(t)) = (weak.upgrade(), q.track(pal, row)) {
            q.view.borrow_mut().choose_elsewhere(&app, &t);
            load_to_deck(&app, &*q.on_track, &t);
        }
    });
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_track_remove(move |pal, row| {
        if let (Some(app), Ok(pal), Ok(row)) =
            (weak.upgrade(), usize::try_from(pal), usize::try_from(row))
        {
            q.remove(&app, pal, row);
        }
    });

    p.follow(app);
}

impl Palette {
    fn update_timer(&self, app: &App) {
        let state = app.global::<BrowserState>();
        if state.get_palette_enabled() && state.get_palette_open() {
            self.sync_if_free();
            self._timer.restart();
        } else {
            self._timer.stop();
        }
    }

    fn follow(self: &Rc<Self>, app: &App) {
        let (weak, q) = (app.as_weak(), Rc::downgrade(self));
        self._timer
            .start(TimerMode::Repeated, Duration::from_millis(300), move || {
                if let (Some(app), Some(q)) = (weak.upgrade(), q.upgrade()) {
                    #[cfg(test)]
                    q.dispatches.set(q.dispatches.get() + 1);
                    if app.global::<BrowserState>().get_palette_open() {
                        q.sync_if_free();
                    }
                }
            });
        let (weak, q) = (app.as_weak(), self.clone());
        app.global::<BrowserState>()
            .on_palette_toggled(move |open| {
                let Some(app) = weak.upgrade() else { return };
                app.global::<BrowserState>().set_palette_open(open);
                q.update_timer(&app);
            });
        let (weak, q) = (app.as_weak(), self.clone());
        prefs::on_change(move |pr| {
            let Some(app) = weak.upgrade() else { return };
            let state = app.global::<BrowserState>();
            if state.get_palette_enabled() != pr.view.panel_palette {
                state.set_palette_enabled(pr.view.panel_palette);
                q.update_timer(&app);
            }
        });
        app.global::<BrowserState>()
            .set_palette_enabled(prefs::with(|p| p.view.panel_palette));
        self.update_timer(app);
    }

    fn track(&self, pal: i32, row: i32) -> Option<Track> {
        self.shown
            .borrow()
            .get(usize::try_from(pal).ok()?)?
            .get(usize::try_from(row).ok()?)
            .cloned()
    }

    /// [`Self::sync`] unless the view is busy (a callback of it is running): the next tick catches up.
    fn sync_if_free(&self) {
        if let Ok(mut v) = self.view.try_borrow_mut() {
            self.sync(&mut v);
        }
    }

    /// Makes the palettes show what the store and the Tag List hold now.
    fn sync(&self, v: &mut View) {
        let store = v.store.clone();
        let nodes = &v.nodes;
        let (names, keys, revision) = {
            let mut a = self.assign.borrow_mut();
            a.prune(&store, |id| nodes.iter().any(|n| n.id == id));
            let mut revision = (
                store.generation(),
                prefs::with(|p| p.view.tempo_decimals.digits()),
                *a,
                true,
            );
            let mut names = vec![tr!("TAG LIST").to_owned()];
            let mut keys = vec![v.tags.iter().map(store::key).collect::<Vec<_>>()];
            for slot in 0..SLOTS {
                let id = a.get(slot);
                names.push(
                    id.and_then(|id| nodes.iter().find(|n| n.id == id))
                        .map_or_else(|| BLANK.to_owned(), |n| n.name.clone()),
                );
            }
            {
                let seen = self.seen.borrow();
                if seen.2 == revision && seen.0 == names && seen.1.first() == keys.first() {
                    return;
                }
            }
            for slot in 0..SLOTS {
                keys.push(match a.get(slot).map(|id| store.entries(id)).transpose() {
                    Ok(keys) => keys.unwrap_or_default(),
                    Err(_) => {
                        revision.3 = false; // Failed reads must be retried on the next poll.
                        Vec::new()
                    }
                });
            }
            (names, keys, revision)
        };
        if self.seen.borrow().0 == names
            && self.seen.borrow().1 == keys
            && self.seen.borrow().2 == revision
        {
            return;
        }
        let mut lists = vec![v.tags.clone()];
        lists.extend(
            keys[1..]
                .iter()
                .map(|k| v.tracks_of(k, false).unwrap_or_default()),
        );
        self.names
            .set_vec(names.iter().map(|n| n.as_str().into()).collect::<Vec<_>>());
        self.tracks.set_vec(
            lists
                .iter()
                .map(|l| ModelRc::new(VecModel::from(l.iter().map(line).collect::<Vec<_>>())))
                .collect::<Vec<_>>(),
        );
        *self.shown.borrow_mut() = lists;
        *self.seen.borrow_mut() = (names, keys, revision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;
    fn rig() -> (App, Rc<Palette>) {
        crate::app::headless::init();
        prefs::set_for_test(|p| p.view.panel_palette = true);
        let app = App::new().unwrap();
        let view = crate::app::browser::timer_test_view(&app);
        let p = Rc::new(Palette {
            assign: RefCell::new(Assign::load(&view.borrow().store)),
            view: view.clone(),
            names: Rc::default(),
            tracks: Rc::default(),
            choices: Rc::default(),
            choice_ids: RefCell::default(),
            shown: RefCell::new(vec![Vec::new(); PALETTES]),
            seen: RefCell::default(),
            on_track: Rc::new(|_| {}),
            _timer: Timer::default(),
            dispatches: std::cell::Cell::new(0),
        });
        p.follow(&app);
        (app, p)
    }

    #[test]
    fn palette_refresh_tracks_assignment_membership_and_late_tree_names() {
        let (_app, p) = rig();
        let mut v = p.view.borrow_mut();
        let store = v.store.clone();
        let input = |name: &str| Track {
            title: name.into(),
            file: Some(format!("/palette/{name}.wav").into()),
            ..Default::default()
        };
        store
            .collection_put(&[input("A"), input("B")], false)
            .unwrap();
        let tracks = v.lib.tracks(None).unwrap();
        let a = tracks.iter().find(|t| t.title == "A").unwrap().clone();
        let b = tracks.iter().find(|t| t.title == "B").unwrap().clone();
        let first = store
            .add_node(None, "Same", store::NodeKind::Playlist)
            .unwrap();
        let second = store
            .add_node(None, "Same", store::NodeKind::Playlist)
            .unwrap();
        store.set_entries(first, &[store::key(&a)]).unwrap();
        store.set_entries(second, &[store::key(&b)]).unwrap();
        v.nodes = store.nodes().unwrap();
        p.assign.borrow_mut().set(&store, 0, Some(first)).unwrap();
        p.sync(&mut v);
        assert_eq!(p.track(1, 0).unwrap().title, "A");
        p.assign.borrow_mut().set(&store, 0, Some(second)).unwrap();
        p.sync(&mut v);
        assert_eq!(p.track(1, 0).unwrap().title, "B");
        store.set_entries(second, &[store::key(&a)]).unwrap();
        p.sync(&mut v);
        assert_eq!(p.track(1, 0).unwrap().title, "A");
        store.rename_node(second, "Renamed").unwrap();
        p.sync(&mut v); // The browser's tree snapshot has not caught up yet.
        v.nodes = store.nodes().unwrap();
        p.sync(&mut v);
        assert_eq!(p.names.row_data(1).unwrap(), "Renamed");
        v.tags = vec![a];
        p.sync(&mut v);
        v.tags = vec![b];
        p.sync(&mut v);
        assert_eq!(p.track(0, 0).unwrap().title, "B");
        store.delete_node(second).unwrap();
        v.nodes = store.nodes().unwrap();
        p.sync(&mut v);
        assert!(p.track(1, 0).is_none());
        assert_eq!(p.names.row_data(1).unwrap(), BLANK);
    }

    #[test]
    fn palette_timer_tracks_open_and_preferences_and_refreshes_immediately() {
        use crate::app::browser::timer_test_pump;
        let (app, p) = rig();
        let state = app.global::<BrowserState>();
        assert!(!p._timer.running());
        state.invoke_palette_toggled(true);
        assert!(p._timer.running());
        assert_eq!(p.names.row_count(), PALETTES);
        state.invoke_palette_toggled(false);
        assert!(!p._timer.running());
        let before = p.dispatches.get();
        timer_test_pump(Duration::from_millis(350));
        assert_eq!(p.dispatches.get(), before);
        let t = Track {
            title: "added while closed".into(),
            ..Default::default()
        };
        p.view.borrow_mut().tags.push(t.clone());
        state.invoke_palette_toggled(true);
        assert_eq!(
            p.shown.borrow()[0],
            [t.clone()],
            "reopening refreshes before the next tick"
        );
        prefs::set_for_test(|pr| pr.view.panel_palette = false);
        assert!(!p._timer.running());
        assert!(
            state.get_palette_open(),
            "the open choice survives a hidden preference"
        );
        p.view.borrow_mut().tags.clear();
        prefs::set_for_test(|pr| pr.view.panel_palette = true);
        assert!(p._timer.running());
        assert!(
            p.shown.borrow()[0].is_empty(),
            "re-enabled palettes refresh immediately"
        );
        p.view.borrow_mut().tags.push(t.clone());
        timer_test_pump(Duration::from_millis(350));
        assert_eq!(
            p.shown.borrow()[0],
            [t],
            "open palettes still follow background store/Tag List changes"
        );
    }

    #[test]
    #[ignore = "16 s real Slint timer dispatch comparison; --release --ignored --nocapture"]
    fn bench_palette_timer_dispatch() {
        use crate::app::browser::timer_test_pump;
        let (app, p) = rig();
        let state = app.global::<BrowserState>();
        let duration = Duration::from_secs(2);
        for unconditional in [true, false] {
            for (phase, open) in [
                ("closed", false),
                ("open", true),
                ("closed-again", false),
                ("reopened", true),
            ] {
                state.invoke_palette_toggled(open);
                if unconditional {
                    p._timer.restart(); // former scheduling, same production callback
                }
                let before = p.dispatches.get();
                timer_test_pump(duration);
                eprintln!(
                    "palette unconditional={unconditional} phase={phase} duration_ms={} dispatches={}",
                    duration.as_millis(),
                    p.dispatches.get() - before
                );
            }
        }
    }

    #[test]
    fn a_line_shows_title_artist_and_bpm() {
        let t = Track {
            title: "Song".into(),
            artist: "Band".into(),
            bpm_x100: 12800,
            ..Default::default()
        };
        assert_eq!(line(&t), "Song — Band  ·  128.00");
        let bare = Track {
            title: "Song".into(),
            ..Default::default()
        };
        assert_eq!(line(&bare), "Song");
    }

    #[test]
    fn the_label_names_title_and_artist() {
        assert_eq!(track_label("Intro", ""), "Intro");
        assert_eq!(
            track_label("Bulletproof", "Club Angel"),
            "Bulletproof — Club Angel"
        );
    }
}
