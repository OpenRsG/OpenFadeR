//! The sidebar's management bar: name bar, + NEW, rename, delete, DUPLICATE TO MY PLAYLISTS, MORE and ADD TO PLAYLIST.

use super::{Act, Ask, Panel, is_named, more_rows, playlist_labels};
use crate::app::browser::{
    App, BrowserState, Home, View,
    sidebar::{self, Entry},
    smart::Smart,
    tagging,
};
use openfader::{
    library::Track,
    store::{self, NodeKind},
};
use slint::ComponentHandle;

impl View {
    /// The Playlist menu's tree operations ([`BrowserState::playlist_op`]): "new", "new-folder", "new-smart", "delete",
    /// "renumber", "sort-<column>".
    pub(super) fn playlist_op(&mut self, app: &App, op: &str) {
        match op {
            "new" | "new-folder" | "new-smart" => {
                if self.new_parent().is_none() {
                    self.entry = Entry::UserRoot; // from anywhere else: the top level of My Playlists
                }
                self.side_new(
                    app,
                    match op {
                        "new" => 0,
                        "new-folder" => 1,
                        _ => 2,
                    },
                );
            }
            "new-from-selection" => {
                let tracks = self.selection();
                if tracks.is_empty() {
                    return self.say(app, tr!("select a track to add"));
                }
                let name = default_name(&tracks);
                self.ask(app, Ask::NewFrom(tracks), tr!("New playlist name:"), name);
            }
            "delete" => self.side_delete(app),
            "renumber" | "sort-title" | "sort-artist" | "sort-genre" | "sort-key" | "sort-bpm"
            | "sort-time" | "sort-rating" | "sort-comment" => self.reorder(app, op),
            _ => self.say(app, tr!("unknown playlist action {}", op)),
        }
    }

    /// Where a new playlist / folder goes: the chosen folder, or next to the chosen playlist.
    fn new_parent(&self) -> Option<Option<i64>> {
        match &self.entry {
            Entry::UserRoot => Some(None),
            Entry::User(id) => self.node(*id).map(|n| {
                if n.kind == NodeKind::Folder {
                    Some(n.id)
                } else {
                    n.parent
                }
            }),
            _ => None,
        }
    }

    pub(super) fn side_new(&mut self, app: &App, which: i32) {
        let kind = [NodeKind::Playlist, NodeKind::Folder, NodeKind::Smart];
        match (self.side_kind(&self.entry), which) {
            (1..=4, 0..=2) => {
                let Some(parent) = self.new_parent() else {
                    return;
                };
                let k = kind[which as usize];
                let prompt = [
                    tr!("New playlist name:"),
                    tr!("New folder name:"),
                    tr!("New smart playlist name:"),
                ][which as usize];
                self.ask(app, Ask::NewNode(parent, k), prompt, String::new());
            }
            (5..=7, 0) => self.ask(
                app,
                Ask::NewCategory,
                tr!("New tag category:"),
                String::new(),
            ),
            (13 | 14, 0) => self.ask(app, Ask::NewBank, tr!("New Cue Bank:"), String::new()),
            (15 | 16, 0) => self.ask(app, Ask::NewStage, tr!("New stage:"), String::new()),
            (6 | 7, 1) => {
                let cat = match &self.entry {
                    Entry::Category(c) => Some(*c),
                    Entry::Tag(t) => self.mytags.iter().find(|m| m.id == *t).map(|m| m.category),
                    _ => None,
                };
                if let Some(c) = cat {
                    self.ask(app, Ask::NewTag(c), tr!("New tag:"), String::new());
                }
            }
            _ => {}
        }
    }

    /// MORE pressed on a node of My Playlists.
    pub(super) fn open_more(&mut self, app: &App) {
        if matches!(self.entry, Entry::StageRoot | Entry::Stage(_)) {
            return self.open_stage_panel(app);
        }
        if matches!(self.entry, Entry::Bank(_)) {
            return self.open_bank_panel(app);
        }
        let Entry::User(id) = self.entry else { return };
        let (rows, acts): (Vec<String>, Vec<Act>) = more_rows(&self.nodes, id).into_iter().unzip();
        if rows.is_empty() {
            return self.say(
                app,
                tr!("nothing to do here: it is the only item and has no folder to move into"),
            );
        }
        self.show_panel(app, Panel::More(acts), tr!("MORE"), rows);
    }

    /// A MORE row was chosen.
    pub(super) fn more(&mut self, app: &App, act: Act) {
        let Entry::User(id) = self.entry else { return };
        let Some(me) = self.node(id).cloned() else {
            return;
        };
        let r = match act {
            Act::Edit => return self.open_editor(app, id),
            Act::Up => self.store.move_node(id, me.parent, me.pos - 1),
            Act::Down => self.store.move_node(id, me.parent, me.pos + 1),
            Act::Into(parent) => self.store.move_node(id, parent, i64::from(i32::MAX)),
        };
        if self.try_store(app, r).is_some() {
            self.rebuild(app);
            self.show(app);
        }
    }

    /// + NEW pressed: the picker lists what can be made at the chosen row.
    pub(super) fn open_new(&mut self, app: &App) {
        let (rows, ids): (Vec<&str>, Vec<Option<i64>>) = match self.side_kind(&self.entry) {
            1..=4 => vec![
                (tr!("New playlist"), 0),
                (tr!("New folder"), 1),
                (tr!("New smart playlist"), 2),
            ],
            5 => vec![(tr!("New category"), 0)],
            6 | 7 => vec![(tr!("New category"), 0), (tr!("New tag"), 1)],
            13 | 14 => vec![(tr!("New Cue Bank"), 0)],
            15 | 16 => vec![(tr!("New stage"), 0)],
            _ => return,
        }
        .into_iter()
        .map(|(r, i)| (r, Some(i)))
        .unzip();
        self.show_panel(
            app,
            Panel::New(ids),
            tr!("NEW"),
            rows.into_iter().map(str::to_owned).collect(),
        );
    }

    pub(super) fn side_rename(&mut self, app: &App) {
        let e = self.entry.clone();
        if !is_named(&e) {
            return;
        }
        let name = self.name_of(&e);
        if matches!(e, Entry::Category(_)) && tagging::is_default(&name) {
            return self.say(
                app,
                tr!("the Genre, Components and Situation categories keep their names"),
            );
        }
        self.ask(app, Ask::Rename(e), tr!("Rename to:"), name);
    }

    /// The name bar closed: `ok` = OK / Enter. Does what was asked and shows the result.
    pub(super) fn name_done(&mut self, app: &App, ok: bool) {
        let s = app.global::<BrowserState>();
        s.set_name_open(false);
        let (Some(ask), true) = (self.ask.take(), ok) else {
            return;
        };
        let name = s.get_name_text().trim().to_owned();
        if name.is_empty() {
            return self.say(app, tr!("a name is needed"));
        }
        let keys = match &ask {
            Ask::NewFrom(tracks) => self.collection_keys(tracks),
            _ => Vec::new(),
        };
        let st = self.store.clone();
        let new_smart = matches!(ask, Ask::NewNode(_, NodeKind::Smart));
        let from = matches!(ask, Ask::NewFrom(_)).then_some(keys.len());
        let made = match ask {
            Ask::NewNode(parent, kind) => st.add_node(parent, &name, kind).and_then(|id| {
                if kind == NodeKind::Smart {
                    st.set_smart(id, &Smart::default().to_text())?;
                }
                Ok(Entry::User(id))
            }),
            Ask::Rename(e @ Entry::User(id)) => st.rename_node(id, &name).map(|()| e),
            Ask::Rename(e @ Entry::Category(id)) => st.rename_mytag_category(id, &name).map(|()| e),
            Ask::Rename(e @ Entry::Tag(id)) => st.rename_mytag(id, &name).map(|()| e),
            Ask::Rename(e @ Entry::Bank(id)) => st.rename_bank(id, &name).map(|()| e),
            Ask::Rename(e @ Entry::Stage(id)) => st.rename_stage(id, &name).map(|()| e),
            Ask::Rename(_) => return,
            Ask::NewCategory => st.add_mytag_category(&name).map(Entry::Category),
            Ask::NewTag(c) => st.add_mytag(c, &name).map(Entry::Tag),
            Ask::NewBank => st.add_bank(&name).map(Entry::Bank),
            Ask::NewStage => st.add_stage(&name).map(Entry::Stage),
            Ask::NewFrom(_) => st.add_node(None, &name, NodeKind::Playlist).and_then(|id| {
                st.add_entries(id, &keys)?;
                Ok(Entry::User(id))
            }),
        };
        if let Some(e) = self.try_store(app, made) {
            self.entry = e.clone();
            self.rebuild(app);
            self.show(app);
            if let (true, Entry::User(id)) = (new_smart, &e) {
                self.open_editor(app, *id);
            }
            if let Some(n) = from {
                self.say(app, tr!("created playlist “{}” with {} tracks", name, n));
            }
        }
    }

    pub(super) fn side_delete(&mut self, app: &App) {
        if self.protected(app, tr!("delete")) {
            return;
        }
        let e = self.entry.clone();
        if !is_named(&e) {
            return;
        }
        if let Entry::Stage(id) = e
            && self.stage(id).is_some_and(|s| s.role != openfader::store::Role::Middle)
        {
            return self.say(app, tr!("INBOX and COLLECTION cannot be deleted: rename them instead"));
        }
        let name = self.name_of(&e);
        if matches!(e, Entry::Category(_)) && tagging::is_default(&name) {
            return self.say(
                app,
                tr!("the Genre, Components and Situation categories cannot be deleted"),
            );
        }
        if self.confirm.as_ref() != Some(&e) {
            self.confirm = Some(e);
            return self.say(
                app,
                tr!(
                    "press DELETE again to delete “{}” (the tracks stay in the library)",
                    name
                ),
            );
        }
        let s = &self.store;
        let r = match &e {
            Entry::User(id) => s.action(tr!("Delete playlist"), || s.delete_node(*id)),
            Entry::Category(id) => {
                s.action(tr!("Delete tag category"), || s.delete_mytag_category(*id))
            }
            Entry::Tag(id) => s.action(tr!("Delete tag"), || s.delete_mytag(*id)),
            Entry::Bank(id) => s.action(tr!("Delete cue bank"), || s.delete_bank(*id)),
            Entry::Stage(id) => s.action(tr!("Delete stage"), || s.delete_stage(*id)),
            _ => return,
        };
        if self.try_store(app, r).is_none() {
            return;
        }
        self.entry = match e {
            Entry::User(_) => Entry::UserRoot,
            Entry::Bank(_) => Entry::BankRoot,
            Entry::Stage(_) => Entry::StageRoot,
            _ => Entry::TagsRoot,
        };
        self.rebuild(app);
        let live: Vec<i64> = self.mytags.iter().map(|t| t.id).collect();
        self.tag_filter.retain(|t| live.contains(t));
        self.reload_tag_map(app);
        self.show(app);
        self.say(app, tr!("deleted “{}”", name));
    }

    /// DUPLICATE TO MY PLAYLISTS: the chosen library playlist (a folder: with everything under it) as new top-level items of My Playlists.
    pub(super) fn side_copy(&mut self, app: &App) {
        let Entry::Lib(at) = self.entry else { return };
        if openfader::plugin::source::is_plugin(&self.lib_playlists[at].id) {
            return self.say(
                app,
                tr!("tracks of a plugin source have no file to copy yet: load them first"),
            );
        }
        let mut parents: Vec<Option<i64>> = vec![None];
        let (mut first, mut lists) = (None, 0);
        for (i, rel) in sidebar::subtree(&self.lib_playlists, at) {
            let p = self.lib_playlists[i].clone();
            parents.truncate(rel + 1);
            let parent = parents[rel.min(parents.len() - 1)];
            let made = if p.is_folder {
                self.store
                    .add_node(parent, &p.name, NodeKind::Folder)
                    .inspect(|id| parents.push(Some(*id)))
            } else {
                let keys = self
                    .lib
                    .tracks(Some(&p.id))
                    .map(|ts| ts.iter().map(store::key).collect::<Vec<_>>());
                keys.and_then(|keys| {
                    let id = self.store.add_node(parent, &p.name, NodeKind::Playlist)?;
                    self.store.add_entries(id, &keys)?;
                    Ok(id)
                })
            };
            match made {
                Ok(id) => {
                    first.get_or_insert(id);
                    lists += usize::from(!p.is_folder);
                }
                Err(e) => return self.say(app, tr!("duplicate failed: {}", e)),
            }
        }
        let Some(first) = first else { return };
        self.entry = Entry::User(first);
        self.rebuild(app);
        self.show(app);
        self.say(
            app,
            tr_n!(
                "copied {n} playlist to My Playlists",
                "copied {n} playlists to My Playlists",
                lists
            ),
        );
    }

    /// ADD TO PLAYLIST pressed.
    pub(super) fn open_add_to(&mut self, app: &App) {
        if self.selected.is_none() {
            return self.say(app, tr!("select a track to add"));
        }
        let (rows, ids) = playlist_labels(&self.nodes);
        if rows.is_empty() {
            return self.say(
                app,
                tr!("no playlists yet: choose My Playlists and + NEW > New playlist"),
            );
        }
        self.show_panel(app, Panel::AddTo(ids), tr!("ADD TO PLAYLIST"), rows);
    }

    /// A row of the ADD TO panel was clicked: the chosen track goes to the end of that playlist.
    pub(super) fn add_to(&mut self, app: &App, id: i64) {
        self.close_panel(app);
        let tracks = self.selection();
        if !tracks.is_empty() {
            self.add_tracks_to(app, id, &tracks);
        }
    }

    /// Keys of the tracks of `ts` the Collection lists, each once: only those can be in a playlist.
    fn collection_keys(&mut self, ts: &[Track]) -> Vec<String> {
        let Ok(c) = self.cache() else {
            return Vec::new();
        };
        let keys: Vec<String> = ts
            .iter()
            .map(store::key)
            .filter(|k| c.by_key.contains_key(k))
            .collect();
        fresh_keys(&[], &keys).0
    }

    /// `ts` go to the end of playlist `id` (ADD TO, a drop on the tree row): the ones it already holds are skipped.
    pub(in crate::app::browser) fn add_tracks_to(&mut self, app: &App, id: i64, ts: &[Track]) {
        if let [t] = ts {
            return self.add_track_to(app, id, t);
        }
        let keys = self.collection_keys(ts);
        let unknown = ts.len() - keys.len();
        let add = || -> Result<String, String> {
            let (fresh, held) = fresh_keys(&self.store.entries(id)?, &keys);
            if !fresh.is_empty() {
                self.store.add_entries(id, &fresh)?;
            }
            let mut msg = tr!("{} added, {} already in the playlist", fresh.len(), held);
            if unknown > 0 {
                msg += &format!(" · {}", tr!("{} not in the Collection", unknown));
            }
            Ok(msg)
        };
        let msg = add().unwrap_or_else(|e| tr!("store: {}", e));
        if matches!(self.home, Home::User(h) if h == id) {
            self.load(app);
        }
        self.say(app, msg);
    }

    /// `t` goes to the end of playlist `id` (ADD TO, a drop on the tree row).
    pub(in crate::app::browser) fn add_track_to(&mut self, app: &App, id: i64, t: &Track) {
        let key = store::key(t);
        let name = self.node(id).map(|n| n.name.clone()).unwrap_or_default();
        // A playlist lists only what the Collection holds: an entry for a track of a device or an opened source would never show.
        let known = self.cache().map(|c| c.by_key.contains_key(&key));
        let add = || -> Result<String, String> {
            if !known? {
                return Ok(tr!(
                    "“{}” is not in the Collection: import it first",
                    t.title
                ));
            }
            if self.store.entries(id)?.contains(&key) {
                return Ok(tr!("“{}” is already in {}", t.title, name));
            }
            self.store.add_entries(id, std::slice::from_ref(&key))?;
            Ok(tr!("added “{}” to {}", t.title, name))
        };
        let msg = add().unwrap_or_else(|e| tr!("store: {}", e));
        if matches!(self.home, Home::User(h) if h == id) {
            self.load(app);
        }
        self.say(app, msg);
    }
}

/// Of `keys`, the ones not in `have` (and not repeated), in order, and how many were left out.
pub(in crate::app::browser) fn fresh_keys(have: &[String], keys: &[String]) -> (Vec<String>, usize) {
    let mut seen: std::collections::HashSet<&str> = have.iter().map(String::as_str).collect();
    let fresh: Vec<String> = keys
        .iter()
        .filter(|k| seen.insert(k.as_str()))
        .cloned()
        .collect();
    let held = keys.len() - fresh.len();
    (fresh, held)
}

/// What a playlist made from `tracks` is called until the user types another name: the first artist, else "New playlist".
fn default_name(tracks: &[Track]) -> String {
    tracks
        .iter()
        .map(|t| t.artist.trim())
        .find(|a| !a.is_empty())
        .map_or_else(|| tr!("New playlist").to_owned(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_down_and_into_reorder_the_store_tree() {
        let st = store::Store::in_memory().unwrap();
        let (a, b, f) = (
            st.add_node(None, "a", NodeKind::Playlist).unwrap(),
            st.add_node(None, "b", NodeKind::Playlist).unwrap(),
            st.add_node(None, "f", NodeKind::Folder).unwrap(),
        );
        let order = |st: &store::Store| {
            st.nodes()
                .unwrap()
                .into_iter()
                .map(|n| (n.name, n.parent))
                .collect::<Vec<_>>()
        };
        let pos_of = |id| {
            st.nodes()
                .unwrap()
                .into_iter()
                .find(|n| n.id == id)
                .unwrap()
                .pos
        };
        st.move_node(b, None, pos_of(b) - 1).unwrap(); // Act::Up
        assert_eq!(
            order(&st),
            [("b".into(), None), ("a".into(), None), ("f".into(), None)]
        );
        st.move_node(b, None, pos_of(b) + 1).unwrap(); // Act::Down
        assert_eq!(order(&st)[0].0, "a");
        st.move_node(a, Some(f), i64::from(i32::MAX)).unwrap(); // Act::Into
        assert_eq!(
            order(&st),
            [
                ("b".into(), None),
                ("f".into(), None),
                ("a".into(), Some(f))
            ]
        );
        assert!(st.move_node(f, Some(f), 0).is_err());
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tracks_a_playlist_holds_are_skipped_and_counted() {
        let (fresh, held) = fresh_keys(&keys(&["a", "b"]), &keys(&["b", "c", "a", "d", "c"]));
        assert_eq!((fresh, held), (keys(&["c", "d"]), 3));
        assert_eq!(fresh_keys(&[], &keys(&["x", "y"])), (keys(&["x", "y"]), 0));
        assert_eq!(fresh_keys(&keys(&["x"]), &[]), (Vec::new(), 0));
    }

    #[test]
    fn a_playlist_from_the_selection_is_named_after_the_first_artist() {
        let t = |artist: &str| Track {
            artist: artist.into(),
            ..Track::default()
        };
        assert_eq!(default_name(&[t(""), t(" Club Angel "), t("Other")]), "Club Angel");
        assert_eq!(default_name(&[t("")]), tr!("New playlist"));
    }
}
