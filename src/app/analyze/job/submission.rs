//! Admission reserves request order immediately; existing workers register identities in bounded batches before publication.

use super::*;
use openfader::analysis::options::Parts;
use std::sync::atomic::AtomicUsize;

pub(super) const CHUNK: usize = 64;
type DetectedReader = Arc<Mutex<HashMap<PathBuf, crate::app::analyze::Found>>>;

pub(crate) struct Tracks {
    pub tracks: Arc<Vec<Track>>,
    pub rows: Option<Vec<usize>>,
    pub reader: Option<AnalysisReader>,
    pub explicit: bool,
    pub refresh: bool,
    pub keep_cues: bool,
    pub pending: Option<(Parts, DetectedReader)>,
}

impl Tracks {
    pub fn len(&self) -> usize {
        self.rows.as_ref().map_or(self.tracks.len(), Vec::len)
    }
}

enum Input {
    #[cfg(test)]
    Jobs(std::vec::IntoIter<Job>),
    Tracks(Tracks, usize),
}

enum Reserved {
    Job(Box<Job>),
    Track(usize, u64, Arc<Mutex<Generations>>),
}

pub(super) struct Submission {
    input: Input,
    ready: VecDeque<Reserved>,
    settings: Settings,
    store: Arc<Store>,
    tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
    cancelled: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
    registered: Arc<AtomicBool>,
    generation: u64,
    cue_epoch: u64,
}

impl Submission {
    pub(super) fn reserve(&mut self) {
        if self.ready.capacity() == 0 {
            self.ready.reserve_exact(self.total.load(Ordering::Relaxed));
        }
    }
    fn register(&mut self, pool: &mut Pool) -> bool {
        for _ in 0..CHUNK {
            let (path, job, index): (PathBuf, Option<Job>, usize) = match &mut self.input {
                #[cfg(test)]
                Input::Jobs(jobs) => match jobs.next() {
                    Some(job) => (job.path.clone(), Some(job), 0),
                    None => return true,
                },
                Input::Tracks(source, cursor) => {
                    if *cursor == source.len() {
                        return true;
                    }
                    let index = source.rows.as_ref().map_or(*cursor, |rows| rows[*cursor]);
                    *cursor += 1;
                    let track = &source.tracks[index];
                    let wanted = track.file.as_ref().is_some_and(|path| {
                        source.pending.as_ref().is_none_or(|(parts, have)| {
                            crate::app::analyze::detected::lacking(track, parts)
                                && !have
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .contains_key(path)
                        })
                    });
                    if !wanted {
                        self.total.fetch_sub(1, Ordering::Relaxed);
                        self.generation += 1;
                        continue;
                    }
                    (track.file.clone().unwrap(), None, index)
                }
            };
            let generation = self.generation;
            self.generation += 1;
            let ticket = pool
                .generations
                .get(&path)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let ticket = Arc::new(Mutex::new(Generations::default()));
                    if !pool.generations.contains_key(&path) {
                        pool.generation_sweep.push_back(path.clone());
                    }
                    pool.generations.insert(path, Arc::downgrade(&ticket));
                    ticket
                });
            {
                let mut generations = ticket.lock().unwrap_or_else(|e| e.into_inner());
                generations.latest();
                if let Some((previous, run)) = generations.pending.last_mut()
                    && run.ptr_eq(&Arc::downgrade(&self.cancelled))
                {
                    *previous = generation;
                } else {
                    generations
                        .pending
                        .push((generation, Arc::downgrade(&self.cancelled)));
                }
            }
            self.ready.push_back(match job {
                Some(mut job) => {
                    job.generation = generation;
                    job.cue_epoch = self.cue_epoch;
                    job.ticket = Some(ticket);
                    Reserved::Job(Box::new(job))
                }
                None => Reserved::Track(index, generation, ticket),
            });
        }
        match &self.input {
            #[cfg(test)]
            Input::Jobs(jobs) => jobs.len() == 0,
            Input::Tracks(source, cursor) => *cursor == source.len(),
        }
    }

    fn next(&mut self) -> Option<Subscriber> {
        let job = match self.ready.pop_front()? {
            Reserved::Job(job) => *job,
            Reserved::Track(index, generation, ticket) => {
                let source = match &self.input {
                    Input::Tracks(source, _) => source,
                    #[cfg(test)]
                    Input::Jobs(_) => unreachable!(),
                };
                let track = &source.tracks[index];
                let path = track.file.clone().unwrap();
                Job {
                    key: track
                        .id
                        .map(openfader::store::id_key)
                        .unwrap_or_else(|| format!("pending:{}", path.display())),
                    path,
                    track: track.clone(),
                    explicit: source.explicit,
                    refresh: source.refresh,
                    keep_cues: source.keep_cues,
                    library_key: !track.key.is_empty(),
                    guide: Guide::default(),
                    reader: source.reader.clone(),
                    generation,
                    ticket: Some(ticket),
                    cue_epoch: self.cue_epoch,
                }
            }
        };
        Some(Subscriber {
            job,
            source: None,
            settings: self.settings.clone(),
            store: self.store.clone(),
            tx: self.tx.clone(),
            cancelled: self.cancelled.clone(),
        })
    }
}

impl Pool {
    pub(super) fn stage(&mut self, mut submission: Submission) -> Option<Submission> {
        if submission.cancelled.load(Ordering::Relaxed) {
            self.staging_active = false;
            return Some(submission);
        }
        if submission.register(self) {
            submission.registered.store(true, Ordering::Release);
            self.admitted.push_back(submission);
        } else {
            self.staging.push_front(submission);
        }
        self.staging_active = false;
        // Sweep only a bounded part of the weak registry, including after large cancelled runs.
        for _ in 0..CHUNK.min(self.generation_sweep.len()) {
            let path = self.generation_sweep.pop_front().unwrap();
            if self
                .generations
                .get(&path)
                .is_some_and(|ticket| ticket.strong_count() > 0)
            {
                self.generation_sweep.push_back(path);
            } else {
                self.generations.remove(&path);
            }
        }
        None
    }

    pub(super) fn refill(&mut self) {
        while self.pending.len() < CHUNK {
            let Some(submission) = self.admitted.front_mut() else {
                break;
            };
            if submission.cancelled.load(Ordering::Relaxed) || submission.ready.is_empty() {
                self.retired.push(self.admitted.pop_front().unwrap());
            } else if let Some(sub) = submission.next() {
                self.pending.push_back(sub);
            }
        }
    }

    pub(super) fn cancel_submissions(&mut self, cancelled: &Arc<AtomicBool>) {
        for submissions in [&mut self.staging, &mut self.admitted] {
            let mut index = 0;
            while index < submissions.len() {
                if Arc::ptr_eq(&submissions[index].cancelled, cancelled) {
                    self.retired.push(submissions.remove(index).unwrap());
                } else {
                    index += 1;
                }
            }
        }
    }

    // Request-sized allocations and metadata clones belong to the staging worker, never admission.
    fn enqueue(
        &mut self,
        input: Input,
        total: usize,
        settings: Settings,
        store: Arc<Store>,
        tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
    ) -> Result<Cancellation, String> {
        let cancelled = Arc::new(AtomicBool::new(total == 0));
        self.admit(&cancelled, settings.workers)?;
        let total = Arc::new(AtomicUsize::new(total));
        let registered = Arc::new(AtomicBool::new(false));
        let cue_epoch = store.cue_generation();
        let generation = self.sequence + 1;
        self.sequence += total.load(Ordering::Relaxed) as u64;
        self.staging.push_back(Submission {
            input,
            ready: VecDeque::new(),
            settings,
            store,
            tx,
            cancelled: cancelled.clone(),
            total: total.clone(),
            registered: registered.clone(),
            generation,
            cue_epoch,
        });
        Ok(Cancellation {
            cancelled,
            total,
            registered,
        })
    }
}

fn submit(
    input: Input,
    total: usize,
    settings: Settings,
    store: Arc<Store>,
    tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
) -> Result<Cancellation, String> {
    let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
    let (workers, low_priority) = (settings.workers, settings.low_priority);
    let cancel = pool.enqueue(input, total, settings, store, tx)?;
    pool.configure(workers, low_priority);
    wake().notify_all();
    Ok(cancel)
}

pub(crate) fn spawn_tracks(
    tracks: Tracks,
    settings: Settings,
    store: Arc<Store>,
    tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
) -> Result<Cancellation, String> {
    let total = tracks.len();
    submit(Input::Tracks(tracks, 0), total, settings, store, tx)
}

#[cfg(test)]
pub(crate) fn spawn(
    jobs: Vec<Job>,
    settings: Settings,
    store: Arc<Store>,
    tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
) -> Result<Cancellation, String> {
    let total = jobs.len();
    submit(Input::Jobs(jobs.into_iter()), total, settings, store, tx)
}

pub(crate) struct Publication {
    _pool: std::sync::MutexGuard<'static, Pool>,
}

/// Hold results until every newer intent has a ticket; fence a bounded chunk against concurrent admission.
pub(crate) fn publication() -> Option<Publication> {
    let pool = pool().lock().unwrap_or_else(|e| e.into_inner());
    if !pool.staging.is_empty() || pool.staging_active {
        return None;
    }
    Some(Publication { _pool: pool })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(count: usize) -> Tracks {
        Tracks {
            tracks: Arc::new(
                (0..count)
                    .map(|i| Track {
                        title: format!("Track {i}"),
                        file: Some(format!("/missing/staged-{i}.wav").into()),
                        ..Default::default()
                    })
                    .collect(),
            ),
            rows: None,
            reader: Some(Arc::new(|_| {
                panic!("registration must not read source analysis")
            })),
            explicit: false,
            refresh: false,
            keep_cues: true,
            pending: None,
        }
    }

    fn settings() -> Settings {
        Settings {
            options: Options::default(),
            tag: None,
            workers: 4,
            low_priority: false,
            grid_source: crate::app::prefs::analysis::GridSource::Ours,
            notice: None,
            learner: None,
        }
    }

    fn enqueue(
        pool: &mut Pool,
        source: Tracks,
    ) -> (
        Cancellation,
        mpsc::Receiver<(PathBuf, Result<Outcome, String>)>,
    ) {
        let (tx, rx) = mpsc::channel();
        let count = source.len();
        let run = pool
            .enqueue(
                Input::Tracks(source, 0),
                count,
                settings(),
                Arc::new(Store::in_memory().unwrap()),
                tx,
            )
            .unwrap();
        (run, rx)
    }

    fn stage(pool: &mut Pool) {
        let Some(Task::Stage(mut submission)) = pool.next(false) else {
            panic!("staging task")
        };
        submission.reserve();
        assert!(pool.stage(submission).is_none());
    }

    #[test]
    fn indexed_selection_retains_shared_metadata_and_captured_row_order() {
        let mut pool = Pool {
            limit: 4,
            ..Default::default()
        };
        let mut source = tracks(10);
        let snapshot = source.tracks.clone();
        source.rows = Some(vec![8, 2, 5]);
        let (run, _) = enqueue(&mut pool, source);
        assert_eq!(run.total(), 3);
        assert_eq!(Arc::strong_count(&snapshot), 2);
        assert!(pool.generations.is_empty());
        stage(&mut pool);
        let jobs: Vec<_> = std::iter::from_fn(|| pool.admitted[0].next()).collect();
        assert_eq!(
            jobs.iter().map(|s| s.job.path.clone()).collect::<Vec<_>>(),
            [8, 2, 5].map(|i| PathBuf::from(format!("/missing/staged-{i}.wav")))
        );
        assert_eq!(
            jobs.iter().map(|s| s.job.generation).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(pool.generations.len(), 3);
    }

    #[test]
    fn ten_and_hundred_thousand_submissions_keep_admission_and_cancellation_bounded() {
        for count in [10_000, 100_000] {
            let mut pool = Pool {
                limit: 4,
                ..Default::default()
            };
            let mut source = tracks(count);
            source.rows = Some((0..count).rev().collect());
            let snapshot = source.tracks.clone();
            let reader = source.reader.clone().unwrap();
            let (run, rx) = enqueue(&mut pool, source);
            assert_eq!(Arc::strong_count(&snapshot), 2);
            assert_eq!(
                Arc::strong_count(&reader),
                2,
                "no eager per-track reader/metadata clones"
            );
            assert!(pool.pending.is_empty() && pool.generations.is_empty());
            stage(&mut pool);
            assert_eq!(pool.staging[0].ready.len(), CHUNK);
            assert!(!run.registered());
            while !pool.staging.is_empty() {
                stage(&mut pool);
            }
            assert!(run.registered());
            assert_eq!(run.total(), count);
            assert_eq!(pool.admitted[0].ready.len(), count);
            assert!(pool.pending.is_empty());
            pool.refill();
            assert_eq!(pool.pending.len(), CHUNK);
            assert_eq!(Arc::strong_count(&reader), CHUNK + 2);
            run.cancelled.store(true, Ordering::Relaxed);
            pool.cancel(&run.cancelled);
            assert!(pool.pending.is_empty() && pool.admitted.is_empty() && pool.staging.is_empty());
            assert_eq!(pool.retired.len(), 1);
            let Some(Task::Discard(retired)) = pool.next(false) else {
                panic!("worker releases snapshot")
            };
            drop(retired);
            assert_eq!(Arc::strong_count(&snapshot), 1);
            assert_eq!(Arc::strong_count(&reader), 1);
            assert!(matches!(
                rx.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
        }
    }

    #[test]
    fn deferred_registration_preserves_newer_intent_and_unrelated_results_until_ready() {
        let mut pool = Pool {
            limit: 4,
            ..Default::default()
        };
        let (old, _) = enqueue(&mut pool, tracks(CHUNK + 1));
        stage(&mut pool);
        let old_path = PathBuf::from("/missing/staged-0.wav");
        let old_ticket = pool.generations[&old_path].upgrade().unwrap();
        let old_generation = old_ticket.lock().unwrap().pending[0].0;
        let mut newer_source = tracks(1);
        newer_source.explicit = true;
        newer_source.keep_cues = false;
        let (newer, _) = enqueue(&mut pool, newer_source);
        let mut unrelated = tracks(1);
        Arc::get_mut(&mut unrelated.tracks).unwrap()[0].file =
            Some("/missing/unrelated.wav".into());
        let (_disjoint, _) = enqueue(&mut pool, unrelated);
        let have = crate::app::analyze::Detected::default();
        let held = crate::app::analyze::Published {
            path: "/missing/staged-1.wav".into(),
            found: (12300, "8A".into()),
            generation: old_generation + 1,
            ticket: Some(
                pool.generations[&PathBuf::from("/missing/staged-1.wav")]
                    .upgrade()
                    .unwrap(),
            ),
        };
        assert!(
            !pool.staging.is_empty(),
            "hold valid old results while newer tickets are incomplete"
        );
        assert!(have.get(&held.path).is_none());
        while !pool.staging.is_empty() {
            stage(&mut pool);
        }
        assert!(crate::app::analyze::publish_one(&have, held).is_some());
        assert_eq!(
            have.get(&PathBuf::from("/missing/staged-1.wav")),
            Some((12300, "8A".into()))
        );
        assert!(
            !current(Some(&old_ticket), old_generation),
            "newer request wins even though old staging finished later"
        );
        newer.cancelled.store(true, Ordering::Relaxed);
        pool.cancel(&newer.cancelled);
        assert!(
            current(Some(&old_ticket), old_generation),
            "cancelled intent releases older live results"
        );
        assert!(old.registered());
    }

    #[test]
    fn staging_counts_only_eligible_tracks_and_preserves_the_request_cue_epoch() {
        let mut pool = Pool {
            limit: 4,
            ..Default::default()
        };
        let mut source = tracks(4);
        let have = Arc::new(Mutex::new(HashMap::new()));
        have.lock()
            .unwrap()
            .insert(source.tracks[0].file.clone().unwrap(), (12000, "8A".into()));
        let rows = Arc::get_mut(&mut source.tracks).unwrap();
        rows[1].bpm_x100 = 12000;
        rows[1].key = "8A".into();
        rows[2].file = None;
        source.pending = Some((Parts::default(), have));
        source.reader = None;
        source.keep_cues = false;
        let store = Arc::new(Store::in_memory().unwrap());
        let key = openfader::store::key(&source.tracks[3]);
        let (tx, rx) = mpsc::channel();
        let run = pool
            .enqueue(Input::Tracks(source, 0), 4, settings(), store.clone(), tx)
            .unwrap();
        store.set_cues(&key, Some(&[])).unwrap();
        stage(&mut pool);
        assert_eq!(run.total(), 1);
        pool.refill();
        let mut sub = pool.pending.pop_front().unwrap();
        assert!(prepare(&mut sub).unwrap());
        assert!(
            sub.job.keep_cues,
            "editing while a snapshot awaits registration is still newer than the request"
        );
        deliver_cancelled(vec![sub]);
        assert!(rx.recv().unwrap().1.unwrap().cancelled);
    }

    #[test]
    fn cancelling_active_registration_releases_the_barrier_and_staging_needs_no_analysis_slot() {
        let mut pool = Pool {
            limit: 4,
            learning_running: 4,
            ..Default::default()
        };
        let source = tracks(100_000);
        let snapshot = source.tracks.clone();
        let (old, _) = enqueue(&mut pool, source);
        let Some(Task::Stage(mut active)) = pool.next(false) else {
            panic!("staging is not an analysis slot")
        };
        active.reserve();
        let (newer, _) = enqueue(&mut pool, tracks(1));
        old.cancelled.store(true, Ordering::Relaxed);
        pool.cancel(&old.cancelled);
        assert!(pool.staging_active);
        drop(
            pool.stage(active)
                .expect("discard cancelled snapshot on worker"),
        );
        assert!(!pool.staging_active);
        assert_eq!(Arc::strong_count(&snapshot), 1);
        stage(&mut pool);
        assert!(newer.registered());
        assert!(pool.staging.is_empty());
        assert_eq!(pool.admitted[0].next().unwrap().job.generation, 100_001);
    }

    #[test]
    fn overlapping_completions_publish_newer_values_but_account_for_every_subscriber() {
        let mut pool = Pool {
            limit: 4,
            ..Default::default()
        };
        let (_old, old_rx) = enqueue(&mut pool, tracks(1));
        stage(&mut pool);
        let (_newer, newer_rx) = enqueue(&mut pool, tracks(1));
        stage(&mut pool);
        pool.admitted[1].settings.options.grid.bpm_range = (110, 220);
        let old = pool.admitted[0].next().unwrap();
        let newer = pool.admitted[1].next().unwrap();
        assert_eq!(newer.settings.options.grid.bpm_range, (110, 220));
        assert_ne!(
            old.settings.options.grid.bpm_range,
            newer.settings.options.grid.bpm_range
        );
        let analysis = |bpm, key: &str| Analysis {
            beats: vec![Beat {
                bar_pos: 1,
                bpm_x100: bpm,
                time_ms: 0,
            }],
            key: Some(key.into()),
            ..Default::default()
        };
        deliver(vec![newer], Ok((analysis(12800, "9A"), Source::Cache)));
        deliver(vec![old], Ok((analysis(12300, "8A"), Source::Cache)));
        let have = crate::app::analyze::Detected::default();
        let mut old_progress = crate::app::analyze::Progress::new(1);
        let mut new_progress = crate::app::analyze::Progress::new(1);
        let (new_paths, new_count) =
            crate::app::analyze::receive(&have, &mut new_progress, &newer_rx);
        let (old_paths, old_count) =
            crate::app::analyze::receive(&have, &mut old_progress, &old_rx);
        assert_eq!(
            (new_count, old_count, new_progress.done, old_progress.done),
            (1, 1, 1, 1)
        );
        assert_eq!(new_paths, [PathBuf::from("/missing/staged-0.wav")]);
        assert!(old_paths.is_empty());
        assert_eq!(have.get(&new_paths[0]), Some((12800, "9A".into())));
        assert_eq!(have.version(), 1);
    }
}
