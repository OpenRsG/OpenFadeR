//! The store laid over a [`Library`]: tracks carry the store's rating and comment, and a track's
//! analysis carries the store's cue list and beat grid. The store wins; what it has no edit for stays
//! the library's. Playlists are the library's own (user playlists are read from the store by the browser).

use super::{Edits, Store, global, key, track::Fields};
use crate::{
    analysis::Analysis,
    library::{Library, Playlist, Track},
};
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};

/// `inner` seen through the process-wide [`global`] store's edits.
pub fn overlay(inner: Box<dyn Library>) -> Box<dyn Library> {
    Box::new(Overlay::new(inner, global()))
}

/// Rating / comment edits by track: a Collection track's by its id (no key string built per track on a 100k listing), the
/// rest by [`key`].
#[derive(Default)]
struct Edited {
    by_id: HashMap<i64, Fields>,
    by_key: HashMap<String, Fields>,
}

impl Edited {
    fn of(fields: HashMap<String, Fields>) -> Self {
        let mut e = Edited::default();
        for (k, f) in fields {
            match id_of_key(&k) {
                Some(id) => e.by_id.insert(id, f).map(drop),
                None => e.by_key.insert(k, f).map(drop),
            };
        }
        e
    }

    fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_key.is_empty()
    }

    fn get(&self, t: &Track) -> Option<&Fields> {
        if let Some(id) = t.id {
            return self.by_id.get(&id);
        }
        let k = key(t);
        match id_of_key(&k) {
            Some(id) => self.by_id.get(&id),
            None => self.by_key.get(&k),
        }
    }
}

impl Edited {
    /// Puts the edited rating and comment on the tracks they belong to.
    fn apply(&self, tracks: &mut [Track]) {
        for t in tracks {
            let Some(f) = self.get(t) else { continue };
            if let Some(r) = f.rating {
                t.rating = r;
            }
            if let Some(c) = &f.comment {
                t.comment.clone_from(c);
            }
        }
    }
}

/// The library's list, the edits it was merged with and the result.
type Merged = (Arc<Vec<Track>>, Arc<Edited>, Arc<Vec<Track>>);

/// `42` of the key `t42` ([`super::id_key`]).
fn id_of_key(k: &str) -> Option<i64> {
    k.strip_prefix('t')?.parse().ok()
}

pub struct Overlay {
    inner: Box<dyn Library>,
    store: Arc<Store>,
    /// The store's rating / comment edits as of the [`Store::generation`] in `.0`.
    fields: Mutex<Option<(u64, Arc<Edited>)>>,
    /// [`Library::tracks_all`] of `inner` with those edits on: valid while both `Arc`s are the ones it was made from.
    merged: Mutex<Option<Merged>>,
}

impl Overlay {
    pub fn new(inner: Box<dyn Library>, store: Arc<Store>) -> Self {
        Self {
            inner,
            store,
            fields: Mutex::new(None),
            merged: Mutex::new(None),
        }
    }

    /// The store's rating / comment edits, read again only when the store changed since the last call.
    fn fields(&self) -> Arc<Edited> {
        let generation = self.store.generation();
        let mut c = self.fields.lock();
        if let Some((g, f)) = &*c
            && *g == generation
        {
            return f.clone();
        }
        let f = Arc::new(Edited::of(self.store.track_fields().unwrap_or_else(|e| {
            eprintln!("store: {e}");
            Default::default()
        })));
        *c = Some((generation, f.clone()));
        f
    }
}

/// Puts the stored cue list, grid and phrases onto `a`; an unedited part is left alone. A stored phrase list replaces the analysed
/// one even when empty (CLEAR): `Some` with no phrases, which [`crate::analysis::auto::fill`] leaves alone.
pub fn apply(a: &mut Analysis, e: &Edits) {
    if let Some(c) = &e.cues {
        a.cues.clone_from(c);
    }
    if let Some(g) = &e.grid {
        a.beats.clone_from(g);
    }
    if let Some(p) = &e.phrases {
        a.phrases = Some(p.clone());
    }
}

impl Library for Overlay {
    fn kind(&self) -> &str {
        self.inner.kind()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        self.inner.playlists()
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let mut tracks = self.inner.tracks(playlist)?;
        let fields = self.fields();
        if !fields.is_empty() {
            fields.apply(&mut tracks);
        }
        Ok(tracks)
    }

    /// The library's list with the edits on, copied once per change of the library or the store, not per call.
    fn tracks_all(&self) -> Result<Arc<Vec<Track>>, String> {
        let (inner, fields) = (self.inner.tracks_all()?, self.fields());
        if fields.is_empty() {
            return Ok(inner);
        }
        let mut merged = self.merged.lock();
        if let Some((i, f, m)) = &*merged
            && Arc::ptr_eq(i, &inner)
            && Arc::ptr_eq(f, &fields)
        {
            return Ok(m.clone());
        }
        let mut tracks = (*inner).clone();
        fields.apply(&mut tracks);
        let all = Arc::new(tracks);
        *merged = Some((inner, fields, all.clone()));
        Ok(all)
    }

    /// The library's analysis with the store's cues / grid / phrases on top; the library's own answer, unchanged, for a
    /// track the store has no such edit for.
    fn analysis(&self, track: &Track) -> Option<Analysis> {
        let edits = self.store.edits(&key(track)).unwrap_or_else(|e| {
            eprintln!("store: {e}");
            Edits::default()
        });
        if edits.cues.is_none() && edits.grid.is_none() && edits.phrases.is_none() {
            return self.inner.analysis(track);
        }
        let mut a = self.inner.analysis(track).unwrap_or_default();
        apply(&mut a, &edits);
        Some(a)
    }

    fn analysis_reader(&self) -> Option<crate::library::AnalysisReader> {
        self.inner.analysis_reader()
    }
}
