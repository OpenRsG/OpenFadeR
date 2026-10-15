//! The Collection as a [`Library`]: the opened source's playlists and tracks with our own Collection laid over them
//! ([`Store::collection_view`]): imported tracks are listed in "All tracks", removed ones are gone, relocated files show
//! their new path. The browser, decks, export and analysis all see this one list; the source itself stays read-only.

use super::{Library, Playlist, Track};
use crate::{analysis::Analysis, store::Store};
use std::sync::Arc;

pub struct Collection {
    inner: Box<dyn Library>,
    store: Arc<Store>,
}

impl Collection {
    pub fn new(inner: Box<dyn Library>, store: Arc<Store>) -> Self {
        Self { inner, store }
    }
}

impl Library for Collection {
    fn kind(&self) -> &str {
        self.inner.kind()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        self.inner.playlists()
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        self.store
            .collection_view(self.inner.tracks(playlist)?, playlist.is_none())
    }

    /// The source's analysis, asked with the path the source knows the track by (a relocated file is found under its
    /// original path through the source's own data). An imported track the source does not know falls back to the
    /// analysis its `analysis_ref` names.
    fn analysis(&self, track: &Track) -> Option<Analysis> {
        self.inner
            .analysis(track)
            .or_else(|| super::load_ref(track))
    }

    fn analysis_reader(&self) -> Option<super::AnalysisReader> {
        let inner = self.inner.analysis_reader();
        Some(std::sync::Arc::new(move |t| {
            inner
                .as_ref()
                .and_then(|r| r(t))
                .or_else(|| super::load_ref(t))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source(Vec<Track>);
    impl Library for Source {
        fn kind(&self) -> &str {
            "test"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            Ok(Vec::new())
        }
        fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn lists_source_then_imports_and_honours_removal() {
        let store = Arc::new(Store::in_memory().unwrap());
        let t = |n: &str| Track {
            title: n.into(),
            file: Some(format!("/m/{n}.mp3").into()),
            ..Track::default()
        };
        let lib = Collection::new(Box::new(Source(vec![t("a"), t("b")])), store.clone());
        store.collection_put(&[t("c")], false).unwrap();
        let titles = |l: &Collection, p| {
            l.tracks(p)
                .unwrap()
                .into_iter()
                .map(|t| t.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(&lib, None), ["a", "b", "c"]);
        assert_eq!(titles(&lib, Some("x")), ["a", "b"]);
        store.collection_remove(&t("a")).unwrap();
        assert_eq!(titles(&lib, None), ["b", "c"]);
        assert_eq!(lib.kind(), "test");
    }
}
