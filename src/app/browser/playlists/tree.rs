//! Sidebar rows: what each [`Entry`] is, shows and says, and the node-tree helpers behind the ADD TO and MORE panels.

use super::Act;
use crate::app::browser::{
    sidebar::{self, Entry, Inputs},
    App, BrowserState, Home, View,
};
use openfader::store::{self, NodeKind};
use crate::app::{Icon, TreeRow};
use slint::{ComponentHandle, Model};

/// "Folder › Playlist" path of node `id`.
fn path_of(nodes: &[store::Node], mut id: i64) -> String {
    let mut parts = Vec::new();
    // The store lists nodes depth-first, so a parent is always found; the bound only guards a corrupt cycle.
    for _ in 0..64 {
        let Some(n) = nodes.iter().find(|n| n.id == id) else {
            break;
        };
        parts.push(n.name.as_str());
        match n.parent {
            Some(p) => id = p,
            None => break,
        }
    }
    parts.reverse();
    parts.join(" › ")
}

/// "Folder › Playlist" labels of every plain playlist, with their node ids, for the ADD TO panel.
pub fn playlist_labels(nodes: &[store::Node]) -> (Vec<String>, Vec<Option<i64>>) {
    nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Playlist)
        .map(|n| (path_of(nodes, n.id), Some(n.id)))
        .unzip()
}

/// Whether `of` is `id` or sits somewhere under it.
fn inside(nodes: &[store::Node], of: i64, id: i64) -> bool {
    let mut at = Some(of);
    for _ in 0..64 {
        match at {
            Some(n) if n == id => return true,
            Some(n) => at = nodes.iter().find(|x| x.id == n).and_then(|x| x.parent),
            None => return false,
        }
    }
    true
}

/// The MORE panel's rows for node `id`: Edit for a smart playlist, Up / Down where there is a sibling to pass, and every
/// folder it may move into (not itself or something inside it, not where it already is).
pub fn more_rows(nodes: &[store::Node], id: i64) -> Vec<(String, Act)> {
    let Some(me) = nodes.iter().find(|n| n.id == id) else {
        return Vec::new();
    };
    let siblings = nodes.iter().filter(|n| n.parent == me.parent).count() as i64;
    let mut rows = Vec::new();
    if me.kind == NodeKind::Smart {
        rows.push((tr!("Edit conditions…").to_owned(), Act::Edit));
    }
    if me.pos > 0 {
        rows.push((tr!("▲ Move up").to_owned(), Act::Up));
    }
    if me.pos + 1 < siblings {
        rows.push((tr!("▼ Move down").to_owned(), Act::Down));
    }
    if me.parent.is_some() {
        rows.push((tr!("Move to the top level").to_owned(), Act::Into(None)));
    }
    let folders = nodes.iter().filter(|f| {
        f.kind == NodeKind::Folder && Some(f.id) != me.parent && !inside(nodes, f.id, id)
    });
    rows.extend(folders.map(|f| {
        (
            tr!("Move into ▸ {}", path_of(nodes, f.id)),
            Act::Into(Some(f.id)),
        )
    }));
    rows
}

impl View {
    /// `BrowserState.side-kind` of an entry.
    pub(super) fn side_kind(&self, e: &Entry) -> i32 {
        match e {
            Entry::UserRoot => 1,
            Entry::User(id) => match self.node(*id).map(|n| n.kind) {
                Some(NodeKind::Playlist) => 3,
                Some(NodeKind::Smart) => 4,
                _ => 2,
            },
            Entry::TagsRoot => 5,
            Entry::BankRoot => 13,
            Entry::Bank(_) => 14,
            Entry::StageRoot => 15,
            Entry::Stage(_) => 16,
            Entry::Category(_) => 6,
            Entry::Tag(_) => 7,
            Entry::Lib(i) => {
                if self.lib_playlists[*i].is_folder {
                    9
                } else {
                    8
                }
            }
            _ => self.source_side_kind(e).unwrap_or(0),
        }
    }

    fn home_of(&self, e: &Entry) -> Home {
        match e {
            Entry::All => Home::Playlist(None),
            Entry::Tags => Home::Tags,
            Entry::Session => Home::History,
            Entry::Dupes => Home::Dupes,
            Entry::Health(c) => Home::Health(*c),
            Entry::CueAnalysis => Home::CueAnalysis,
            Entry::User(id) => match self.node(*id).map(|n| n.kind) {
                Some(NodeKind::Playlist) => Home::User(*id),
                Some(NodeKind::Smart) => Home::Smart(*id),
                _ => Home::Nothing,
            },
            Entry::Day(d) => Home::Day(d.clone()),
            Entry::Tag(id) => Home::Tag(*id),
            Entry::Bank(id) => Home::Bank(*id),
            Entry::Stage(id) => self.stage_home(*id),
            Entry::Lib(i) if !self.lib_playlists[*i].is_folder => {
                Home::Playlist(Some(self.lib_playlists[*i].id.clone()))
            }
            _ => self.source_home(e).unwrap_or(Home::Nothing),
        }
    }

    /// What the status line says for an entry that lists no tracks.
    pub(in crate::app::browser) fn entry_hint(&self) -> &'static str {
        match self.entry {
            Entry::HealthRoot => crate::app::browser::health::root_hint(),
            Entry::UserRoot => {
                tr!(
                    "My Playlists: + NEW makes a playlist, folder or smart playlist; ADD TO PLAYLIST puts the chosen track in a playlist"
                )
            }
            Entry::HistoryRoot => {
                tr!("History: tracks that stayed on air for 60 s, one list per day")
            }
            Entry::StageRoot => {
                tr!(
                    "Stages: new music from the watched folders waits in INBOX; drag a track onto the next stage or use MORE; + NEW adds a stage before COLLECTION"
                )
            }
            Entry::BankRoot => {
                tr!(
                    "Cue Bank: + NEW makes a list of up to 8 cues (A–H) of different tracks; the device export carries the lists"
                )
            }
            Entry::TagsRoot => {
                tr!("Tags: + NEW adds a category; choose a category and + NEW adds a tag to it")
            }
            Entry::LibRoot => {
                tr!("Library: read-only; DUPLICATE TO MY PLAYLISTS copies a playlist you can edit")
            }
            Entry::Lib(_) => {
                tr!(
                    "Folder of the library (read-only): DUPLICATE TO MY PLAYLISTS copies it with its playlists"
                )
            }
            Entry::User(_) => {
                tr!("Folder: + NEW makes a playlist, folder or smart playlist inside it")
            }
            Entry::Category(_) => {
                tr!("Category: + NEW adds a tag; choose a tag to list its tracks")
            }
            _ => self.source_hint(&self.entry).unwrap_or(""),
        }
    }

    /// Reads the store's tree data again and rebuilds the sidebar rows (the list on screen is untouched).
    pub(in crate::app::browser) fn rebuild(&mut self, app: &App) {
        let store = self.store.clone();
        self.nodes = self.ok(app, store.nodes(), Vec::new());
        self.cats = self.ok(app, store.mytag_categories(), Vec::new());
        self.mytags = self.ok(app, store.mytags(), Vec::new());
        self.banks = self.ok(app, store.banks(), Vec::new());
        self.read_stages(app);
        let days = self.ok(app, store.history_day_counts(), Vec::new());
        let src_rows = self.source_rows();
        let (cue_on, all_tracks, banks_on) = crate::app::prefs::with(|p| {
            let v = &p.view;
            (
                v.panel_cue_analysis,
                v.panel_all_tracks,
                v.media_hot_cue_bank,
            )
        });
        let cue_analysis = if cue_on {
            self.cue_analysis_list().ok().map(|l| l.len())
        } else {
            None
        };
        let stage_rows = self.stage_rows();
        let (mut entries, mut labels) = sidebar::build(&Inputs {
            tag_list: self.tags.len(),
            session: self.history.len(),
            lib: &self.lib_playlists,
            nodes: &self.nodes,
            days: &days,
            cats: &self.cats,
            tags: &self.mytags,
            sources: &src_rows,
            all_tracks,
            cue_analysis,
            banks: banks_on.then_some(&self.banks[..]),
            stages: &stage_rows,
        });
        self.health_insert(&mut entries, &mut labels);
        // The chosen row is never folded away (a playlist just made, a device just opened): its heading opens.
        let at = entries.iter().position(|e| *e == self.entry);
        if let Some(h) = at.and_then(|r| sidebar::heading_of(&entries, &labels, r)) {
            if !self.open.contains(h) {
                self.open.push(h.clone());
            }
        }
        let (entries, labels) = sidebar::fold(entries, labels, &self.open);
        let mut heads = 0;
        let rows: Vec<TreeRow> = entries
            .iter()
            .zip(&labels)
            .map(|(e, l)| {
                let head = sidebar::is_heading(e);
                let row = TreeRow {
                    text: l.trim_start().trim_start_matches("▸ ").into(),
                    depth: sidebar::depth(l) as i32,
                    icon: self.icon_of(e),
                    head,
                    open: head && self.open.contains(e),
                    heads,
                };
                heads += i32::from(head);
                row
            })
            .collect();
        self.entries = entries;
        self.sidebar.set_vec(rows);
        let row = self
            .entries
            .iter()
            .position(|e| *e == self.entry)
            .unwrap_or(0);
        self.entry = self.entries[row].clone();
        let state = app.global::<BrowserState>();
        state.set_side_row(row as i32);
        state.set_side_kind(self.side_kind(&self.entry));
        self.publish_source(app);
    }

    /// Click on a heading (or Left / Right on it): its rows unfold or fold away.
    pub(in crate::app::browser) fn toggle(&mut self, app: &App, row: usize) {
        let Some(e) = self.entries.get(row).filter(|e| sidebar::is_heading(e)).cloned() else {
            return;
        };
        match self.open.iter().position(|o| *o == e) {
            Some(k) => {
                self.open.remove(k);
            }
            None => self.open.push(e),
        }
        self.rebuild(app);
    }

    /// BACK (or Left) on `row`: the section it is in folds and its heading becomes the chosen entry; the caller chooses the
    /// returned row (the heading's, or `row` when it is under none), which shows that entry.
    pub(in crate::app::browser) fn tree_back(&mut self, app: &App, row: usize) -> usize {
        let Some(e) = self.entries.get(row) else {
            return row;
        };
        let head = if sidebar::is_heading(e) {
            row
        } else if self.sidebar.row_data(row).is_some_and(|r| r.depth == 0) {
            return row; // a top row under no heading (All tracks, a library source …)
        } else {
            // The shown rows above, back to the first one that is not indented.
            let top = (0..row).rev().find(|&k| self.sidebar.row_data(k).is_some_and(|r| r.depth == 0));
            match top.filter(|&k| sidebar::is_heading(&self.entries[k])) {
                Some(k) => k,
                None => return row,
            }
        };
        let e = self.entries[head].clone();
        self.open.retain(|o| *o != e);
        self.entry = e;
        self.rebuild(app);
        head
    }

    /// The glyph before a row: what kind of list it is.
    fn icon_of(&self, e: &Entry) -> Icon {
        match e {
            Entry::All | Entry::PlaylistsAll | Entry::SourceAll(_) => Icon::Browse,
            Entry::Tags => Icon::Star,
            Entry::Session | Entry::HistoryRoot | Entry::Day(_) => Icon::Clock,
            Entry::Dupes => Icon::Grid,
            Entry::CueAnalysis => Icon::MixPoint,
            Entry::UserRoot => Icon::List,
            Entry::User(id) => match self.node(*id).map(|n| n.kind) {
                Some(NodeKind::Folder) => Icon::Folder,
                Some(NodeKind::Smart) => Icon::Smart,
                _ => Icon::List,
            },
            Entry::BankRoot | Entry::Bank(_) => Icon::Cue,
            Entry::StageRoot | Entry::Stage(_) => Icon::Inbox,
            Entry::TagsRoot | Entry::Category(_) | Entry::Tag(_) => Icon::Tag,
            Entry::LibRoot => Icon::Lock,
            Entry::Lib(i) if self.lib_playlists[*i].is_folder => Icon::Folder,
            Entry::Lib(_) => Icon::List,
            Entry::HealthRoot | Entry::Health(_) => Icon::Pulse,
            Entry::DevicesRoot => Icon::Usb,
            Entry::Source(i) if self.sources.get(*i).is_some_and(|s| s.is_device()) => Icon::Usb,
            Entry::Source(_) => Icon::Folder,
            Entry::SourceList(i, j) => match self.sources.get(*i).and_then(|s| s.playlists.get(*j)) {
                Some(p) if p.is_folder => Icon::Folder,
                _ => Icon::List,
            },
        }
    }

    pub(in crate::app::browser) fn select(&mut self, app: &App, row: usize) {
        if let Some(e) = self.entries.get(row).cloned() {
            self.entry = e;
            if self.entry == Entry::HealthRoot {
                self.health.rescan();
            }
            if let Entry::Source(i) = self.entry {
                self.open_source(app, i);
            }
            self.show(app);
        }
    }

    /// Shows what the chosen entry lists.
    pub(in crate::app::browser) fn show(&mut self, app: &App) {
        self.confirm = None;
        self.home = self.home_of(&self.entry);
        if self.sel_entry != self.entry {
            self.sel.clear(); // another list: only the chosen track stays selected
            self.sel_entry = self.entry.clone();
        }
        self.related = None;
        app.global::<BrowserState>()
            .set_side_kind(self.side_kind(&self.entry));
        self.publish_source(app);
        self.load(app);
    }

    /// Name of the list on screen, for file names and messages.
    pub(in crate::app::browser) fn list_name(&self) -> String {
        match &self.entry {
            e if super::is_named(e) => self.name_of(e),
            Entry::Lib(i) => self.lib_playlists[*i].name.clone(),
            Entry::Source(i) | Entry::SourceAll(i) => self
                .sources
                .get(*i)
                .map_or_else(String::new, |s| s.name.clone()),
            Entry::SourceList(i, j) => self
                .sources
                .get(*i)
                .and_then(|s| s.playlists.get(*j))
                .map_or_else(String::new, |p| p.name.clone()),
            Entry::Day(d) => d.clone(),
            Entry::All => "All tracks".to_owned(),
            Entry::Tags => "Tag List".to_owned(),
            Entry::Session => "Session history".to_owned(),
            Entry::Dupes => "Duplicate tracks".to_owned(),
            Entry::HealthRoot => "Library health".to_owned(),
            Entry::Health(c) => c.label().to_owned(),
            Entry::CueAnalysis => "CUE Analysis".to_owned(),
            _ => "Tracks".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::Node;

    fn node(id: i64, parent: Option<i64>, name: &str, kind: NodeKind) -> Node {
        Node {
            id,
            parent,
            name: name.into(),
            kind,
            pos: 0,
        }
    }

    #[test]
    fn add_to_lists_only_plain_playlists_with_their_folder_path() {
        let nodes = [
            node(1, None, "Sets", NodeKind::Folder),
            node(2, Some(1), "Friday", NodeKind::Playlist),
            node(3, Some(1), "Smart one", NodeKind::Smart),
            node(4, None, "Loose", NodeKind::Playlist),
        ];
        let (labels, ids) = playlist_labels(&nodes);
        assert_eq!(labels, ["Sets › Friday", "Loose"]);
        assert_eq!(ids, [Some(2), Some(4)]);
    }

    #[test]
    fn more_offers_only_moves_that_change_something() {
        let mut nodes = vec![
            Node {
                pos: 0,
                ..node(1, None, "A", NodeKind::Folder)
            },
            Node {
                pos: 1,
                ..node(2, None, "B", NodeKind::Smart)
            },
            Node {
                pos: 0,
                ..node(3, Some(1), "inner", NodeKind::Folder)
            },
            Node {
                pos: 0,
                ..node(4, Some(3), "deep", NodeKind::Playlist)
            },
        ];
        let acts = |nodes: &[Node], id| {
            more_rows(nodes, id)
                .into_iter()
                .map(|r| r.1)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            acts(&nodes, 2),
            [Act::Edit, Act::Up, Act::Into(Some(1)), Act::Into(Some(3))],
            "last sibling: no Down; smart edits"
        );
        assert_eq!(
            acts(&nodes, 1),
            [Act::Down],
            "a folder cannot enter itself or its children"
        );
        assert_eq!(
            acts(&nodes, 4),
            [Act::Into(None), Act::Into(Some(1))],
            "only one sibling; not into its own parent"
        );
        nodes.truncate(1);
        assert!(acts(&nodes, 1).is_empty() && acts(&nodes, 9).is_empty());
    }
}
