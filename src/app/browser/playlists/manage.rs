//! The sidebar's management bar: name bar, + NEW, rename, delete, DUPLICATE TO MY PLAYLISTS, MORE and ADD TO PLAYLIST.

use super::{Act, Ask, Panel, is_named, more_rows, playlist_labels};
use crate::app::{
    browser::{
        App, BrowserState, Home, View, tagging,
        sidebar::{self, Entry},
        smart::Smart,
    },
    plural,
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
            "delete" => self.side_delete(app),
            "renumber" | "sort-title" | "sort-artist" | "sort-genre" | "sort-key" | "sort-bpm"
            | "sort-time" | "sort-rating" | "sort-comment" => self.reorder(app, op),
            _ => self.say(app, format!("unknown playlist action {op}")),
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
                    "New playlist name:",
                    "New folder name:",
                    "New smart playlist name:",
                ][which as usize];
                self.ask(app, Ask::NewNode(parent, k), prompt, String::new());
            }
            (5..=7, 0) => self.ask(app, Ask::NewCategory, "New tag category:", String::new()),
            (13 | 14, 0) => self.ask(app, Ask::NewBank, "New Cue Bank:", String::new()),
            (6 | 7, 1) => {
                let cat = match &self.entry {
                    Entry::Category(c) => Some(*c),
                    Entry::Tag(t) => self.mytags.iter().find(|m| m.id == *t).map(|m| m.category),
                    _ => None,
                };
                if let Some(c) = cat {
                    self.ask(app, Ask::NewTag(c), "New tag:", String::new());
                }
            }
            _ => {}
        }
    }

    /// MORE pressed on a node of My Playlists.
    pub(super) fn open_more(&mut self, app: &App) {
        if matches!(self.entry, Entry::Bank(_)) {
            return self.open_bank_panel(app);
        }
        let Entry::User(id) = self.entry else { return };
        let (rows, acts): (Vec<String>, Vec<Act>) = more_rows(&self.nodes, id).into_iter().unzip();
        if rows.is_empty() {
            return self.say(
                app,
                "nothing to do here: it is the only item and has no folder to move into",
            );
        }
        self.show_panel(app, Panel::More(acts), "MORE", rows);
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
                ("New playlist", 0),
                ("New folder", 1),
                ("New smart playlist", 2),
            ],
            5 => vec![("New category", 0)],
            6 | 7 => vec![("New category", 0), ("New tag", 1)],
            13 | 14 => vec![("New Cue Bank", 0)],
            _ => return,
        }
        .into_iter()
        .map(|(r, i)| (r, Some(i)))
        .unzip();
        self.show_panel(
            app,
            Panel::New(ids),
            "NEW",
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
                "the Genre, Components and Situation categories keep their names",
            );
        }
        self.ask(app, Ask::Rename(e), "Rename to:", name);
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
            return self.say(app, "a name is needed");
        }
        let st = self.store.clone();
        let new_smart = matches!(ask, Ask::NewNode(_, NodeKind::Smart));
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
            Ask::Rename(_) => return,
            Ask::NewCategory => st.add_mytag_category(&name).map(Entry::Category),
            Ask::NewTag(c) => st.add_mytag(c, &name).map(Entry::Tag),
            Ask::NewBank => st.add_bank(&name).map(Entry::Bank),
        };
        if let Some(e) = self.try_store(app, made) {
            self.entry = e.clone();
            self.rebuild(app);
            self.show(app);
            if let (true, Entry::User(id)) = (new_smart, &e) {
                self.open_editor(app, *id);
            }
        }
    }

    pub(super) fn side_delete(&mut self, app: &App) {
        if self.protected(app, "delete") {
            return;
        }
        let e = self.entry.clone();
        if !is_named(&e) {
            return;
        }
        let name = self.name_of(&e);
        if matches!(e, Entry::Category(_)) && tagging::is_default(&name) {
            return self.say(
                app,
                "the Genre, Components and Situation categories cannot be deleted",
            );
        }
        if self.confirm.as_ref() != Some(&e) {
            self.confirm = Some(e);
            return self.say(
                app,
                format!("press DELETE again to delete “{name}” (the tracks stay in the library)"),
            );
        }
        let r = match &e {
            Entry::User(id) => self.store.delete_node(*id),
            Entry::Category(id) => self.store.delete_mytag_category(*id),
            Entry::Tag(id) => self.store.delete_mytag(*id),
            Entry::Bank(id) => self.store.delete_bank(*id),
            _ => return,
        };
        if self.try_store(app, r).is_none() {
            return;
        }
        self.entry = match e {
            Entry::User(_) => Entry::UserRoot,
            Entry::Bank(_) => Entry::BankRoot,
            _ => Entry::TagsRoot,
        };
        self.rebuild(app);
        let live: Vec<i64> = self.mytags.iter().map(|t| t.id).collect();
        self.tag_filter.retain(|t| live.contains(t));
        self.reload_tag_map(app);
        self.show(app);
        self.say(app, format!("deleted “{name}”"));
    }

    /// DUPLICATE TO MY PLAYLISTS: the chosen library playlist (a folder: with everything under it) as new top-level items of My Playlists.
    pub(super) fn side_copy(&mut self, app: &App) {
        let Entry::Lib(at) = self.entry else { return };
        if openfader::plugin::source::is_plugin(&self.lib_playlists[at].id) {
            return self.say(
                app,
                "tracks of a plugin source have no file to copy yet: load them first",
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
                Err(e) => return self.say(app, format!("duplicate failed: {e}")),
            }
        }
        let Some(first) = first else { return };
        self.entry = Entry::User(first);
        self.rebuild(app);
        self.show(app);
        self.say(
            app,
            format!("copied {lists} playlist{} to My Playlists", plural(lists)),
        );
    }

    /// ADD TO PLAYLIST pressed.
    pub(super) fn open_add_to(&mut self, app: &App) {
        if self.selected.is_none() {
            return self.say(app, "select a track to add");
        }
        let (rows, ids) = playlist_labels(&self.nodes);
        if rows.is_empty() {
            return self.say(
                app,
                "no playlists yet: choose My Playlists and + NEW > New playlist",
            );
        }
        self.show_panel(app, Panel::AddTo(ids), "ADD TO PLAYLIST", rows);
    }

    /// A row of the ADD TO panel was clicked: the chosen track goes to the end of that playlist.
    pub(super) fn add_to(&mut self, app: &App, id: i64) {
        self.close_panel(app);
        if let Some(t) = self.selected.clone() {
            self.add_track_to(app, id, &t);
        }
    }

    /// `t` goes to the end of playlist `id` (ADD TO, a drop on the tree row).
    pub(in crate::app::browser) fn add_track_to(&mut self, app: &App, id: i64, t: &Track) {
        let key = store::key(t);
        let name = self.node(id).map(|n| n.name.clone()).unwrap_or_default();
        let add = || -> Result<String, String> {
            if self.store.entries(id)?.contains(&key) {
                return Ok(format!("“{}” is already in {name}", t.title));
            }
            self.store.add_entries(id, std::slice::from_ref(&key))?;
            Ok(format!("added “{}” to {name}", t.title))
        };
        let msg = add().unwrap_or_else(|e| format!("store: {e}"));
        if matches!(self.home, Home::User(h) if h == id) {
            self.load(app);
        }
        self.say(app, msg);
    }
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
