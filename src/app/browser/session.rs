//! The browser's session state and the poll that keeps it current: the Tag List (kept in the store between runs), the
//! session history, the stored play history, and the 200 ms timer that watches the decks.

use super::{
    Home, View, history,
    sidebar::{HISTORY_ROW, TAG_ROW},
    table::reference,
};
use crate::app::{
    App, BrowserState, DeckState, analyze::detected, deck::Decks, library::settings, prefs,
};
use openfader::{
    library::Track,
    plugin::{self, source},
    store,
};
use serde_json::json;
use slint::{ComponentHandle, Model, StandardListViewItem, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

impl View {
    /// The Tag List changed: update its sidebar count and, when it is on screen, the rows.
    pub(super) fn tags_changed(&mut self, app: &App) {
        self.sidebar.set_row_data(
            TAG_ROW,
            StandardListViewItem::from(tr!("Tag List ({})", self.tags.len()).as_str()),
        );
        let keys: Vec<String> = self.tags.iter().map(store::key).collect();
        if let Err(e) = self.store.set_taglist(&keys) {
            self.say(app, tr!("store: {}", e));
        }
        if self.related.is_none() && matches!(self.home, Home::Tags) {
            self.load(app);
        }
    }

    /// The Tag List of the last run; dropped instead when Preferences > Advanced > Browse > "Delete all tracks from the Tag
    /// List when quitting" is on.
    pub(super) fn restore_tag_list(&mut self, app: &App) {
        if settings::get().taglist_quit {
            let _ = self.store.set_taglist(&[]);
            return;
        }
        let keys = self.store.taglist().unwrap_or_default();
        if let Ok(tags) = self.tracks_of(&keys, false) {
            self.tags = tags;
            self.tags_changed(app);
        }
    }

    /// A track reached 30 s on air: list it, update the sidebar count and, when shown, the rows.
    fn log(&mut self, app: &App, t: Track) {
        self.history.push(t);
        self.sidebar.set_row_data(
            HISTORY_ROW,
            StandardListViewItem::from(tr!("Session history ({})", self.history.len()).as_str()),
        );
        if self.related.is_none() && matches!(self.home, Home::History) {
            self.load(app);
        }
    }

    /// A track was on air for the stored history's threshold: append it to today (and show the new day / count).
    fn played(&mut self, app: &App, t: &Track) {
        self.mark_played(t);
        plugin::event::fire("track_played", json!({"track": source::track_json(t)}));
        if let Err(e) = history::log_today(&self.store, t) {
            return app
                .global::<BrowserState>()
                .set_status(tr!("history: {}", e).into());
        }
        self.rebuild(app);
        if self.related.is_none() && matches!(self.home, Home::Day(_)) {
            self.load(app);
        }
    }

    /// Preferences > Advanced > Browse: tracks of the Tag List that a deck just loaded leave it.
    fn drop_loaded_from_tag_list(&mut self, app: &App, fresh: &[&(String, String)]) {
        let before = self.tags.len();
        self.tags.retain(|t| {
            !fresh
                .iter()
                .any(|(title, artist)| t.title == *title && t.artist == *artist)
        });
        if self.tags.len() != before {
            self.tags_changed(app);
        }
    }
}

/// (title, artist) of every deck that has a track loaded, in deck order.
fn loaded(app: &App) -> Vec<(String, String)> {
    let s = app.global::<DeckState>();
    s.get_decks()
        .iter()
        .take(s.get_count() as usize)
        .filter(|d| d.loaded)
        .map(|d| (d.title.to_string(), d.artist.to_string()))
        .collect()
}

/// Polls the decks (a few property reads): logs what played on air, re-marks the rows (or re-filters) when the reference
/// changed, and rebuilds them when bulk analysis found new BPMs / keys. The timer lives in `view._timer`.
pub(super) fn start_poll(app: &App, view: &Rc<RefCell<View>>, decks: Decks) {
    let taglist_load = Rc::new(Cell::new(settings::get().taglist_load));
    let flag = taglist_load.clone();
    prefs::on_change(move |p| flag.set(settings::Lib::of(p).taglist_load));
    let (mut session, mut stored) = (
        history::Watcher::new(history::MIN_ON_AIR),
        history::Watcher::new(history::MIN_PLAYED),
    );
    let (weak_view, weak) = (Rc::downgrade(view), app.as_weak());
    let (mut ticks, mut seen) = (0u32, Vec::<(String, String)>::new());
    view.borrow()
        ._timer
        .start(TimerMode::Repeated, Duration::from_millis(200), move || {
            let (Some(v), Some(app)) = (weak_view.upgrade(), weak.upgrade()) else {
                return;
            };
            let (played, r, found) = (
                session.poll(&app, &decks),
                reference(&app),
                detected().version(),
            );
            let stored = stored.poll(&app, &decks);
            let mut v = v.borrow_mut();
            let now = loaded(&app);
            let fresh: Vec<&(String, String)> = now.iter().filter(|n| !seen.contains(n)).collect();
            if taglist_load.get() && !fresh.is_empty() {
                v.drop_loaded_from_tag_list(&app, &fresh);
            }
            seen = now;
            ticks = ticks.wrapping_add(1);
            if ticks % 5 == 0 {
                v.refresh_sources(&app); // devices plugged / unplugged, Media Browser settings
            }
            for t in played {
                v.log(&app, t);
            }
            for t in &stored {
                v.played(&app, t);
            }
            if v.detected_ver != found {
                (v.detected_ver, v.reference) = (found, r);
                if prefs::with(|p| p.view.panel_cue_analysis) {
                    v.rebuild(&app); // the CUE Analysis count
                }
                if v.related.is_none() && matches!(v.home, Home::CueAnalysis) {
                    v.load(&app);
                } else {
                    v.refresh(&app);
                }
            } else if v.reference != r {
                if v.filter.uses_reference() {
                    v.reference = r;
                    v.refresh(&app);
                } else {
                    v.remark(r);
                }
            }
        });
}
