//! Our own Collection as the [`Library`] the app opens: every row of the `collection` table, each with its `id`. Other
//! libraries and folders are connections synced into it (`library::connect`), never opened as the library. User playlists
//! are the store's node tree, not this library's, so it lists none.
//!
//! Its tracks carry the store's text and rating edits ([`Store::collection_edited`]).
//!
//! Analysis: the plugin analysis a row's `analysis_ref` names ([`super::load_ref`]); a row without one has none here and
//! the analysis layer falls back to our own.

use super::{AnalysisReader, Library, Playlist, Track};
use crate::{analysis::Analysis, store::Store};
use std::sync::Arc;

pub struct Own {
    store: Arc<Store>,
}

impl Own {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

impl Library for Own {
    fn kind(&self) -> &str {
        "Collection"
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(Vec::new())
    }

    /// All rows for `None`; a playlist query is empty (the browser reads user playlists from the store).
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        match playlist {
            None => Ok(self.store.collection_edited()?.to_vec()),
            Some(_) => Ok(Vec::new()),
        }
    }

    fn tracks_all(&self) -> Result<Arc<Vec<Track>>, String> {
        self.store.collection_edited()
    }

    fn edited(&self) -> bool {
        true
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        super::load_ref(track)
    }

    fn analysis_reader(&self) -> Option<AnalysisReader> {
        Some(Arc::new(super::load_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_row_with_its_id_and_no_playlists() {
        let dir = std::env::temp_dir().join(format!("openfader-own-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = |n: &str, aref: Option<&str>| {
            std::fs::write(dir.join(n), n.as_bytes()).unwrap();
            Track {
                title: n.into(),
                file: Some(dir.join(n)),
                analysis_ref: aref.map(Into::into),
                ..Track::default()
            }
        };
        let store = Arc::new(Store::in_memory().unwrap());
        let (plain, referenced) = (t("a.mp3", None), t("b.mp3", Some("nobody:x")));
        store
            .collection_put(&[plain.clone(), referenced.clone()], false)
            .unwrap();
        let own = Own::new(store);
        let rows = own.tracks(None).unwrap();
        assert_eq!(own.kind(), "Collection");
        assert_eq!(
            rows.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["a.mp3", "b.mp3"]
        );
        assert!(rows.iter().all(|t| t.id.is_some()));
        assert!(own.playlists().unwrap().is_empty() && own.tracks(Some("x")).unwrap().is_empty());
        // No analysis_ref, and a ref no plugin answers: no analysis, no panic; the reader agrees.
        let read = own.analysis_reader().unwrap();
        for t in &rows {
            assert!(own.analysis(t).is_none() && read(t).is_none());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
