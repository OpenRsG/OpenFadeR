//! The drop targets besides the palettes: the decks (a dragged track is loaded there) and the tree rows (a track dropped on a
//! playlist is added to it, on the Tag List it is tagged, on a tag it gets the tag), plus the text under the cursor.
//! The slint side is `ui/dnd.slint` (`DragState`).

use super::{Palette, drag::Drag, track_label};
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

    /// A track drag a deck takes.
    pub(super) fn accepts_deck(text: &str) -> bool {
        matches!(Drag::parse(text), Some(Drag::Track(_) | Drag::Palette(..)))
    }

    /// Whether tree row `row` takes the track drag `text`: a plain playlist of My Playlists, the Tag List, a tag.
    pub(super) fn accepts_tree(&self, row: usize, text: &str) -> bool {
        if !Self::accepts_deck(text) {
            return false;
        }
        let Ok(v) = self.view.try_borrow() else {
            return false;
        };
        match v.entries.get(row) {
            Some(Entry::Tags | Entry::Tag(_)) => true,
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
                .say(app, "drop a track on a deck to load it");
        };
        self.view.borrow_mut().selected = Some(t.clone());
        trackinfo::show(app, &t);
        (self.on_track)(&t);
        app.global::<DeckState>().invoke_load(deck as i32);
    }

    /// A drag ended on tree row `row`.
    pub(super) fn tree_dropped(&self, app: &App, row: usize, text: &str) {
        let Some(drag) = Drag::parse(text) else {
            return;
        };
        let mut v = self.view.borrow_mut();
        let Some(t) = self.track_of(&v, &drag) else {
            return v.say(app, "drop a track on a playlist, the Tag List or a tag");
        };
        match v.entries.get(row).cloned() {
            Some(Entry::Tags) => self.add_track(app, &mut v, 0, &t),
            Some(Entry::Tag(id)) => v.tag_track(app, &t, id),
            Some(e) => match Self::playlist_of(&v, &e) {
                Some(id) => v.add_track_to(app, id, &t),
                None => v.say(app, "drop a track on a playlist, the Tag List or a tag"),
            },
            None => {}
        }
    }

    /// The text under the cursor while `text` is dragged: the track's title and artist, or the tree row's label.
    pub(super) fn label(&self, text: &str) -> String {
        let Ok(v) = self.view.try_borrow() else {
            return String::new();
        };
        match Drag::parse(text) {
            Some(Drag::Tree(row)) => v
                .sidebar
                .row_data(row)
                .map_or_else(String::new, |r| r.text.trim().to_owned()),
            Some(d) => self
                .track_of(&v, &d)
                .map_or_else(String::new, |t| track_label(&t.title, &t.artist)),
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
