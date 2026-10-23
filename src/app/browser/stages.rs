//! The Stages node of the sidebar: INBOX → REVIEW → COLLECTION ([`openfader::inbox`]). New music from the watched folders waits
//! in INBOX; a stage lists the tracks waiting in it (the Collection's row is the Collection). A track moves on by dragging it onto
//! another stage's row, or from MORE (the chosen track, or every shown one, to a stage); + NEW adds a stage before the
//! Collection, RENAME and DELETE work on the chosen one (INBOX and COLLECTION stay). MORE also lists the recent moves, each
//! undoable ([`openfader::inbox::undo_batch`]); Edit > Undo takes back the newest like any edit.

use super::{
    playlists::Panel,
    sidebar::{Entry, StageRow},
    App, Home, View,
};
use openfader::{
    library::Track,
    store::{Role, Stage},
};
use std::collections::HashMap;

/// The stages and how many tracks wait in each, as of the last [`View::rebuild`].
#[derive(Default)]
pub(super) struct Stages {
    list: Vec<Stage>,
    counts: HashMap<i64, usize>,
}

/// One row of the MORE panel on a stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    /// The chosen track goes to this stage.
    Chosen(i64),
    /// Every shown track goes to this stage.
    Shown(i64),
    /// Opens the list of recent moves.
    Recent,
}

/// Recent moves the panel lists.
const RECENT: usize = 15;

/// `HH:MM` (local) of unix time `at`.
fn clock(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0).map_or_else(String::new, |t| {
        t.with_timezone(&chrono::Local).format("%H:%M").to_string()
    })
}

impl View {
    /// Reads the stages and their counts again.
    pub(super) fn read_stages(&mut self, app: &App) {
        self.stages.list = self.ok(app, self.store.stages(), Vec::new());
        self.stages.counts = self.ok(app, self.store.stage_counts(), HashMap::new());
    }

    /// The sidebar's stage rows.
    pub(super) fn stage_rows(&self) -> Vec<StageRow> {
        let count = |id| self.stages.counts.get(&id).copied().unwrap_or(0);
        self.stages
            .list
            .iter()
            .map(|s| StageRow {
                id: s.id,
                name: s.name.clone(),
                count: count(s.id),
            })
            .collect()
    }

    pub(super) fn stage(&self, id: i64) -> Option<&Stage> {
        self.stages.list.iter().find(|s| s.id == id)
    }

    /// What a stage lists: the Collection's own row is the whole Collection.
    pub(super) fn stage_home(&self, id: i64) -> Home {
        match self.stage(id).map(|s| s.role) {
            Some(Role::Collection) => Home::Playlist(None),
            Some(_) => Home::Stage(id),
            None => Home::Nothing,
        }
    }

    /// MORE pressed on the Stages heading or a stage.
    pub(super) fn open_stage_panel(&mut self, app: &App) {
        let (mut rows, mut acts) = (Vec::new(), Vec::new());
        if let Entry::Stage(here) = self.entry
            && self.stage(here).is_some_and(|s| s.role != Role::Collection)
        {
            let shown = self.shown.len();
            for s in self.stages.list.iter().filter(|s| s.id != here) {
                rows.push(tr!("Move the chosen track to {}", s.name));
                acts.push(Act::Chosen(s.id));
                rows.push(tr!("Move the {} shown tracks to {}", shown, s.name));
                acts.push(Act::Shown(s.id));
            }
        }
        rows.push(tr!("Recent moves…").to_owned());
        acts.push(Act::Recent);
        self.show_panel(app, Panel::Stage(acts), tr!("MORE"), rows);
    }

    /// A MORE row of a stage was chosen.
    pub(super) fn stage_act(&mut self, app: &App, act: Act) {
        match act {
            Act::Chosen(to) => match self.selected.clone() {
                Some(t) if self.list.contains(&t) => self.move_to_stage(app, &[t], to),
                _ => self.say(app, tr!("choose a track in the list first")),
            },
            Act::Shown(to) => {
                let tracks: Vec<Track> = self.shown.iter().map(|&i| self.list[i].clone()).collect();
                self.move_to_stage(app, &tracks, to);
            }
            Act::Recent => self.open_moves(app),
        }
    }

    /// `tracks` go to stage `to` (a drop on its row, MORE): files too when the settings say so. Says what happened.
    pub(in crate::app::browser) fn move_to_stage(&mut self, app: &App, tracks: &[Track], to: i64) {
        let ids: Vec<i64> = tracks.iter().filter_map(|t| t.id).collect();
        let cfg = crate::app::library::inbox::config();
        let name = self.stage(to).map(|s| s.name.clone()).unwrap_or_default();
        match openfader::inbox::move_tracks(&self.store, &ids, to, &cfg) {
            Ok(r) => {
                self.rebuild(app);
                self.load(app);
                let mut said = if r.moved > 0 {
                    openfader::plugin::event::fire(
                        "inbox_moved",
                        serde_json::json!({
                            "stage": name,
                            "files": r.files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>(),
                        }),
                    );
                    tr_n!("moved {n} track to {}", "moved {n} tracks to {}", r.moved, name)
                } else {
                    tr!("nothing moved to {}: the tracks are there already, or in the Collection", name)
                };
                if let Some(first) = r.failed.first() {
                    said += &tr!(" · {} not moved: {}", r.failed.len(), first);
                }
                self.say(app, said);
            }
            Err(e) => self.say(app, tr!("move failed: {}", e)),
        }
    }

    /// The list of recent moves, newest first; a row undoes that move.
    fn open_moves(&mut self, app: &App) {
        let batches = self.ok(app, self.store.recent_batches(RECENT), Vec::new());
        if batches.is_empty() {
            return self.say(app, tr!("no recent moves to undo"));
        }
        let rows = batches
            .iter()
            .map(|b| tr!("Undo: {} · {}", b.label, clock(b.at)))
            .collect();
        let ids = batches.iter().map(|b| b.action).collect();
        self.show_panel(app, Panel::Moves(ids), tr!("RECENT MOVES"), rows);
    }

    /// A row of the recent moves was chosen: that batch goes back (files, stages, paths).
    pub(super) fn undo_move(&mut self, app: &App, action: i64) {
        match openfader::inbox::undo_batch(&self.store, action) {
            Ok(r) => {
                self.rebuild(app);
                self.load(app);
                let mut said = tr_n!("moved {n} track back", "moved {n} tracks back", r.moved);
                if r.skipped > 0 {
                    said += &tr!(" · {} changed since, left alone", r.skipped);
                }
                if let Some(first) = r.failed.first() {
                    said += &tr!(" · {} not moved: {}", r.failed.len(), first);
                }
                self.say(app, said);
            }
            Err(e) => self.say(app, tr!("undo: {}", e)),
        }
    }
}
