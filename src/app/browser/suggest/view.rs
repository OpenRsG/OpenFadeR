//! The SUGGEST tab: its state, the track the list is ranked for, and the table rows it decorates (`BrowserState.suggest-*` in
//! ui/browser-state.slint, the bar in ui/browser-suggest.slint). The list is what the tree shows (the search target); ranking
//! reorders it in [`View::rank_list`], which [`View::load`] calls after every rebuild.
//!
//! The anchor ([`Anchor`]): LIST = the row that was chosen when LIST was pressed (it stays put while rows of the ranked list are
//! clicked, otherwise every click would re-rank), MASTER / a deck number = the track on that deck, followed live by a timer.
//! Features come from a background index ([`openfader::analysis::similar::index`]): the list ranks with what is ready and
//! re-ranks as more arrives; tracks the index has no analysis for are counted in the status, not guessed.

use super::super::View;
use super::{Conditions, Item, Links, Mode, max_list, rank};
use crate::app::{App, BrowserState, DeckState, deck, plural, prefs};
use openfader::{
    analysis::similar::{Profile, Theirs, index},
    library::Track,
    store::{self, NodeKind},
};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

/// How often the follow timer looks for a new anchor or new index results.
const TICK: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    List,
    Master,
    Deck(usize),
}

impl Anchor {
    /// Position among `suggest-anchors` (LIST, MASTER, 1, 2, ...).
    fn index(self) -> usize {
        match self {
            Anchor::List => 0,
            Anchor::Master => 1,
            Anchor::Deck(d) => d + 2,
        }
    }

    fn of(index: usize) -> Anchor {
        match index {
            0 => Anchor::List,
            1 => Anchor::Master,
            n => Anchor::Deck(n - 2),
        }
    }
}

pub struct State {
    pub on: bool,
    mode: Mode,
    anchor: Anchor,
    cond: Conditions,
    /// The LIST anchor.
    pinned: Option<Track>,
    /// Table note per row of `View.list` while it is ranked; empty otherwise.
    pub(in crate::app::browser) notes: Vec<String>,
    status: String,
    /// What the list was last ranked for (anchor id, index version): the timer re-ranks when either moves.
    seen: (String, u64),
    /// Playlist / history company of one anchor (by store key), kept until the anchor changes.
    links: Option<(String, Rc<Links>)>,
    /// Queue the list's tracks again in the index, also the ones that had no features (the mode or the anchor changed).
    retry: bool,
    timer: Timer,
}

impl Default for State {
    fn default() -> Self {
        State {
            on: false,
            mode: Mode::Radar,
            anchor: Anchor::Master,
            cond: Conditions::default(),
            pinned: None,
            notes: Vec::new(),
            status: String::new(),
            seen: (String::new(), 0),
            links: None,
            retry: false,
            timer: Timer::default(),
        }
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Radar => "Collection Radar",
        Mode::Era => "Era",
        Mode::Mood => "Mood",
        Mode::Association => "Association",
    }
}

/// What the mode could not judge a track for.
fn lacks(m: Mode) -> &'static str {
    match m {
        Mode::Era => "a year tag",
        _ => "analysis data",
    }
}

fn id_of(t: &Track) -> String {
    format!("{:?}|{}", t.file, t.title)
}

impl View {
    fn deck_with<R>(&self, i: usize, f: impl FnOnce(&Track) -> R) -> Option<R> {
        self.decks.get(i)?.with_track(f)
    }

    /// `f` applied to the track the list is ranked for, without cloning it.
    fn anchor_with<R>(&self, app: &App, f: impl Fn(&Track) -> R) -> Option<R> {
        match self.suggest.anchor {
            Anchor::List => self.suggest.pinned.as_ref().map(f),
            Anchor::Deck(i) => self.deck_with(i, f),
            Anchor::Master => {
                let master = usize::try_from(app.global::<DeckState>().get_master()).ok();
                master
                    .and_then(|m| self.deck_with(m, &f))
                    .or_else(|| {
                        (0..deck::active().min(self.decks.len()))
                            .find_map(|i| self.deck_with(i, &f))
                    })
                    .or_else(|| self.suggest.pinned.as_ref().map(&f)) // no deck loaded: the row that was chosen
            }
        }
    }

    /// The track the list is ranked for.
    fn suggest_anchor(&self, app: &App) -> Option<Track> {
        self.anchor_with(app, Track::clone)
    }

    /// Where the Association mode looks for tracks that sit together: My Playlists, library playlists and histories, the stored
    /// play history and this session's.
    // ponytail: reads every playlist per new anchor; keep the lists if a library has thousands of them.
    fn link_lists(&self) -> Vec<Vec<String>> {
        let keys = |ts: &[Track]| ts.iter().map(store::key).collect::<Vec<_>>();
        let mut lists: Vec<Vec<String>> = self
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Playlist)
            .filter_map(|n| self.store.entries(n.id).ok())
            .collect();
        for day in self.store.history_days().unwrap_or_default() {
            lists.extend(self.store.history_day(&day).ok());
        }
        lists.push(keys(&self.history));
        let local = |p: &&openfader::library::Playlist| {
            !p.is_folder && !openfader::plugin::source::is_plugin(&p.id) // a plugin list would ask its plugin
        };
        for p in self.lib_playlists.iter().filter(local) {
            lists.extend(self.lib.tracks(Some(&p.id)).ok().map(|t| keys(&t)));
        }
        lists
    }

    fn links_for(&mut self, anchor_key: &str) -> Rc<Links> {
        let limit = max_list(self.list.len());
        let id = format!("{anchor_key}|{limit}");
        if let Some((k, l)) = &self.suggest.links {
            if *k == id {
                return l.clone();
            }
        }
        let l = Rc::new(Links::from_lists(anchor_key, &self.link_lists(), limit));
        self.suggest.links = Some((id, l.clone()));
        l
    }

    /// Pushes the tab's state to the Slint globals.
    fn publish_suggest(&self, app: &App) {
        let s = app.global::<BrowserState>();
        let anchors: Vec<SharedString> = ["LIST".to_owned(), "MASTER".to_owned()]
            .into_iter()
            .chain((1..=deck::active()).map(|n| n.to_string()))
            .map(Into::into)
            .collect();
        s.set_suggest(self.suggest.on && self.related.is_none());
        s.set_suggest_mode(self.suggest.mode.index() as i32);
        s.set_suggest_anchors(ModelRc::from(Rc::new(VecModel::from(anchors))));
        s.set_suggest_anchor(self.suggest.anchor.index() as i32);
        let chips: Vec<SharedString> = self
            .suggest
            .cond
            .labels()
            .into_iter()
            .map(Into::into)
            .collect();
        s.set_suggest_conditions(ModelRc::from(Rc::new(VecModel::from(chips))));
    }

    /// Reorders `self.list` by the chosen mode when the SUGGEST tab is on (called by `load`, before the rows are built).
    pub(in crate::app::browser) fn rank_list(&mut self, app: &App) {
        self.suggest.notes.clear();
        self.publish_suggest(app);
        if !self.suggest.on || self.related.is_some() {
            return;
        }
        self.slots.clear();
        let mode = self.suggest.mode;
        let ix = index();
        let version = ix.version();
        let Some(anchor) = self.suggest_anchor(app) else {
            self.list = Rc::default();
            self.suggest.seen = (String::new(), version);
            self.suggest.status = match self.suggest.anchor {
                Anchor::List => "Track Suggestion: click a track, then press LIST".to_owned(),
                _ => "Track Suggestion: load a track on a deck, or click a track and press LIST"
                    .to_owned(),
            };
            return;
        };
        self.suggest.seen = (id_of(&anchor), version);
        let retry = std::mem::take(&mut self.suggest.retry);
        let reader = self.lib.analysis_reader();
        let file = |t: &Track| {
            let theirs = reader.clone().map(|r| {
                let t = t.clone();
                Box::new(move || r(&t)) as Theirs
            });
            Some((t.file.clone()?, theirs))
        };
        ix.request(self.list.iter().chain([&anchor]).filter_map(file), retry);
        let akey = store::key(&anchor);
        let links = if mode == Mode::Association {
            self.links_for(&akey)
        } else {
            Rc::default()
        };
        let by_file = |t: &Track| t.file.as_deref().and_then(|f| ix.get(f));
        let profiles: Vec<Option<Arc<Profile>>> = self.list.iter().map(by_file).collect();
        let keys: Vec<String> = self.list.iter().map(store::key).collect();
        let anchor_profile = by_file(&anchor);
        let out = {
            let pool: Vec<Item> = self
                .list
                .iter()
                .zip(&profiles)
                .zip(&keys)
                .map(|((t, p), k)| Item::new(t, p.as_deref(), k))
                .collect();
            rank(
                mode,
                &Item::new(&anchor, anchor_profile.as_deref(), &akey),
                &pool,
                &self.suggest.cond,
                &links,
            )
        };
        let (done, total) = ix.progress();
        let mut status = match out.blocked {
            Some(why) => format!("{} for “{}”: {why}", mode_name(mode), anchor.title),
            None => format!(
                "{} track{} suggested for “{}” by {}",
                out.picks.len(),
                plural(out.picks.len()),
                anchor.title,
                mode_name(mode)
            ),
        };
        if out.picks.is_empty() && out.blocked.is_none() && mode == Mode::Association {
            status += " · no shared label, composer, artist, album or playlist";
        }
        if out.no_data > 0 {
            status += &format!(" · {} without {}", out.no_data, lacks(mode));
        }
        if ix.pending() > 0 {
            status += &format!(" · reading {done}/{total}");
        }
        self.suggest.status = status;
        self.suggest.notes = out.picks.iter().map(|p| p.note.clone()).collect();
        self.list = Rc::new(out.picks.iter().map(|p| self.list[p.at].clone()).collect());
    }

    /// The status line of the ranked list (`None` when the tab is off).
    pub(in crate::app::browser) fn suggest_status(&self) -> Option<&str> {
        (self.suggest.on && self.related.is_none()).then_some(self.suggest.status.as_str())
    }

    /// A row of the ranked list was chosen: say why it ranks there.
    pub(in crate::app::browser) fn suggest_selected(&self, app: &App) {
        let at = self
            .selected
            .as_ref()
            .and_then(|s| self.list.iter().position(|t| t == s));
        if let Some(note) = at
            .and_then(|i| self.suggest.notes.get(i).map(|n| (i, n)))
            .filter(|_| self.suggest_status().is_some())
        {
            self.say(app, format!("#{} · {}", note.0 + 1, note.1));
        }
    }

    /// The SUGGEST tab turned on or off: Related and the tab are exclusive; LIST pins the chosen row.
    fn suggest_toggle(&mut self, app: &App, on: bool) {
        let on = on && prefs::with(|p| p.view.media_track_suggestion);
        if on {
            self.related = None;
            self.suggest.pinned = self.selected.clone().or(self.suggest.pinned.take());
        }
        (self.suggest.on, self.suggest.retry, self.suggest.links) = (on, on, None);
        self.load(app);
    }

    /// Turns the tab off without reloading (Related takes the list).
    pub(in crate::app::browser) fn suggest_off(&mut self) {
        self.suggest.on = false;
    }

    /// A chip changed what the ranking depends on: rank again.
    fn suggest_changed(&mut self, app: &App) {
        self.suggest.retry = true;
        if self.suggest.on {
            self.load(app);
        } else {
            self.publish_suggest(app);
        }
    }
}

/// Wires the SUGGEST tab's callbacks, the Media Browser setting and the follow timer.
pub fn wire(app: &App, view: &Rc<RefCell<View>>) {
    let state = app.global::<BrowserState>();
    state.set_suggest_enabled(prefs::with(|p| p.view.media_track_suggestion));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_suggest_toggled(move |on| v.borrow_mut().suggest_toggle(&weak.unwrap(), on));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_suggest_mode_chosen(move |i| {
        let mut v = v.borrow_mut();
        v.suggest.mode = Mode::ALL[usize::try_from(i).unwrap_or(0).min(Mode::ALL.len() - 1)];
        v.suggest_changed(&weak.unwrap());
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_suggest_anchor_chosen(move |i| {
        let mut v = v.borrow_mut();
        v.suggest.anchor = Anchor::of(usize::try_from(i).unwrap_or(0));
        if v.suggest.anchor == Anchor::List {
            v.suggest.pinned = v.selected.clone().or(v.suggest.pinned.take());
        }
        v.suggest.links = None;
        v.suggest_changed(&weak.unwrap());
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_suggest_condition(move |i| {
        let mut v = v.borrow_mut();
        v.suggest.cond.step(usize::try_from(i).unwrap_or(9));
        v.suggest_changed(&weak.unwrap());
    });
    let (v, weak) = (view.clone(), app.as_weak());
    prefs::on_change(move |p| {
        let Some(app) = weak.upgrade() else { return };
        app.global::<BrowserState>()
            .set_suggest_enabled(p.view.media_track_suggestion);
        let mut v = v.borrow_mut();
        if !p.view.media_track_suggestion && v.suggest.on {
            v.suggest_toggle(&app, false); // the tab vanished with its setting
        }
    });
    // Follow the anchor (a deck's track changes) and the index (more analysis arrived). New results re-rank at most every
    // third tick while the index is still reading, so a long queue does not rebuild the table ten times a second.
    let (weak_view, weak, mut ticks) = (Rc::downgrade(view), app.as_weak(), 0u32);
    view.borrow()
        .suggest
        .timer
        .start(TimerMode::Repeated, TICK, move || {
            let (Some(v), Some(app)) = (weak_view.upgrade(), weak.upgrade()) else {
                return;
            };
            let mut v = v.borrow_mut();
            if !v.suggest.on || v.related.is_some() {
                return;
            }
            ticks = ticks.wrapping_add(1);
            let anchor = v.anchor_with(&app, id_of).unwrap_or_default();
            let ix = index();
            let more = ix.version() != v.suggest.seen.1 && (ix.pending() == 0 || ticks % 3 == 0);
            if anchor != v.suggest.seen.0 || more {
                v.load(&app);
            }
        });
}
