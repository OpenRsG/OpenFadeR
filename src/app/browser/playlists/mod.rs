//! My Playlists and the sidebar's management bar: user folders / playlists / smart playlists live in the
//! [store](openfader::store) (`nodes`, `entries`), the opened library's own playlists stay read-only beside them and
//! can be copied in ("DUPLICATE TO MY PLAYLISTS"). Also here: what each sidebar [`Entry`] shows, the create / rename /
//! delete flow (a name bar over the table; DELETE needs a second press), the ADD TO PLAYLIST panel, and ▲ ▼ REMOVE on a
//! playlist. A playlist stores track keys ([`store::key`]); an entry whose track the library no longer has is skipped.
//!
//! Tracks go in with ADD TO PLAYLIST or, with the Playlist Palette on, by drag and drop onto a palette ([`super::palette`]).
//!
//! Children: [`tree`] (sidebar rows and what an entry shows), [`lists`] (the track lists of those entries, reorder, remove,
//! move), [`manage`] (name bar, new / rename / delete / copy, the MORE and ADD TO panels).

mod lists;
mod manage;
mod tree;

use super::{hotcuebank, sidebar::Entry, stages, App, BrowserState, View};
use openfader::store::{self, NodeKind};
use slint::{ComponentHandle, StandardListViewItem};
use std::{cell::RefCell, fmt::Display, rc::Rc};
use tree::more_rows;

pub use tree::playlist_labels;

/// What the name bar is asking a name for.
#[derive(Clone)]
pub enum Ask {
    /// New folder / playlist / smart playlist under a folder (`None`: top level of My Playlists).
    NewNode(Option<i64>, NodeKind),
    Rename(Entry),
    NewCategory,
    NewTag(i64),
    NewBank,
    NewStage,
}

/// The picker panel on the table's right edge; each row's id is a playlist node or a tag (`None`: a heading).
pub enum Panel {
    Closed,
    AddTo(Vec<Option<i64>>),
    /// What + NEW can make here (0 playlist / category, 1 folder / tag, 2 smart playlist).
    New(Vec<Option<i64>>),
    /// tags of the chosen track.
    Assign(Vec<Option<i64>>),
    /// tags the list must carry.
    Filter(Vec<Option<i64>>),
    /// What MORE can do with the chosen node of My Playlists.
    More(Vec<Act>),
    /// What MORE can do with the chosen Cue Bank.
    Bank(Vec<hotcuebank::Act>),
    /// What MORE can do on a stage: the moves it offers, and the list of recent moves.
    Stage(Vec<stages::Act>),
    /// The recent move batches (journal actions), newest first: a row undoes one.
    Moves(Vec<i64>),
}

/// One entry of the MORE panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    /// Smart playlist: open the editor.
    Edit,
    /// One place earlier / later among its siblings.
    Up,
    Down,
    /// Into this folder (`None`: top level of My Playlists), as the last item.
    Into(Option<i64>),
}

/// Entries the name bar can rename and DELETE can remove.
fn is_named(e: &Entry) -> bool {
    matches!(
        e,
        Entry::User(_) | Entry::Category(_) | Entry::Tag(_) | Entry::Bank(_) | Entry::Stage(_)
    )
}

impl View {
    pub(super) fn say(&self, app: &App, msg: impl Into<slint::SharedString>) {
        app.global::<BrowserState>().set_status(msg.into());
    }

    /// A store result: on error the message goes to the status line and `None` comes back.
    pub(super) fn try_store<T, E: Display>(&self, app: &App, r: Result<T, E>) -> Option<T> {
        r.map_err(|e| self.say(app, tr!("store: {}", e))).ok()
    }

    /// A store result: on error the message goes to the status line and `fallback` stands in.
    pub(super) fn ok<T>(&self, app: &App, r: Result<T, String>, fallback: T) -> T {
        self.try_store(app, r).unwrap_or(fallback)
    }

    /// Whether Library Protection forbids `what`; says so when it does.
    fn protected(&self, app: &App, what: &str) -> bool {
        let on = crate::app::library::settings::get().protect;
        if on {
            self.say(
                app,
                tr!(
                    "Library Protection is on: cannot {} (Preferences > Advanced > Browse)",
                    what
                ),
            );
        }
        on
    }

    fn node(&self, id: i64) -> Option<&store::Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn name_of(&self, e: &Entry) -> String {
        match e {
            Entry::User(id) => self.node(*id).map(|n| n.name.clone()),
            Entry::Category(id) => self
                .cats
                .iter()
                .find(|c| c.id == *id)
                .map(|c| c.name.clone()),
            Entry::Tag(id) => self
                .mytags
                .iter()
                .find(|t| t.id == *id)
                .map(|t| t.name.clone()),
            Entry::Bank(id) => self
                .banks
                .iter()
                .find(|b| b.id == *id)
                .map(|b| b.name.clone()),
            Entry::Stage(id) => self.stage(*id).map(|s| s.name.clone()),
            _ => None,
        }
        .unwrap_or_default()
    }

    fn ask(&mut self, app: &App, ask: Ask, prompt: &str, initial: String) {
        let s = app.global::<BrowserState>();
        self.ask = Some(ask);
        s.set_name_prompt(prompt.into());
        s.set_name_text(initial.into());
        s.set_name_open(true);
    }

    pub(super) fn close_panel(&mut self, app: &App) {
        self.panel = Panel::Closed;
        app.global::<BrowserState>().set_panel_open(false);
    }

    pub(super) fn show_panel(&mut self, app: &App, panel: Panel, title: &str, rows: Vec<String>) {
        let s = app.global::<BrowserState>();
        self.panel_rows.set_vec(
            rows.iter()
                .map(|r| StandardListViewItem::from(r.as_str()))
                .collect::<Vec<_>>(),
        );
        self.panel = panel;
        s.set_panel_title(title.into());
        s.set_panel_open(true);
    }
}

pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>) {
    let state = app.global::<BrowserState>();
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_side_rename(move || v.borrow_mut().side_rename(&weak.unwrap()));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_side_delete(move || v.borrow_mut().side_delete(&weak.unwrap()));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_side_copy(move || v.borrow_mut().side_copy(&weak.unwrap()));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_playlist_op(move |op| v.borrow_mut().playlist_op(&weak.unwrap(), &op));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_name_done(move |ok| v.borrow_mut().name_done(&weak.unwrap(), ok));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_panel_close(move || v.borrow_mut().close_panel(&weak.unwrap()));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_panel_request(move |which| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        let same = matches!(
            (&v.panel, which),
            (Panel::AddTo(_), 0)
                | (Panel::Assign(_), 1)
                | (Panel::Filter(_), 2)
                | (Panel::New(_), 3)
                | (Panel::More(_), 4)
                | (Panel::Bank(_), 4)
                | (Panel::Stage(_), 4)
                | (Panel::Moves(_), 4)
        );
        if same {
            return v.close_panel(&app);
        }
        match which {
            0 => v.open_add_to(&app),
            1 => v.open_tag_panel(&app, true),
            2 => v.open_tag_panel(&app, false),
            3 => v.open_new(&app),
            _ => v.open_more(&app),
        }
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_panel_row(move |i| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        let Ok(i) = usize::try_from(i) else { return };
        if let Panel::Stage(acts) = &v.panel {
            let act = acts.get(i).copied();
            v.close_panel(&app);
            if let Some(act) = act {
                v.stage_act(&app, act);
            }
            return;
        }
        if let Panel::Moves(ids) = &v.panel {
            let id = ids.get(i).copied();
            v.close_panel(&app);
            if let Some(id) = id {
                v.undo_move(&app, id);
            }
            return;
        }
        if let Panel::Bank(acts) = &v.panel {
            let act = acts.get(i).cloned();
            v.close_panel(&app);
            if let Some(act) = act {
                v.bank_act(&app, act);
            }
            return;
        }
        if let Panel::More(acts) = &v.panel {
            let act = acts.get(i).copied();
            v.close_panel(&app);
            if let Some(act) = act {
                v.more(&app, act);
            }
            return;
        }
        let id = match &v.panel {
            Panel::AddTo(ids) | Panel::New(ids) | Panel::Assign(ids) | Panel::Filter(ids) => {
                ids.get(i).copied().flatten()
            }
            Panel::Closed | Panel::More(_) | Panel::Bank(_) | Panel::Stage(_) | Panel::Moves(_) => {
                None
            }
        };
        let Some(id) = id else { return };
        match v.panel {
            Panel::AddTo(_) => v.add_to(&app, id),
            Panel::New(_) => {
                v.close_panel(&app);
                v.side_new(&app, id as i32);
            }
            _ => v.tag_row_clicked(&app, i, id),
        }
    });
}
