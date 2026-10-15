//! The store laid over a [`Library`]: tracks carry the store's rating and comment, and a track's
//! analysis carries the store's cue list and beat grid. The store wins; what it has no edit for stays
//! the library's. Playlists are the library's own (user playlists are read from the store by the browser).

use super::{Edits, Store, file_name, global, key, name_of};
use crate::{
    analysis::Analysis,
    library::{Library, Playlist, Track},
};
use std::{collections::HashSet, sync::Arc};

/// `inner` seen through the process-wide [`global`] store: the Collection ([`crate::library::collection`]) over the source,
/// then the store's edits.
pub fn overlay(inner: Box<dyn Library>) -> Box<dyn Library> {
    let store = global();
    Box::new(Overlay::new(
        Box::new(crate::library::collection::Collection::new(
            inner,
            store.clone(),
        )),
        store,
    ))
}

pub struct Overlay {
    inner: Box<dyn Library>,
    store: Arc<Store>,
}

impl Overlay {
    pub fn new(inner: Box<dyn Library>, store: Arc<Store>) -> Self {
        Self { inner, store }
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
        let fields = self.store.track_fields().unwrap_or_else(|e| {
            eprintln!("store: {e}");
            Default::default()
        });
        // Look the store up by file name first so only tracks that may have edits are stat'ed for their key.
        let names: HashSet<&str> = fields.keys().map(|k| name_of(k)).collect();
        for t in &mut tracks {
            let Some(f) = t
                .file
                .as_deref()
                .map(file_name)
                .filter(|n| names.contains(&**n))
                .and_then(|_| fields.get(&key(t)))
            else {
                continue;
            };
            if let Some(r) = f.rating {
                t.rating = r;
            }
            if let Some(c) = &f.comment {
                t.comment.clone_from(c);
            }
        }
        Ok(tracks)
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
