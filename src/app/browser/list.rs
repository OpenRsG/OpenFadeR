//! Turning the source list into the visible table rows: loading the list of the chosen [`Home`], the search / filter /
//! sort pass, and the cheaper row updates (a new reference, an edited track).

use super::{
    filter,
    table::{self, Facts, Look, Reference},
    App, BrowserState, Home, View,
};
use openfader::{library::Track, store};
use slint::{ComponentHandle, Model};

impl View {
    /// Rebuilds `list` from the current source and refreshes the rows.
    // ponytail: the whole-library list is cloned out of the cache per view change; share it (Arc) if that shows up in profiles.
    pub(super) fn load(&mut self, app: &App) {
        let (home, anchor) = (self.home.clone(), self.related.clone());
        self.slots.clear();
        let list = match (&anchor, &home) {
            (Some(a), _) => self.cache().map(|all| {
                let partners = self.store.matches_of(&store::key(a)).unwrap_or_default();
                filter::related_with(a, &all.tracks, &partners, &Look::now())
                    .into_iter()
                    .cloned()
                    .collect()
            }),
            (None, Home::Nothing) => Ok(Vec::new()),
            (None, Home::Tags) => Ok(self.tags.clone()),
            (None, Home::History) => Ok(self.history.clone()),
            (None, Home::Playlist(None)) => self.cache().map(|c| c.tracks.to_vec()),
            (None, Home::Playlist(id)) => self.lib.tracks(id.as_deref()),
            (None, Home::User(id)) => self.user_list(*id),
            (None, Home::Smart(id)) => self.smart_list(*id),
            (None, Home::Day(day)) => self.day_list(day),
            (None, Home::Tag(id)) => self.mytag_list(*id),
            (None, Home::Dupes) => self.dupes_list(),
            (None, Home::Bank(id)) => self.bank_list(*id),
            (None, Home::CueAnalysis) => self.cue_analysis_list(),
            (None, Home::Source(s, id)) => self
                .sources
                .get(*s)
                .and_then(|s| s.lib.clone())
                .ok_or_else(|| "the source is not open".to_owned())
                .and_then(|l| l.tracks(id.as_deref())),
        };
        let state = app.global::<BrowserState>();
        let plain = anchor.is_none();
        let editing = plain && !self.suggest.on; // a ranked list is no longer in the order its edit buttons act on
        state.set_related(!plain);
        state.set_tag_view(editing && matches!(home, Home::Tags));
        state.set_user_view(editing && matches!(home, Home::User(_) | Home::Bank(_)));
        state.set_history_view(editing && matches!(home, Home::History | Home::Day(_)));
        state.set_remote_search(self.search_entry().is_some());
        match list {
            Ok(list) => {
                self.list = list;
                self.rank_list(app);
                self.refresh(app);
            }
            Err(e) => state.set_status(format!("query failed: {e}").into()),
        }
    }

    /// Search box, filter bar, tag filter and header sort over `list`, then the rows and the status line.
    ///
    /// BPM and key are resolved once per track ([`Facts`]) and the search compares without lowercasing each field
    /// ([`filter::shown`], [`filter::sort`]).
    pub(super) fn refresh(&mut self, app: &App) {
        self.look = Look::now();
        let look = self.look;
        let q = if self.search_entry().is_some() {
            String::new() // the box searches online there: it does not filter the results
        } else {
            self.query.to_lowercase()
        };
        let facts: Vec<Facts> = self.list.iter().map(Facts::of).collect();
        self.shown = filter::shown(
            &self.list,
            &facts,
            &q,
            &self.filter,
            &self.reference,
            &look,
            |t| super::tagging::passes(&self.tag_map, &self.tag_filter, t),
        );
        if let Some((col, asc)) = self.sort.filter(|_| !self.ordered()) {
            filter::sort(&mut self.shown, &self.list, &facts, col, asc, &look);
        }
        let rows: Vec<_> = self
            .shown
            .iter()
            .map(|&i| self.table_row(i, &facts[i]))
            .collect();
        self.rows.set_vec(rows);
        self.strips.reset(self.shown.len());
        let state = app.global::<BrowserState>();
        // The selection follows the track, not the row index.
        let at = self.selected.as_ref().and_then(|s| self.row_of(s));
        state.set_current_row(at.map_or(-1, |n| n as i32));
        let mut status = match (&self.related, &self.home, self.suggest_status()) {
            (Some(a), ..) => format!(
                "{} tracks relate to “{}” (registered partners, then compatible key, BPM ±6%)",
                self.shown.len(),
                a.title
            ),
            (None, Home::Nothing, _) => self.entry_hint().to_owned(),
            (None, _, Some(s)) => s.to_owned(),
            (None, Home::Bank(_), _) => format!(
                "{} of 8 banks, A first · MORE adds a cue from a deck or the chosen track, loads the list onto a deck",
                self.list.len()
            ),
            (None, Home::Dupes, _) => format!(
                "{} tracks share title, artist and length (±1 s) with another one, in {} groups",
                self.list.len(),
                self.dupe_groups
            ),
            (None, ..) => format!("{} of {} tracks", self.shown.len(), self.list.len()),
        };
        if self.filter.ignored(&self.reference) {
            status += " · BPM ±6% / KEY MATCH wait for a loaded deck";
        }
        state.set_status(status.into());
        state.set_tag_filter_count(self.tag_filter.len() as i32);
    }

    /// Re-marks the visible rows for a new reference: only each row's hint cell changes, in place (the selection stays).
    pub(super) fn remark(&mut self, r: Reference) {
        self.reference = r;
        for (n, &i) in self.shown.iter().enumerate() {
            if let Some(row) = self.rows.row_data(n) {
                table::remark(&row, &self.list[i], &self.reference, &self.look);
            }
        }
    }

    /// The track panel edited a rating / comment: every copy of that track here shows the new values.
    pub(super) fn track_edited(&mut self, app: &App, edited: &Track) {
        let key = store::key(edited);
        let put = |t: &mut Track| {
            if store::key(t) == key {
                (t.rating, t.comment) = (edited.rating, edited.comment.clone());
            }
        };
        [&mut self.list, &mut self.tags, &mut self.history]
            .into_iter()
            .flatten()
            .for_each(put);
        self.selected.iter_mut().for_each(put);
        self.cache = None;
        if matches!(self.home, Home::Smart(_)) && self.related.is_none() {
            self.load(app);
        } else {
            self.refresh(app);
        }
    }
}
