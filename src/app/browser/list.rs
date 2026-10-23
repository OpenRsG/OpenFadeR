//! Turning the source list into the visible table rows: loading the list of the chosen [`Home`], the search / filter /
//! sort pass, and the cheaper row updates (a new reference, an edited track).

use super::{
    App, BrowserState, Home, View, filter,
    table::{Look, Reference},
};
use openfader::{library::Track, store};
use slint::ComponentHandle;

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
            (None, Home::Health(c)) => self.health_list(*c),
            (None, Home::Bank(id)) => self.bank_list(*id),
            (None, Home::Stage(id)) => self.store.stage_tracks(*id),
            (None, Home::CueAnalysis) => self.cue_analysis_list(),
            (None, Home::Source(s, id)) => self
                .sources
                .get(*s)
                .and_then(|s| s.lib.clone())
                .ok_or_else(|| tr!("the source is not open").to_owned())
                .and_then(|l| l.tracks(id.as_deref())),
        };
        let state = app.global::<BrowserState>();
        let plain = anchor.is_none();
        let editing = plain && !self.suggest.on; // a ranked list is no longer in the order its edit buttons act on
        state.set_related(!plain);
        state.set_tag_view(editing && matches!(home, Home::Tags));
        state.set_user_view(editing && matches!(home, Home::User(_) | Home::Bank(_)));
        state.set_history_view(editing && matches!(home, Home::History | Home::Day(_)));
        state.set_health_view(editing && matches!(home, Home::Health(_)));
        state.set_remote_search(self.search_entry().is_some());
        match list {
            Ok(list) => {
                self.list = std::rc::Rc::new(list);
                self.rank_list(app);
                self.rows
                    .replace(self.list.clone(), self.suggest.notes.clone());
                self.refresh(app);
            }
            Err(e) => state.set_status(tr!("query failed: {}", e).into()),
        }
    }

    /// Search box, filter bar, tag filter and header sort over `list`, then the rows and the status line.
    ///
    /// BPM and key are cached per track ([`super::table::Facts`]) and the search compares without lowercasing each field
    /// ([`filter::shown`], [`filter::sort`]).
    pub(super) fn refresh(&mut self, app: &App) {
        let look = Look::now();
        let q = if self.search_entry().is_some() {
            String::new() // the box searches online there: it does not filter the results
        } else {
            self.query.to_lowercase()
        };
        let shown = {
            let facts = self.rows.facts();
            let mut shown = filter::shown(
                &self.list,
                &facts,
                &q,
                &self.filter,
                &self.reference,
                &look,
                |t| super::tagging::passes(&self.tag_map, &self.tag_filter, t),
            );
            if let Some((col, asc)) = self.sort.filter(|_| !self.ordered()) {
                filter::sort(&mut shown, &self.list, &facts, col, asc, &look);
            }
            shown
        };
        self.shown = shown;
        if self.rows.update(&self.shown, look, self.reference.clone()) {
            self.strips.reset(app, self.shown.len());
            self.covers.reset(app, self.shown.len());
        }
        let state = app.global::<BrowserState>();
        // The selection follows the track, not the row index.
        let at = self.selected.as_ref().and_then(|s| self.row_of(s));
        state.set_current_row(at.map_or(-1, |n| n as i32));
        let mut status = match (&self.related, &self.home, self.suggest_status()) {
            (Some(a), ..) => tr!(
                "{} tracks relate to “{}” (registered partners, then compatible key, BPM ±6%)",
                self.shown.len(),
                a.title
            ),
            (None, Home::Nothing, _) => self.entry_hint().to_owned(),
            (None, _, Some(s)) => s.to_owned(),
            (None, Home::Bank(_), _) => tr!(
                "{} of 8 banks, A first · MORE adds a cue from a deck or the chosen track, loads the list onto a deck",
                self.list.len()
            ),
            (None, Home::Stage(_), _) => tr!(
                "{} of {} tracks waiting · drag one onto the next stage, or MORE moves the chosen or all shown tracks",
                self.shown.len(),
                self.list.len()
            ),
            (None, Home::Dupes, _) => tr!(
                "{} tracks share title, artist and length (±1 s) with another one, in {} groups",
                self.list.len(),
                self.dupe_groups
            ),
            (None, Home::Health(c), _) => self.health_status(*c),
            (None, ..) => tr!("{} of {} tracks", self.shown.len(), self.list.len()),
        };
        if self.filter.ignored(&self.reference) {
            status += tr!(" · BPM ±6% / KEY MATCH wait for a loaded deck");
        }
        state.set_status(status.into());
        state.set_tag_filter_count(self.tag_filter.len() as i32);
    }

    /// Re-marks the visible rows for a new reference: only each row's hint cell changes, in place (the selection stays).
    pub(super) fn remark(&mut self, r: Reference) {
        self.rows.remark(r.clone());
        self.reference = r;
    }

    // ponytail: source snapshots are scanned per refreshed track; index them if large source playlists make undo lag.
    pub(super) fn fresh_track(&mut self, t: &Track) -> Option<Track> {
        let key = store::key(t);
        let collection = self
            .cache()
            .ok()
            .and_then(|c| c.by_key.get(&key).map(|&i| c.tracks[i].clone()));
        if t.id.is_some() && collection.is_some() {
            return collection;
        }
        let find = |lib: &dyn openfader::library::Library| {
            lib.tracks_all()
                .ok()?
                .iter()
                .find(|fresh| fresh.file == t.file && store::key(fresh) == key)
                .cloned()
        };
        if let Home::Source(i, _) = self.home
            && let Some(fresh) = self
                .sources
                .get(i)
                .and_then(|s| s.lib.as_ref())
                .and_then(|lib| find(&**lib))
        {
            return Some(fresh);
        }
        self.sources
            .iter()
            .filter_map(|s| s.lib.as_ref())
            .find_map(|lib| find(&**lib))
            .or(collection)
    }

    /// Undo/redo and Reload Tag change what the library lists for a track (an override removed, BPM, key, length read from the
    /// file again): every copy kept here becomes the library's current one, so it stays equal to its row in the list.
    pub(super) fn reread_metadata(&mut self) {
        let mut kept = [
            std::mem::take(&mut self.tags),
            std::mem::take(&mut self.history),
        ];
        for t in kept.iter_mut().flatten() {
            if let Some(fresh) = self.fresh_track(t) {
                *t = fresh;
            }
        }
        let [tags, history] = kept;
        (self.tags, self.history) = (tags, history);
        let mut chosen = [self.selected.take(), self.related.take()];
        for t in chosen.iter_mut().flatten() {
            if let Some(fresh) = self.fresh_track(t) {
                *t = fresh;
            }
        }
        let [selected, related] = chosen;
        (self.selected, self.related) = (selected, related);
        for i in 0..self.decks.len() {
            if let Some(t) = self.decks[i].with_track(Clone::clone)
                && let Some(fresh) = self.fresh_track(&t)
            {
                self.decks[i].refresh_metadata(&fresh);
            }
            self.decks[i].reread_edits(&*self.lib);
        }
    }

    /// The track editor changed metadata: every copy here uses the same durable overrides.
    pub(super) fn track_edited(&mut self, app: &App, edited: &Track) {
        let key = store::key(edited);
        let put = |t: &mut Track| {
            if store::key(t) == key {
                crate::app::trackinfo::copy_metadata(t, edited);
            }
        };
        [
            std::rc::Rc::make_mut(&mut self.list),
            &mut self.tags,
            &mut self.history,
        ]
        .into_iter()
        .flatten()
        .for_each(put);
        self.selected.iter_mut().for_each(put);
        self.related.iter_mut().for_each(put);
        for deck in &self.decks {
            deck.refresh_metadata(edited);
        }
        self.cache = None;
        self.rows
            .replace(self.list.clone(), self.suggest.notes.clone());
        if matches!(self.home, Home::Smart(_)) && self.related.is_none() {
            self.load(app);
        } else {
            self.refresh(app);
        }
    }
}
