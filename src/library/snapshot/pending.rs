//! [`Pending`]: a [`Snapshot`] of the library seen through the store's edits ([`crate::store::Overlay`]) whose analyses are
//! still to be read. [`crate::library::Library`] is not `Send`, so the UI thread takes the tracks and playlists (cheap) and the worker that
//! writes reads the analyses (a plugin library opens files for each), with the library's [`AnalysisReader`].

use super::Snapshot;
use crate::{
    analysis::{Beat, Cue},
    library::{AnalysisReader, Track},
    store::{self, Store},
};

/// Beat grid and cues a deck holds for a loaded track ([`Snapshot::override_session`]).
type Session = (Track, Vec<Beat>, Vec<Cue>);

#[derive(Clone)]
pub struct Pending {
    /// Tracks, playlists and store data; source analyses are read only on the worker.
    snap: Snapshot,
    reader: Option<AnalysisReader>,
    /// Applied after the analyses are read, in the order they came.
    sessions: Vec<Session>,
}

impl Pending {
    pub(super) fn new(snap: Snapshot, reader: Option<AnalysisReader>) -> Self {
        Self {
            snap,
            reader,
            sessions: Vec::new(),
        }
    }

    /// Selected tracks without any synchronous source reads.
    pub fn of_tracks(tracks: Vec<Track>, reader: Option<AnalysisReader>) -> Self {
        Self::new(
            Snapshot::of_tracks(tracks.into_iter().map(|t| (t, None)).collect()),
            reader,
        )
    }
    /// The tracks and playlists, for what needs no analysis (a list of playlists, a count). Its analyses are not read.
    pub fn outline(&self) -> &Snapshot {
        &self.snap
    }

    /// [`Snapshot::add_store`]; colours and tags do not depend on the analysis.
    pub fn add_store(&mut self, store: &Store) -> Result<(), String> {
        self.snap.add_store(store)
    }

    /// [`Snapshot::override_session`], applied by [`Pending::read`] on top of the analysis it reads.
    pub fn override_session(&mut self, t: &Track, beats: Vec<Beat>, cues: Vec<Cue>) {
        self.sessions.push((t.clone(), beats, cues));
    }

    /// [`Snapshot::narrowed`]; `None` when no playlist has any of the ids.
    pub fn narrowed(&self, ids: &[&str]) -> Option<Self> {
        self.snap.narrowed(ids).map(|snap| Self {
            snap,
            reader: self.reader.clone(),
            sessions: self.sessions.clone(),
        })
    }

    /// `f` on the outline (what [`crate::device::sync::narrow`] does), keeping the analyses still to be read.
    pub fn map(
        &self,
        f: impl FnOnce(&Snapshot) -> Result<Snapshot, String>,
    ) -> Result<Self, String> {
        Ok(Self {
            snap: f(&self.snap)?,
            reader: self.reader.clone(),
            sessions: self.sessions.clone(),
        })
    }

    /// The snapshot with its analyses: what [`Snapshot::take`] gives for the overlaid library, read here, so call it on a
    /// worker. `store` is the one the library is overlaid with: its cue, grid and phrase edits go on top of what the reader
    /// answers ([`store::edited`]), then the decks' sessions.
    pub fn read(&self, store: &Store) -> Snapshot {
        self.read_cancellable(store, &|| false)
            .expect("uncancelled snapshot")
    }

    /// [`Pending::read`] with cancellation checked between source reads and session overrides.
    pub fn read_cancellable(
        &self,
        store: &Store,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Snapshot> {
        let mut snap = self.snap.clone();
        for (t, a) in &mut snap.tracks {
            if cancelled() {
                return None;
            }
            let own = self
                .reader
                .as_ref()
                .map_or_else(|| a.take(), |read| read(t));
            *a = store::with_edits(own, t, store);
        }
        for (t, beats, cues) in &self.sessions {
            if cancelled() {
                return None;
            }
            snap.override_session(t, beats.clone(), cues.clone());
        }
        (!cancelled()).then_some(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Analysis;
    use crate::library::{Library, Playlist};
    use std::sync::Arc;

    /// Three tracks, one playlist of two; analyses are available through both source interfaces.
    struct Lib;

    struct Empty;
    impl Library for Empty {
        fn kind(&self) -> &str {
            "empty source"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            Lib.playlists()
        }
        fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
            Lib.tracks(playlist)
        }
    }

    fn analysis(t: &Track) -> Option<Analysis> {
        Some(Analysis {
            key: Some(t.title.clone()),
            ..Analysis::default()
        })
    }

    impl Library for Lib {
        fn kind(&self) -> &str {
            "test"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            Ok(vec![Playlist {
                id: "p".into(),
                name: "p".into(),
                depth: 0,
                is_folder: false,
            }])
        }
        fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
            let all = (0..3).map(|i| Track {
                title: format!("t{i}"),
                ..Default::default()
            });
            Ok(match playlist {
                None => all.collect(),
                Some(_) => all.skip(1).collect(),
            })
        }
        fn analysis(&self, t: &Track) -> Option<Analysis> {
            analysis(t)
        }
        fn analysis_reader(&self) -> Option<AnalysisReader> {
            Some(Arc::new(analysis))
        }
    }

    fn cue(ms: u32) -> Cue {
        Cue {
            hot_cue: 1,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    /// The parts of a snapshot's analyses the test compares (a waveform-free `Analysis` has no `PartialEq`).
    fn shape(s: &Snapshot) -> Vec<(String, Option<String>, Option<Vec<u32>>)> {
        s.tracks()
            .iter()
            .map(|(t, a)| {
                (
                    t.title.clone(),
                    a.as_ref().and_then(|a| a.key.clone()),
                    a.as_ref()
                        .map(|a| a.cues.iter().map(|c| c.time_ms).collect()),
                )
            })
            .collect()
    }

    #[test]
    fn read_gives_what_take_gives_for_the_overlaid_library() {
        let store = Arc::new(Store::in_memory().unwrap());
        let overlaid = || store::Overlay::new(Box::new(Lib), store.clone());
        // An edit for a track the reader answers, and one for a track the library would answer with nothing new.
        let t1 = Track {
            title: "t1".into(),
            ..Default::default()
        };
        store
            .set_cues(&store::key(&t1), Some(&[cue(900)][..]))
            .unwrap();
        let session = (t1.clone(), Vec::new(), vec![cue(1234)]);

        let mut eager = Snapshot::take(&overlaid()).unwrap();
        eager.override_session(&session.0, session.1.clone(), session.2.clone());

        let mut pending = Snapshot::take_pending(&overlaid()).unwrap();
        assert!(pending.outline().tracks().iter().all(|(_, a)| a.is_none()));
        pending.override_session(&session.0, session.1.clone(), session.2.clone());
        let read = pending.read(&store);

        assert_eq!(shape(&read), shape(&eager));
        assert_eq!(
            read.tracks()[2].1.as_ref().unwrap().key.as_deref(),
            Some("t2")
        );
        assert_eq!(read.len(), 3);
        assert_eq!(
            read.playlists()
                .map(|(_, m)| m.to_vec())
                .collect::<Vec<_>>(),
            eager
                .playlists()
                .map(|(_, m)| m.to_vec())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_library_without_a_reader_never_reads_sources_on_the_ui_thread_and_keeps_store_edits() {
        let store = Store::in_memory().unwrap();
        let pending = Snapshot::take_pending(&Empty).unwrap();
        assert!(pending.outline().tracks().iter().all(|(_, a)| a.is_none()));
        let track = &pending.outline().tracks()[0].0;
        store
            .set_cues(&store::key(track), Some(&[cue(900)]))
            .unwrap();
        let read = pending.read(&store);
        assert_eq!(read.tracks()[0].1.as_ref().unwrap().cues[0].time_ms, 900);
        assert!(read.tracks()[1..].iter().all(|(_, a)| a.is_none()));
    }

    #[test]
    fn narrowed_keeps_the_analyses_to_read() {
        let store = Store::in_memory().unwrap();
        let pending = Snapshot::take_pending(&Lib).unwrap();
        let narrowed = pending.narrowed(&["p"]).unwrap();
        assert!(pending.narrowed(&["nope"]).is_none());
        let s = narrowed.read(&store);
        assert_eq!(shape(&s).len(), 2);
        assert!(s.tracks().iter().all(|(_, a)| a.is_some()));
    }
    #[test]
    fn deferred_track_sources_keep_their_own_cues_grid_and_key_without_overrides() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let calls = reads.clone();
        let pending = Pending::of_tracks(
            vec![Track {
                title: "source".into(),
                ..Default::default()
            }],
            Some(Arc::new(move |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                Some(Analysis {
                    key: Some("9B".into()),
                    cues: vec![cue(1800)],
                    beats: vec![Beat {
                        bar_pos: 1,
                        bpm_x100: 12800,
                        time_ms: 100,
                    }],
                    ..Default::default()
                })
            })),
        );
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        let read = pending.read(&Store::in_memory().unwrap());
        let analysis = read.tracks()[0].1.as_ref().unwrap();
        assert_eq!(analysis.key.as_deref(), Some("9B"));
        assert_eq!(analysis.cues[0].time_ms, 1800);
        assert_eq!(analysis.beats[0].time_ms, 100);
        assert_eq!(reads.load(Ordering::Relaxed), 1);
    }
}
