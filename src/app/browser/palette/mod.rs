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
    View,
    table::{BPM, cell},
    wire::load_to_deck,
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
    /// Palette names, track keys, store generation and BPM display precision used for `shown`.
    seen: RefCell<(Vec<String>, Vec<Vec<String>>, (u64, usize))>,
    on_track: Rc<dyn Fn(&Track)>,
    _timer: Timer,
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
    });
    state.set_palette_names(p.names.clone().into());
    state.set_palette_tracks(p.tracks.clone().into());
    state.set_palette_choices(p.choices.clone().into());
    state.set_palette_enabled(prefs::with(|p| p.view.panel_palette));
    let weak = app.as_weak();
    prefs::on_change(move |pr| {
        if let Some(app) = weak.upgrade() {
            app.global::<BrowserState>()
                .set_palette_enabled(pr.view.panel_palette);
        }
    });

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
        match v.selected.clone() {
            Some(t) => q.add_track(&app, &mut v, pal, &t),
            None => v.say(&app, tr!("select a track to add")),
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
        q.view.borrow_mut().selected = Some(t.clone());
        trackinfo::show(&app, &t);
        (q.on_track)(&t);
    });
    let (weak, q) = (app.as_weak(), p.clone());
    state.on_palette_track_load(move |pal, row| {
        if let (Some(app), Some(t)) = (weak.upgrade(), q.track(pal, row)) {
            q.view.borrow_mut().selected = Some(t.clone());
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

    let (weak, q) = (app.as_weak(), Rc::downgrade(&p));
    p._timer
        .start(TimerMode::Repeated, Duration::from_millis(300), move || {
            let (Some(app), Some(q)) = (weak.upgrade(), q.upgrade()) else {
                return;
            };
            if app.global::<BrowserState>().get_palette_open() {
                q.sync_if_free();
            }
        });
}

impl Palette {
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
        let (names, keys) = {
            let mut a = self.assign.borrow_mut();
            a.prune(&store, |id| nodes.iter().any(|n| n.id == id));
            let mut names = vec![tr!("TAG LIST").to_owned()];
            let mut keys = vec![v.tags.iter().map(store::key).collect::<Vec<_>>()];
            for slot in 0..SLOTS {
                let id = a.get(slot);
                names.push(
                    id.and_then(|id| nodes.iter().find(|n| n.id == id))
                        .map_or_else(|| BLANK.to_owned(), |n| n.name.clone()),
                );
                keys.push(id.map_or_else(Vec::new, |id| store.entries(id).unwrap_or_default()));
            }
            (names, keys)
        };
        let revision = (
            store.generation(),
            prefs::with(|p| p.view.tempo_decimals.digits()),
        );
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
