//! One selected-track read at a time; another selection replaces the waiting request.

use super::analyze;
use openfader::{
    analysis::Phrases,
    library::{AnalysisReader, Track},
};
use std::{collections::HashMap, path::PathBuf, sync::mpsc};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    store: String,
    file: Option<PathBuf>,
    reference: Option<String>,
}

impl Key {
    fn new(store: String, t: &Track) -> Self {
        Self {
            store,
            file: t.file.clone(),
            reference: t.analysis_ref.clone(),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Details {
    pub found: Option<analyze::Found>,
    pub phrases: Option<Phrases>,
}

struct Request {
    key: Key,
    track: Track,
    reader: Option<AnalysisReader>,
    found: bool,
}

struct Running {
    epoch: u64,
    key: Key,
    rx: mpsc::Receiver<Details>,
}

#[derive(Default)]
pub(super) struct Reads {
    scope: Option<(u64, u64)>,
    epoch: u64,
    cache: HashMap<Key, Details>,
    running: Option<Running>,
    pending: Option<Request>,
}

impl Reads {
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    pub fn scope(&mut self, library: u64, generation: u64) {
        if self.scope != Some((library, generation)) {
            self.scope = Some((library, generation));
            self.invalidate();
        }
    }

    pub fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.cache.clear();
        self.pending = None;
    }

    pub fn select(
        &mut self,
        key: String,
        track: &Track,
        reader: Option<AnalysisReader>,
        found: bool,
    ) -> Details {
        let key = Key::new(key, track);
        self.pending = None;
        if let Some(details) = self.cache.get(&key) {
            return details.clone();
        }
        if !found && reader.is_none() {
            return self.cache.entry(key).or_default().clone();
        }
        if self
            .running
            .as_ref()
            .is_none_or(|r| r.epoch != self.epoch || r.key != key)
        {
            let request = Request {
                key,
                track: track.clone(),
                reader,
                found,
            };
            if self.running.is_some() {
                self.pending = Some(request);
            } else {
                self.start(request);
            }
        }
        Details::default()
    }

    fn start(&mut self, request: Request) {
        let (tx, rx) = mpsc::channel();
        self.running = Some(Running {
            epoch: self.epoch,
            key: request.key,
            rx,
        });
        std::thread::spawn(move || {
            let found = request
                .found
                .then(|| analyze::Detected::read(&request.track))
                .flatten();
            let phrases = request
                .reader
                .and_then(|read| read(&request.track))
                .and_then(|a| a.phrases);
            let _ = tx.send(Details { found, phrases });
        });
    }

    /// Takes completed work only. A miss is memoized until an explicit analysis/store/source invalidation.
    pub fn poll(&mut self) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let details = match running.rx.try_recv() {
            Ok(details) => Some(details),
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.complete(details);
        true
    }

    fn complete(&mut self, details: Option<Details>) {
        let running = self.running.take().unwrap();
        if running.epoch == self.epoch {
            if self.cache.len() >= 512 {
                self.cache.clear();
            }
            self.cache.insert(running.key, details.unwrap_or_default());
        }
        if let Some(request) = self.pending.take() {
            self.start(request);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::{Analysis, Mood, Phrase, PhraseKind};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::Duration;

    fn track(title: &str) -> Track {
        Track {
            title: title.into(),
            ..Default::default()
        }
    }

    fn phrases(mood: Mood) -> Phrases {
        Phrases {
            mood,
            list: vec![Phrase {
                beat: 1,
                end_beat: 17,
                kind: PhraseKind::Intro,
                variant: 1,
                fill: None,
            }],
        }
    }

    fn finish(reads: &mut Reads) {
        let details = reads
            .running
            .as_ref()
            .unwrap()
            .rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        reads.complete(Some(details));
    }

    #[test]
    fn a_slow_selection_keeps_only_the_latest_waiting_source_read() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(release_rx);
        let reader: AnalysisReader = Arc::new(move |t| {
            entered_tx.send(t.title.clone()).unwrap();
            if t.title == "a" {
                release.lock().unwrap().recv().unwrap();
            }
            Some(Analysis {
                phrases: Some(phrases(Mood::High)),
                ..Default::default()
            })
        });
        let mut reads = Reads::default();
        reads.scope(1, 1);
        reads.select("a".into(), &track("a"), Some(reader.clone()), false);
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "a"
        );
        reads.select("b".into(), &track("b"), Some(reader.clone()), false);
        reads.select("c".into(), &track("c"), Some(reader.clone()), false);
        assert!(
            !reads.poll(),
            "submission and polling return while the old source is blocked"
        );
        release_tx.send(()).unwrap();
        finish(&mut reads);
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "c"
        );
        finish(&mut reads);
        assert!(
            entered_rx.try_recv().is_err(),
            "superseded b was never read"
        );
        let details = reads.select("c".into(), &track("c"), Some(reader), false);
        assert_eq!(details.phrases, Some(phrases(Mood::High)));
        assert!(reads.running.is_none());
    }

    #[test]
    fn same_track_rejects_old_source_and_old_store_generation_responses() {
        for scope in [(2, 1), (1, 2)] {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let release = Mutex::new(release_rx);
            let old: AnalysisReader = Arc::new(move |_| {
                entered_tx.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
                Some(Analysis {
                    phrases: Some(phrases(Mood::High)),
                    ..Default::default()
                })
            });
            let new: AnalysisReader = Arc::new(|_| {
                Some(Analysis {
                    phrases: Some(phrases(Mood::Low)),
                    ..Default::default()
                })
            });
            let t = track("same");
            let mut reads = Reads::default();
            reads.scope(1, 1);
            reads.select("same".into(), &t, Some(old), false);
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            reads.scope(scope.0, scope.1);
            reads.select("same".into(), &t, Some(new.clone()), false);
            release_tx.send(()).unwrap();
            finish(&mut reads);
            assert!(
                reads.cache.is_empty(),
                "the old response cannot populate the new scope"
            );
            finish(&mut reads);
            assert_eq!(
                reads.select("same".into(), &t, Some(new), false).phrases,
                Some(phrases(Mood::Low))
            );
        }
    }

    #[test]
    fn source_analysis_references_do_not_share_a_memo_even_with_the_same_store_key() {
        let reader: AnalysisReader = Arc::new(|t| {
            Some(Analysis {
                phrases: Some(phrases(if t.analysis_ref.as_deref() == Some("one") {
                    Mood::High
                } else {
                    Mood::Low
                })),
                ..Default::default()
            })
        });
        let mut reads = Reads::default();
        let mut t = track("same");
        t.file = Some("/music/shared-source.wav".into());
        t.analysis_ref = Some("one".into());
        reads.select("same".into(), &t, Some(reader.clone()), false);
        finish(&mut reads);
        t.analysis_ref = Some("two".into());
        assert!(
            reads
                .select("same".into(), &t, Some(reader.clone()), false)
                .phrases
                .is_none()
        );
        finish(&mut reads);
        assert_eq!(
            reads.select("same".into(), &t, Some(reader), false).phrases,
            Some(phrases(Mood::Low))
        );
    }

    #[test]
    fn a_miss_is_read_once_then_analysis_arrival_invalidates_it_without_reselection() {
        let available = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let reader: AnalysisReader = {
            let available = available.clone();
            let calls = calls.clone();
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                available.load(Ordering::SeqCst).then(|| Analysis {
                    phrases: Some(phrases(Mood::Mid)),
                    ..Default::default()
                })
            })
        };
        let mut reads = Reads::default();
        let t = track("missing");
        reads.scope(1, 1);
        reads.select("same".into(), &t, Some(reader.clone()), true);
        finish(&mut reads);
        for _ in 0..4 {
            let details = reads.select("same".into(), &t, Some(reader.clone()), true);
            assert!(details.phrases.is_none() && details.found.is_none());
            assert!(
                reads.running.is_none(),
                "a negative answer does not read again every refresh"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        available.store(true, Ordering::SeqCst);
        reads.invalidate();
        reads.select("same".into(), &t, Some(reader.clone()), true);
        finish(&mut reads);
        assert_eq!(
            reads.select("same".into(), &t, Some(reader), true).phrases,
            Some(phrases(Mood::Mid))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_late_source_answer_cannot_restore_explicitly_deleted_phrases() {
        let reader: AnalysisReader = Arc::new(|_| {
            Some(Analysis {
                phrases: Some(phrases(Mood::High)),
                ..Default::default()
            })
        });
        let t = track("edited");
        let mut reads = Reads::default();
        reads.select("same".into(), &t, Some(reader.clone()), false);
        let edit = Phrases {
            mood: Mood::Low,
            list: Vec::new(),
        };
        finish(&mut reads);
        let details = reads.select("same".into(), &t, Some(reader), false);
        assert!(!super::super::phrase_text(None, details.phrases.clone()).is_empty());
        assert_eq!(
            super::super::phrase_text(Some(edit), details.phrases),
            tr!("Phrases: none")
        );
    }
}
