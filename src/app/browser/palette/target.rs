//! The drop targets besides the palettes: the decks (a dragged track is loaded there) and the tree rows (a track dropped on a
//! playlist is added to it, on the Tag List it is tagged, on a tag it gets the tag), plus the text under the cursor.
//! The slint side is `ui/dnd.slint` (`DragState`).

use super::{drag::Drag, track_label, Palette};
use crate::app::device;
use crate::app::{
    App, DeckState,
    browser::{View, sidebar::Entry},
    trackinfo,
};
use openfader::library::Track;
use slint::{ComponentHandle, Model};

impl Palette {
    /// The track a drag carries; `None` for a playlist drag or a row that is gone.
    pub(super) fn track_of(&self, v: &View, d: &Drag) -> Option<Track> {
        match *d {
            Drag::Track(row) => v.shown.get(row).and_then(|&k| v.list.get(k)).cloned(),
            Drag::Palette(p, row) => self.shown.borrow().get(p)?.get(row).cloned(),
            Drag::Tree(_) => None,
        }
    }

    /// The tracks a drag carries: the whole selection for a drag from one of several selected rows, else [`Self::track_of`].
    pub(super) fn tracks_of(&self, v: &View, d: &Drag) -> Vec<Track> {
        match *d {
            Drag::Track(row) => v
                .sel
                .carried(&v.shown, row)
                .into_iter()
                .filter_map(|k| v.list.get(k).cloned())
                .collect(),
            _ => self.track_of(v, d).into_iter().collect(),
        }
    }

    /// A track drag a deck takes.
    pub(super) fn accepts_deck(text: &str) -> bool {
        matches!(Drag::parse(text), Some(Drag::Track(_) | Drag::Palette(..)))
    }

    /// The export snapshot id of the playlist or folder a tree row stands for (My Playlists, the opened library); `None` for any
    /// other row.
    fn snapshot_id(v: &View, e: &Entry) -> Option<String> {
        match e {
            Entry::User(id) => Some(format!("store:{id}")),
            Entry::Lib(i) => v.lib_playlists.get(*i).map(|p| p.id.clone()),
            _ => None,
        }
    }

    /// Whether tree row `row` takes the drag `text`: a device row takes tracks, and a playlist or folder on the device itself
    /// (it is transferred); a plain playlist of My Playlists, the Tag List, a tag and a stage take tracks.
    pub(super) fn accepts_tree(&self, row: usize, text: &str) -> bool {
        let Ok(v) = self.view.try_borrow() else {
            return false;
        };
        if let Some(to) = v.entries.get(row).and_then(|e| v.device_target(e)) {
            return match Drag::parse(text) {
                Some(Drag::Tree(from)) => {
                    to.list.is_none()
                        && v.entries
                            .get(from)
                            .and_then(|e| Self::snapshot_id(&v, e))
                            .is_some()
                }
                Some(_) => true,
                None => false,
            };
        }
        if !Self::accepts_deck(text) {
            return false;
        }
        match v.entries.get(row) {
            Some(Entry::Tags | Entry::Tag(_)) => true,
            Some(Entry::Stage(_)) => true,
            Some(e) => Self::playlist_of(&v, e).is_some(),
            None => false,
        }
    }

    /// A drag ended on deck `deck`: the track becomes the browser's chosen one and loads there (the deck's own Eject/Load Lock
    /// applies, as for LOAD).
    pub(super) fn deck_dropped(&self, app: &App, deck: usize, text: &str) {
        let Some(drag) = Drag::parse(text) else {
            return;
        };
        let Some(t) = self.track_of(&self.view.borrow(), &drag) else {
            return self
                .view
                .borrow()
                .say(app, tr!("drop a track on a deck to load it"));
        };
        self.view.borrow_mut().choose_elsewhere(app, &t);
        trackinfo::show(app, &t);
        (self.on_track)(&t);
        app.global::<DeckState>().invoke_load(deck as i32);
    }

    /// A drag ended on tree row `row`.
    pub(super) fn tree_dropped(&self, app: &App, row: usize, text: &str) {
        let Some(drag) = Drag::parse(text) else {
            return;
        };
        if self.device_dropped(app, row, &drag) {
            return;
        }
        let mut v = self.view.borrow_mut();
        let tracks = self.tracks_of(&v, &drag);
        if tracks.is_empty() {
            return v.say(
                app,
                tr!("drop a track on a playlist, the Tag List or a tag"),
            );
        }
        match v.entries.get(row).cloned() {
            Some(Entry::Tags) => self.add_tracks(app, &mut v, 0, &tracks),
            Some(Entry::Tag(id)) => v.tag_tracks(app, &tracks, id),
            Some(Entry::Stage(id)) => v.move_to_stage(app, &tracks, id),
            Some(e) => match Self::playlist_of(&v, &e) {
                Some(id) => v.add_tracks_to(app, id, &tracks),
                None => v.say(
                    app,
                    tr!("drop a track on a playlist, the Tag List or a tag"),
                ),
            },
            None => {}
        }
    }

    /// A drag ended on a device row (the device itself or one of its playlists): the device module transfers it. `false`: `row` is
    /// no device row.
    fn device_dropped(&self, app: &App, row: usize, drag: &Drag) -> bool {
        let v = self.view.borrow();
        let Some(to) = v.entries.get(row).and_then(|e| v.device_target(e)) else {
            return false;
        };
        let playlist = match drag {
            Drag::Tree(from) => v
                .entries
                .get(*from)
                .and_then(|e| Self::snapshot_id(&v, e))
                .filter(|_| to.list.is_none()),
            _ => None,
        };
        let tracks = self.tracks_of(&v, drag);
        if playlist.is_none() && tracks.is_empty() {
            v.say(
                app,
                tr!("drop tracks on a device or its playlists, or a playlist on the device"),
            );
            return true;
        }
        drop(v); // the transfer reads the browser's selection
        match playlist {
            Some(id) => device::drop_playlist(to, id),
            None => device::drop_tracks(to, tracks),
        }
        true
    }

    /// The text under the cursor while `text` is dragged: the track's title and artist, "{n} tracks" for a selection, or the tree row's label.
    pub(super) fn label(&self, text: &str) -> String {
        let Ok(v) = self.view.try_borrow() else {
            return String::new();
        };
        match Drag::parse(text) {
            Some(Drag::Tree(row)) => v
                .sidebar
                .row_data(row)
                .map_or_else(String::new, |r| r.text.trim().to_owned()),
            Some(d) => match self.tracks_of(&v, &d).as_slice() {
                [] => String::new(),
                [t] => track_label(&t.title, &t.artist),
                many => tr!("{} tracks", many.len()),
            },
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decks_take_track_drags_and_nothing_else() {
        assert!(Palette::accepts_deck("track:3"));
        assert!(Palette::accepts_deck("list:1:0"));
        assert!(!Palette::accepts_deck("tree:3"));
        assert!(!Palette::accepts_deck("file:///a.mp3"));
    }
}
