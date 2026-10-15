//! What the palettes do to the store and the browser: drops, adding and removing tracks, the name bar and the playlist chooser.

use super::{BLANK, Palette, TAG_LIST_MAX, UNTITLED, assign::SLOTS, drag::Drag};
use crate::app::{
    App, BrowserState,
    browser::{Home, View, playlists::playlist_labels, sidebar::Entry},
};
use openfader::{
    library::Track,
    store::{self, NodeKind},
};
use slint::{ComponentHandle, StandardListViewItem, Timer};
use std::time::Duration;

impl Palette {
    /// The user's playlist (node id, plain playlists only) a sidebar entry stands for.
    pub(super) fn playlist_of(v: &View, e: &Entry) -> Option<i64> {
        match e {
            Entry::User(id) => v
                .nodes
                .iter()
                .find(|n| n.id == *id && n.kind == NodeKind::Playlist)
                .map(|n| n.id),
            _ => None,
        }
    }

    /// A drag ended on palette `pal`.
    pub(super) fn dropped(&self, app: &App, pal: usize, text: &str) {
        let Some(drag) = Drag::parse(text) else {
            return;
        };
        let mut v = self.view.borrow_mut();
        match drag {
            Drag::Palette(from, _) if from == pal => {
                v.say(app, "the track is already in this list")
            }
            Drag::Tree(row) => {
                let Some(id) = v.entries.get(row).and_then(|e| Self::playlist_of(&v, e)) else {
                    return v.say(app, "only a playlist of My Playlists can go on a palette");
                };
                self.put_playlist(app, &mut v, pal, id);
            }
            Drag::Track(_) | Drag::Palette(..) => match self.track_of(&v, &drag) {
                Some(t) => self.add_track(app, &mut v, pal, &t),
                None => v.say(app, "that track is gone"),
            },
        }
    }

    /// A playlist dropped on a palette: a playlist palette shows it (overwriting), the TAG LIST takes its tracks.
    fn put_playlist(&self, app: &App, v: &mut View, pal: usize, id: i64) {
        if pal == 0 {
            let keys = v.store.entries(id).unwrap_or_default();
            let tracks = v.tracks_of(&keys, false).unwrap_or_default();
            let before = v.tags.len();
            for t in tracks {
                if v.tags.len() < TAG_LIST_MAX && !v.tags.contains(&t) {
                    v.tags.push(t);
                }
            }
            if v.tags.len() != before {
                v.tags_changed(app);
            }
            v.say(
                app,
                format!("{} tracks added to the Tag List", v.tags.len() - before),
            );
        } else {
            let store = v.store.clone();
            let r = self.assign.borrow_mut().set(&store, pal - 1, Some(id));
            if let Err(e) = r {
                return v.say(app, format!("store: {e}"));
            }
        }
        self.sync(v);
    }

    /// `t` goes to the end of palette `pal`'s list; a blank palette first makes "Untitled Playlist" and asks for its name.
    pub(super) fn add_track(&self, app: &App, v: &mut View, pal: usize, t: &Track) {
        if pal == 0 {
            let msg = if v.tags.contains(t) {
                format!("“{}” is already in the Tag List", t.title)
            } else if v.tags.len() >= TAG_LIST_MAX {
                format!("the Tag List holds up to {TAG_LIST_MAX} tracks")
            } else {
                v.tags.push(t.clone());
                v.tags_changed(app);
                format!("tagged “{}” ({} in the Tag List)", t.title, v.tags.len())
            };
            v.say(app, msg);
            return self.sync(v);
        }
        let Some(slot) = pal.checked_sub(1).filter(|&s| s < SLOTS) else {
            return;
        };
        let store = v.store.clone();
        let mut fresh = false;
        let id = match self.assign.borrow().get(slot) {
            Some(id) => id,
            None => match store.add_node(None, UNTITLED, NodeKind::Playlist) {
                Ok(id) => {
                    fresh = true;
                    id
                }
                Err(e) => return v.say(app, format!("store: {e}")),
            },
        };
        let key = store::key(t);
        let name = v
            .nodes
            .iter()
            .find(|n| n.id == id)
            .map_or_else(|| UNTITLED.to_owned(), |n| n.name.clone());
        let msg = match store.entries(id) {
            Ok(have) if have.contains(&key) => format!("“{}” is already in {name}", t.title),
            Ok(_) => match store.add_entries(id, &[key]) {
                Ok(()) => format!("added “{}” to {name}", t.title),
                Err(e) => format!("store: {e}"),
            },
            Err(e) => format!("store: {e}"),
        };
        if fresh {
            if let Err(e) = self.assign.borrow_mut().set(&store, slot, Some(id)) {
                return v.say(app, format!("store: {e}"));
            }
            v.rebuild(app); // the tree lists the new playlist
            app.global::<BrowserState>().set_palette_edit(pal as i32);
        }
        if matches!(v.home, Home::User(h) if h == id) && v.related.is_none() {
            v.load(app);
        }
        v.say(app, msg);
        self.sync(v);
    }

    /// The name bar was clicked: the palette's playlist (the Tag List for palette 0) is chosen in the tree.
    pub(super) fn header(&self, app: &App, pal: usize) {
        let v = self.view.borrow();
        let entry = match pal.checked_sub(1) {
            None => Some(Entry::Tags),
            Some(slot) => self.assign.borrow().get(slot).map(Entry::User),
        };
        let Some(row) = entry.and_then(|e| v.entries.iter().position(|x| *x == e)) else {
            return;
        };
        // The tree takes a row from `side-row` only when it changes: park it off the list first.
        let state = app.global::<BrowserState>();
        state.set_side_row(-1);
        let weak = app.as_weak();
        Timer::single_shot(Duration::from_millis(20), move || {
            if let Some(app) = weak.upgrade() {
                app.global::<BrowserState>().set_side_row(row as i32);
            }
        });
    }

    pub(super) fn rename(&self, app: &App, pal: usize, name: &str) {
        let mut v = self.view.borrow_mut();
        let Some(id) = pal.checked_sub(1).and_then(|s| self.assign.borrow().get(s)) else {
            return;
        };
        if name.is_empty() {
            return v.say(app, "a name is needed");
        }
        match v.store.rename_node(id, name) {
            Ok(()) => v.rebuild(app),
            Err(e) => v.say(app, format!("store: {e}")),
        }
        self.sync(&mut v);
    }

    /// ▾ pressed: lists the playlists the palette can show (a blank list first).
    pub(super) fn fill_choices(&self) {
        let v = self.view.borrow();
        let (labels, ids) = playlist_labels(&v.nodes);
        let (mut rows, mut all) = (vec![BLANK.to_owned()], vec![None]);
        rows.extend(labels);
        all.extend(ids);
        self.choices.set_vec(
            rows.iter()
                .map(|r| StandardListViewItem::from(r.as_str()))
                .collect::<Vec<_>>(),
        );
        *self.choice_ids.borrow_mut() = all;
    }

    pub(super) fn choose(&self, app: &App, pal: usize, row: usize) {
        let mut v = self.view.borrow_mut();
        let Some(slot) = pal.checked_sub(1).filter(|&s| s < SLOTS) else {
            return;
        };
        let Some(id) = self.choice_ids.borrow().get(row).copied() else {
            return;
        };
        // a folder heading of the list (no node id) is not a playlist; row 0 empties the palette
        if id.is_none() && row != 0 {
            return;
        }
        let store = v.store.clone();
        if let Err(e) = self.assign.borrow_mut().set(&store, slot, id) {
            return v.say(app, format!("store: {e}"));
        }
        self.sync(&mut v);
    }

    /// "Remove from list": the track leaves the Tag List / the palette's playlist.
    pub(super) fn remove(&self, app: &App, pal: usize, row: usize) {
        let mut v = self.view.borrow_mut();
        let Some(t) = self
            .shown
            .borrow()
            .get(pal)
            .and_then(|l| l.get(row))
            .cloned()
        else {
            return;
        };
        match pal.checked_sub(1) {
            None => {
                if let Some(at) = v.tags.iter().position(|x| *x == t) {
                    v.tags.remove(at);
                    v.tags_changed(app);
                }
            }
            Some(slot) => {
                let Some(id) = self.assign.borrow().get(slot) else {
                    return;
                };
                let key = store::key(&t);
                let mut keys = v.store.entries(id).unwrap_or_default();
                if let Some(at) = keys.iter().position(|k| *k == key) {
                    keys.remove(at);
                    if let Err(e) = v.store.set_entries(id, &keys) {
                        return v.say(app, format!("store: {e}"));
                    }
                    if matches!(v.home, Home::User(h) if h == id) && v.related.is_none() {
                        v.load(app);
                    }
                }
            }
        }
        self.sync(&mut v);
    }
}
