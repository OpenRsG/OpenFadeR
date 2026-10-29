//! Turning the source list into the visible table rows: loading the list of the chosen [`Home`], the search / filter /
//! sort pass, and the cheaper row updates (a new reference, an edited track).

use super::{
    App, BrowserState, Home, View, filter,
    table::{Look, Reference},
};
use openfader::{library::Track, store};
use slint::ComponentHandle;
use std::{
    cell::OnceCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

fn restored_track(
    t: &Track,
    collection: Option<&super::Cache>,
    source: impl FnOnce(String) -> Option<Track>,
) -> Option<Track> {
    let key = store::key(t);
    let collection = collection.and_then(|c| c.by_key.get(&key).map(|&i| &c.tracks[i]));
    if t.id.is_some() && collection.is_some() {
        return collection.cloned();
    }
    source(key).or_else(|| collection.cloned())
}

fn metadata_index<'a>(
    snapshots: &'a [Arc<Vec<Track>>],
    wanted: &HashSet<Option<PathBuf>>,
) -> HashMap<(Option<&'a Path>, String), &'a Track> {
    let mut index = HashMap::new();
    // Match paths before computing store keys: unrelated missing files must not add a reload-wide stat scan.
    for track in snapshots
        .iter()
        .flat_map(|list| list.iter())
        .filter(|t| wanted.contains(&t.file))
    {
        index
            .entry((track.file.as_deref(), store::key(track)))
            .or_insert(track);
    }
    index
}

impl View {
    /// Rebuilds `list` from the current source and refreshes the rows. The whole library is the cache's own list, shared.
    pub(super) fn load(&mut self, app: &App) {
        let (home, anchor) = (self.home.clone(), self.related.clone());
        self.slots.clear();
        let all = anchor.is_none() && matches!(home, Home::Playlist(None));
        let list = if all {
            self.cache().map(|c| c.tracks.clone())
        } else {
            self.built_list(&anchor, &home).map(Arc::new)
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
                let old = std::mem::replace(&mut self.list, list);
                self.sel.remap(&old, &self.list);
                self.rank_list(app);
                self.rows
                    .replace(self.list.clone(), self.suggest.notes.clone());
                self.refresh(app);
            }
            Err(e) => state.set_status(tr!("query failed: {}", e).into()),
        }
    }

    /// The list of every source but the whole library (that one is the cache's, shared): `anchor` = the Related track.
    fn built_list(&mut self, anchor: &Option<Track>, home: &Home) -> Result<Vec<Track>, String> {
        match (anchor, home) {
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
        }
    }

    /// Search box, filter bar, tag filter and header sort over `list`, then the rows and the status line.
    ///
    /// BPM/key facts and the full-list sort permutation are cached; filter changes only select indices from that order.
    pub(super) fn refresh(&mut self, app: &App) {
        let look = Look::now();
        let q = if self.search_entry().is_some() {
            String::new() // the box searches online there: it does not filter the results
        } else {
            self.query.to_lowercase()
        };
        let shown = self.rows.shown(
            &q,
            &self.filter,
            &self.reference,
            &look,
            self.sort.filter(|_| !self.ordered()),
            |t| super::tagging::passes(&self.tag_map, &self.tag_filter, t),
        );
        self.shown = shown;
        self.sel.retain(&self.shown);
        if self.rows.update(&self.shown, look, self.reference.clone()) {
            self.strips.reset(app, self.shown.len());
            self.covers.reset(app, self.shown.len());
        }
        let state = app.global::<BrowserState>();
        // The selection follows the track, not the row index.
        let at = self.selected.as_ref().and_then(|s| self.row_of(s));
        state.set_current_row(at.map_or(-1, |n| n as i32));
        if let Some(n) = at {
            self.sel.add(self.shown[n]); // the chosen row is always selected
        }
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
        state.set_status(status.as_str().into());
        self.status_base = status;
        self.publish_selection(app);
        state.set_tag_filter_count(self.tag_filter.len() as i32);
    }

    /// Re-marks the visible rows for a new reference: only each row's hint cell changes, in place (the selection stays).
    pub(super) fn remark(&mut self, r: Reference) {
        self.rows.remark(r.clone());
        self.reference = r;
    }

    /// A single details-panel refresh; bulk restoration below indexes its source snapshots once.
    pub(super) fn fresh_track(&mut self, t: &Track) -> Option<Track> {
        let collection = self.cache().ok();
        restored_track(t, collection.as_deref(), |key| {
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
        })
    }

    /// Undo/redo and Reload Tag change what the library lists for a track (an override removed, BPM, key, length read from the
    /// file again): every copy kept here becomes the library's current one, so it stays equal to its row in the list.
    pub(super) fn reread_metadata(&mut self) {
        let collection = self.cache().ok();
        let wanted: HashSet<_> = self
            .tags
            .iter()
            .chain(&self.history)
            .chain(self.selected.iter())
            .chain(self.related.iter())
            .map(|t| t.file.clone())
            .chain(
                self.decks
                    .iter()
                    .filter_map(|d| d.with_track(|t| t.file.clone())),
            )
            .collect();
        let snapshots = OnceCell::new();
        let index = OnceCell::new();
        let active = match self.home {
            Home::Source(i, _) => Some(i),
            _ => None,
        };
        let sources = &self.sources;
        let fresh = |t: &Track| {
            restored_track(t, collection.as_deref(), |key| {
                let snapshots = snapshots.get_or_init(|| {
                    (0..sources.len())
                        .map(|_| OnceCell::new())
                        .collect::<Vec<_>>()
                });
                let index = index.get_or_init(|| {
                    (0..sources.len())
                        .map(|_| OnceCell::new())
                        .collect::<Vec<_>>()
                });
                let identity = (t.file.as_deref(), key);
                for i in active
                    .into_iter()
                    .chain((0..sources.len()).filter(|i| Some(*i) != active))
                {
                    let Some(lib) = sources.get(i).and_then(|s| s.lib.as_ref()) else {
                        continue;
                    };
                    let Some(snapshot) = snapshots[i].get_or_init(|| lib.tracks_all().ok()) else {
                        continue;
                    };
                    let index = index[i]
                        .get_or_init(|| metadata_index(std::slice::from_ref(snapshot), &wanted));
                    if let Some(track) = index.get(&identity) {
                        return Some((*track).clone());
                    }
                }
                None
            })
        };
        let mut kept = [
            std::mem::take(&mut self.tags),
            std::mem::take(&mut self.history),
        ];
        for t in kept.iter_mut().flatten() {
            if let Some(fresh) = fresh(t) {
                *t = fresh;
            }
        }
        let [tags, history] = kept;
        (self.tags, self.history) = (tags, history);
        let mut chosen = [self.selected.take(), self.related.take()];
        for t in chosen.iter_mut().flatten() {
            if let Some(fresh) = fresh(t) {
                *t = fresh;
            }
        }
        let [selected, related] = chosen;
        (self.selected, self.related) = (selected, related);
        for i in 0..self.decks.len() {
            if let Some(t) = self.decks[i].with_track(Clone::clone)
                && let Some(fresh) = fresh(&t)
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
            Arc::make_mut(&mut self.list),
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

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
