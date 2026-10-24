//! Process-wide bulk scheduling and per-request completion. Compatible queued or in-flight jobs share decoding and
//! analysis; every subscriber keeps its own cue guide, tag settings, cancellation and result channel.

use super::{Outcome, store_cues, summary};
use openfader::{
    analysis::{
        auto,
        cache::{self, Source},
        options::Options,
        tag, Analysis, Beat, Phrases,
    },
    library::Track,
    store::Store,
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, LazyLock, Mutex, Weak,
    },
};

/// What the library already knows about a track, gathered on the UI thread: the grid and phrases cues are placed on (the
/// library's own, not ours: its cues must sit on its grid) and whether it has cues.
#[derive(Default)]
pub struct Guide {
    pub beats: Vec<Beat>,
    pub phrases: Option<Phrases>,
    pub has_cues: bool,
}

pub struct Job {
    pub path: PathBuf,
    /// What the analysis is of: its identity names the stored entry ([`cache`]).
    pub track: Track,
    /// [`openfader::store::key`] of the track.
    pub key: String,
    /// Asked for by the user (the track menu): analysed again whatever the cache holds, and its key is written to the tag
    /// even when the library has one.
    pub explicit: bool,
    /// Analysed again because an update changed the analysis ([`super::stale`]): the cache entry is replaced, but a cue list
    /// the track has stays whatever the settings say, and no tag is written.
    pub refresh: bool,
    /// The library gave the track a key.
    pub library_key: bool,
    pub guide: Guide,
}

/// The key as the file's tag spells it (the user's key notation).
pub type TagText = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// How a run goes.
#[derive(Clone)]
pub struct Settings {
    pub options: Options,
    /// Key Analysis "Write the value to the ID3 tag": `Some` = write, spelling the key this way.
    pub tag: Option<TagText>,
    pub workers: usize,
    /// Analysis Process "Power saving": the workers run at the lowest priority.
    pub low_priority: bool,
}

/// The cues and tag of one analysed track (`a`), the part of [`spawn`]'s work that is not decoding.
fn finish(job: &Job, a: &Analysis, src: Source, s: &Settings, store: &Store) -> Outcome {
    let g = &job.guide;
    let (beats, phrases) = if g.beats.is_empty() {
        (a.beats.as_slice(), a.phrases.clone())
    } else {
        let own = || {
            s.options
                .parts
                .phrase
                .then(|| auto::phrases(&g.beats, &a.three_band_detail))
                .flatten()
        };
        (g.beats.as_slice(), g.phrases.clone().or_else(own))
    };
    // Cues stored after the job was made (a pad pressed meanwhile, cue sync from a device) are newer than this analysis: they stay.
    let arrived = !g.has_cues
        && store
            .edits(&job.key)
            .is_ok_and(|e| e.cues.is_some_and(|c| !c.is_empty()));
    // A refresh never replaces a cue list, whatever "Prohibit overwriting CUEs" says.
    let cue_options = auto::CueOptions {
        keep_existing: s.options.cue.keep_existing || job.refresh,
        ..s.options.cue
    };
    let cues = !arrived
        && auto::for_analysis(beats, phrases.as_ref(), &cue_options, g.has_cues).is_some_and(|c| {
            match store.system(|| store_cues(store, &job.key, &c)) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("analyze: {}: cues not stored: {e}", job.path.display());
                    false
                }
            }
        });
    let tag = s
        .tag
        .as_ref()
        .filter(|_| !job.refresh && (job.explicit || !job.library_key))
        .zip(a.key.as_deref())
        .map(|(text, key)| tag::write_key(&job.path, &text(key)));
    Outcome {
        found: summary(a),
        src,
        cues,
        tag,
    }
}

/// Lowers the calling thread's priority: nice 19 on Linux (niceness is per thread there), the background QoS class on macOS, the
/// lowest thread priority on Windows.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        }
        // PRIO_PROCESS = 0 with who = 0 is the calling thread on Linux.
        unsafe { setpriority(0, 0, 19) };
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: acts on the calling thread only.
        unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0) };
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
        };
        // SAFETY: acts on the calling thread only.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
    }
}

pub struct Cancellation(Arc<AtomicBool>);

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Subscriber {
    job: Job,
    source: Option<String>,
    settings: Settings,
    store: Arc<Store>,
    tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
    cancelled: Arc<AtomicBool>,
}

struct Work {
    track: Track,
    path: PathBuf,
    key: String,
    explicit: bool,
    source: Option<String>,
    options: Options,
    low_priority: bool,
    subscribers: Mutex<Vec<Subscriber>>,
}

impl Work {
    fn compatible(&self, subscriber: &Subscriber) -> bool {
        let (job, settings) = (&subscriber.job, &subscriber.settings);
        self.key == job.key
            && self.path == job.path
            && self.source == subscriber.source
            && self.explicit == job.explicit
            && self.options == settings.options
            && self.low_priority == settings.low_priority
    }

    fn cancelled(&self) -> bool {
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .all(|s| s.cancelled.load(Ordering::Relaxed))
    }
}

#[derive(Default)]
struct Pool {
    queued: VecDeque<Arc<Work>>,
    running: Vec<Arc<Work>>,
    by_track: HashMap<String, Vec<Weak<Work>>>,
    limit: usize,
}

impl Pool {
    fn submit(&mut self, subscriber: Subscriber) {
        let work = self
            .by_track
            .get(&subscriber.job.key)
            .into_iter()
            .flatten()
            .filter_map(Weak::upgrade)
            .find(|w| w.compatible(&subscriber) && !w.cancelled());
        if let Some(work) = work {
            work.subscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(subscriber);
            return;
        }
        let work = Arc::new(Work {
            track: subscriber.job.track.clone(),
            path: subscriber.job.path.clone(),
            key: subscriber.job.key.clone(),
            explicit: subscriber.job.explicit,
            source: subscriber.source.clone(),
            options: subscriber.settings.options,
            low_priority: subscriber.settings.low_priority,
            subscribers: Mutex::new(vec![subscriber]),
        });
        self.by_track
            .entry(work.key.clone())
            .or_default()
            .push(Arc::downgrade(&work));
        self.queued.push_back(work);
    }

    fn forget(&mut self, work: &Arc<Work>) {
        if let Some(candidates) = self.by_track.get_mut(&work.key) {
            candidates.retain(|w| w.upgrade().is_some_and(|w| !Arc::ptr_eq(&w, work)));
            if candidates.is_empty() {
                self.by_track.remove(&work.key);
            }
        }
    }

    fn take_ready(&mut self) -> Vec<Arc<Work>> {
        let mut ready = Vec::new();
        while self.running.len() < self.limit {
            let Some(work) = self.queued.pop_front() else {
                break;
            };
            if work.cancelled() {
                self.forget(&work);
                continue;
            }
            self.running.push(work.clone());
            ready.push(work);
        }
        ready
    }
}

fn pool() -> &'static Mutex<Pool> {
    static POOL: LazyLock<Mutex<Pool>> = LazyLock::new(|| Mutex::new(Pool::default()));
    &POOL
}

fn dispatch() {
    let ready = pool()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take_ready();
    for work in ready {
        std::thread::spawn(move || {
            // Release the slot even if a detector or a subscriber's completion panics.
            struct Slot(Arc<Work>);
            impl Drop for Slot {
                fn drop(&mut self) {
                    {
                        let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
                        pool.running.retain(|w| !Arc::ptr_eq(w, &self.0));
                        pool.forget(&self.0);
                    }
                    dispatch();
                }
            }
            let _slot = Slot(work.clone());
            if work.low_priority {
                lower_priority();
            }
            if work.cancelled() {
                return;
            }
            let result =
                cache::get_or_analyze_with(&work.track, &work.options, work.explicit, &|| {
                    work.cancelled()
                });
            // Closing subscribers under the pool lock keeps later requests out of a completed computation.
            let subscribers = {
                let _pool = pool().lock().unwrap_or_else(|e| e.into_inner());
                std::mem::take(&mut *work.subscribers.lock().unwrap_or_else(|e| e.into_inner()))
            };
            if let Some(result) = result.transpose() {
                deliver(subscribers, result);
            }
        });
    }
}

fn deliver(subscribers: Vec<Subscriber>, result: Result<(Analysis, Source), String>) {
    for sub in subscribers {
        if sub.cancelled.load(Ordering::Relaxed) {
            continue;
        }
        // Library health lists a file whose analysis failed as corrupt, until it analyses or loads again.
        let outcome = match &result {
            Ok((a, source)) => {
                let _ = sub.store.health_clear_failure(&sub.job.track);
                Ok(finish(
                    &sub.job,
                    a,
                    copy_source(source),
                    &sub.settings,
                    &sub.store,
                ))
            }
            Err(e) => {
                let _ = sub.store.health_note_failure(&sub.job.track, e);
                Err(e.clone())
            }
        };
        let _ = sub.tx.send((sub.job.path, outcome));
    }
}

fn copy_source(source: &Source) -> Source {
    match source {
        Source::Cache => Source::Cache,
        Source::Computed { ignored } => Source::Computed {
            ignored: ignored.clone(),
        },
    }
}

/// Runs share a process-wide budget; compatible requests share computation but retain their own completion settings.
pub fn spawn(
    jobs: Vec<Job>,
    settings: Settings,
    store: Arc<Store>,
    tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
) -> Cancellation {
    let cancelled = Arc::new(AtomicBool::new(false));
    pool().lock().unwrap_or_else(|e| e.into_inner()).limit = settings.workers.max(1);
    let run_cancelled = cancelled.clone();
    std::thread::spawn(move || {
        for job in jobs {
            if run_cancelled.load(Ordering::Relaxed) {
                break;
            }
            // Source metadata can block on removable or network media, so it stays off the UI thread.
            let source = cache::key(&job.path).ok();
            pool()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .submit(Subscriber {
                    source,
                    job,
                    settings: settings.clone(),
                    store: store.clone(),
                    tx: tx.clone(),
                    cancelled: run_cancelled.clone(),
                });
            dispatch();
        }
    });
    Cancellation(cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::{Mood, Phrase, PhraseKind, auto::CueOptions};

    fn beats() -> Vec<Beat> {
        (0..256)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect()
    }

    fn analysed() -> Analysis {
        let list = vec![
            (1, PhraseKind::Intro),
            (65, PhraseKind::Chorus),
            (129, PhraseKind::Outro),
        ]
        .into_iter()
        .map(|(beat, kind)| Phrase {
            beat,
            end_beat: beat + 64,
            kind,
            variant: 0,
            fill: None,
        })
        .collect();
        Analysis {
            beats: beats(),
            key: Some("8A".into()),
            phrases: Some(Phrases {
                mood: Mood::High,
                list,
            }),
            ..Default::default()
        }
    }

    fn job(path: &str, guide: Guide) -> Job {
        Job {
            path: path.into(),
            track: Track {
                file: Some(path.into()),
                ..Track::default()
            },
            key: format!("{path}|1"),
            explicit: false,
            refresh: false,
            library_key: false,
            guide,
        }
    }

    fn settings(tag: bool) -> Settings {
        Settings {
            options: Options::default(),
            tag: tag.then(|| Arc::new(|k: &str| format!("<{k}>")) as TagText),
            workers: 1,
            low_priority: false,
        }
    }

    fn src() -> Source {
        Source::Computed { ignored: None }
    }

    /// A temp dir holding a fake mp3, removed afterwards (never the user's music).
    fn temp_mp3(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rk-job-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp3");
        std::fs::write(&file, [0xff, 0xfb, 0x90, 0x64, 1, 2, 3]).unwrap();
        (dir, file)
    }

    #[test]
    fn cue_analysis_stores_cues_on_the_library_grid_and_marks_the_track() {
        let store = Store::in_memory().unwrap();
        let j = job("/m/a.mp3", Guide::default());
        let out = finish(&j, &analysed(), src(), &settings(false), &store);
        assert!(out.cues && out.tag.is_none());
        let stored = store.edits(&j.key).unwrap().cues.unwrap();
        assert_eq!(stored.iter().filter(|c| c.hot_cue == 0).count(), 1);
        assert_eq!(
            stored
                .iter()
                .filter(|c| c.hot_cue > 0)
                .map(|c| c.time_ms)
                .collect::<Vec<_>>(),
            [0, 32_000, 64_000],
            "phrase starts of the analysis"
        );
        assert!(super::super::analysed(&store).contains(&j.key));
        assert_eq!(out.found, (12000, "8A".into()));

        // the library's own (shifted) grid wins over ours, with the library's phrases or phrases derived from it
        let shifted: Vec<Beat> = beats()
            .into_iter()
            .map(|b| Beat {
                time_ms: b.time_ms + 100,
                ..b
            })
            .collect();
        let j2 = job(
            "/m/b.mp3",
            Guide {
                beats: shifted,
                ..Default::default()
            },
        );
        finish(&j2, &analysed(), src(), &settings(false), &store);
        let on_library_grid = store.edits(&j2.key).unwrap().cues.unwrap();
        assert!(
            on_library_grid
                .iter()
                .filter(|c| c.hot_cue == 0)
                .all(|c| c.time_ms == 100),
            "1.1 of the library grid"
        );
    }

    #[test]
    fn cues_stored_while_the_track_was_analysed_stay() {
        let store = Store::in_memory().unwrap();
        let j = job("/m/a.mp3", Guide::default());
        let mine = vec![openfader::analysis::Cue {
            hot_cue: 1,
            time_ms: 4_000,
            loop_end_ms: None,
            color: None,
            comment: None,
        }];
        store.set_cues(&j.key, Some(&mine)).unwrap();
        assert!(!finish(&j, &analysed(), src(), &settings(false), &store).cues);
        assert_eq!(store.edits(&j.key).unwrap().cues, Some(mine));
    }

    #[test]
    fn a_refresh_after_an_update_keeps_the_users_cues_grid_and_tags() {
        let store = Store::in_memory().unwrap();
        let (dir, file) = temp_mp3("refresh");
        let path = file.to_str().unwrap();
        let before = std::fs::read(&file).unwrap();
        let mut j = job(
            path,
            Guide {
                has_cues: true,
                ..Default::default()
            },
        );
        j.refresh = true;
        // The user's cue list and an edited grid (the library's own beats, shifted).
        let mine = vec![openfader::analysis::Cue {
            hot_cue: 1,
            time_ms: 4_000,
            loop_end_ms: None,
            color: None,
            comment: None,
        }];
        let grid: Vec<Beat> = beats()
            .into_iter()
            .map(|b| Beat {
                time_ms: b.time_ms + 70,
                ..b
            })
            .collect();
        store.set_cues(&j.key, Some(&mine)).unwrap();
        store.set_grid(&j.key, &grid, &beats()).unwrap();
        // Overwriting CUEs is allowed (the default) and the key tag is asked for: a refresh does neither.
        let out = finish(&j, &analysed(), src(), &settings(true), &store);
        assert!(!out.cues && out.tag.is_none());
        let edits = store.edits(&j.key).unwrap();
        assert_eq!(edits.cues, Some(mine));
        assert_eq!(edits.grid, Some(grid));
        assert_eq!(
            std::fs::read(&file).unwrap(),
            before,
            "the file is untouched"
        );
        // The same job without the refresh mark (the track menu's re-analysis) does overwrite the cues.
        j.refresh = false;
        assert!(finish(&j, &analysed(), src(), &settings(false), &store).cues);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prohibit_overwriting_and_zero_counts_leave_the_track_alone() {
        let store = Store::in_memory().unwrap();
        let had = job(
            "/m/a.mp3",
            Guide {
                has_cues: true,
                ..Default::default()
            },
        );
        let keep = Settings {
            options: Options {
                cue: CueOptions {
                    keep_existing: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..settings(false)
        };
        assert!(!finish(&had, &analysed(), src(), &keep, &store).cues);
        assert!(store.edits(&had.key).unwrap().cues.is_none());
        let overwrite = finish(&had, &analysed(), src(), &settings(false), &store);
        assert!(overwrite.cues, "overwriting is allowed unless prohibited");
        let none = Settings {
            options: Options {
                cue: CueOptions {
                    hot: 0,
                    memory: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..settings(false)
        };
        let other = job("/m/c.mp3", Guide::default());
        assert!(!finish(&other, &analysed(), src(), &none, &store).cues);
        assert!(store.edits(&other.key).unwrap().cues.is_none());
        let no_grid = Analysis {
            beats: Vec::new(),
            ..analysed()
        };
        assert!(
            !finish(&other, &no_grid, src(), &settings(false), &store).cues,
            "no grid, no cues"
        );
    }

    #[test]
    fn the_key_is_written_only_when_asked_and_only_to_the_file() {
        let store = Store::in_memory().unwrap();
        let (dir, file) = temp_mp3("tag");
        let path = file.to_str().unwrap();
        let before = std::fs::read(&file).unwrap();
        // off: the file is never touched
        assert!(
            finish(
                &job(path, Guide::default()),
                &analysed(),
                src(),
                &settings(false),
                &store
            )
            .tag
            .is_none()
        );
        assert_eq!(std::fs::read(&file).unwrap(), before);
        // on: the key in the user's spelling
        let out = finish(
            &job(path, Guide::default()),
            &analysed(),
            src(),
            &settings(true),
            &store,
        );
        assert_eq!(out.tag, Some(Ok(true)));
        assert_eq!(tag::read_key(&file).as_deref(), Some("<8A>"));
        // a library key is respected by background analysis, not by an explicit request
        let mut j = job(path, Guide::default());
        j.library_key = true;
        let analysed_9b = Analysis {
            key: Some("9B".into()),
            ..analysed()
        };
        assert!(
            finish(&j, &analysed_9b, src(), &settings(true), &store)
                .tag
                .is_none()
        );
        j.explicit = true;
        assert_eq!(
            finish(&j, &analysed_9b, src(), &settings(true), &store).tag,
            Some(Ok(true))
        );
        assert_eq!(tag::read_key(&file).as_deref(), Some("<9B>"));
        // no detected key (the KEY part off): nothing to write
        let no_key = Analysis {
            key: None,
            ..analysed()
        };
        assert!(
            finish(
                &job(path, Guide::default()),
                &no_key,
                src(),
                &settings(true),
                &store
            )
            .tag
            .is_none()
        );
        // an unsupported file is refused, not damaged
        let m4a = dir.join("a.m4a");
        std::fs::write(&m4a, b"x").unwrap();
        let out = finish(
            &job(m4a.to_str().unwrap(), Guide::default()),
            &analysed(),
            src(),
            &settings(true),
            &store,
        );
        assert!(matches!(out.tag, Some(Err(_))));
        assert_eq!(std::fs::read(&m4a).unwrap(), b"x");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn power_saving_workers_run_at_the_lowest_priority() {
        unsafe extern "C" {
            fn getpriority(which: i32, who: u32) -> i32;
        }
        let nice = std::thread::spawn(|| {
            lower_priority();
            unsafe { getpriority(0, 0) }
        })
        .join()
        .unwrap();
        assert_eq!(nice, 19);
        let other = std::thread::spawn(|| unsafe { getpriority(0, 0) })
            .join()
            .unwrap();
        assert!(
            other < 19 || nice == other,
            "the lowered priority stays with that thread"
        );
    }

    #[test]
    fn the_pool_analyses_every_job_once_and_reports_failures() {
        let (tx, rx) = mpsc::channel();
        let jobs: Vec<Job> = (0..6)
            .map(|i| job(&format!("/nonexistent/{i}.mp3"), Guide::default()))
            .collect();
        let _run = spawn(
            jobs,
            Settings {
                workers: 3,
                ..settings(false)
            },
            Arc::new(Store::in_memory().unwrap()),
            tx,
        );
        let mut got: Vec<PathBuf> = rx
            .iter()
            .map(|(p, r)| {
                assert!(r.is_err(), "a missing file is an error, not a panic");
                p
            })
            .collect();
        got.sort();
        assert_eq!(got.len(), 6);
        got.dedup();
        assert_eq!(got.len(), 6, "each job exactly once");
    }

    fn subscriber(
        path: &str,
        explicit: bool,
        settings: Settings,
    ) -> (
        Subscriber,
        mpsc::Receiver<(PathBuf, Result<Outcome, String>)>,
    ) {
        let (tx, rx) = mpsc::channel();
        let mut job = job(path, Guide::default());
        job.explicit = explicit;
        (
            Subscriber {
                source: cache::key(&job.path).ok(),
                job,
                settings,
                store: Arc::new(Store::in_memory().unwrap()),
                tx,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
            rx,
        )
    }

    #[test]
    fn bulk_budget_and_coalescing_cover_queued_and_inflight_runs() {
        let mut pool = Pool {
            limit: 2,
            ..Default::default()
        };
        for path in ["a", "b", "c"] {
            pool.submit(subscriber(path, false, settings(false)).0);
        }
        let ready = pool.take_ready();
        assert_eq!(ready.len(), 2);
        assert_eq!(pool.queued.len(), 1);
        pool.submit(subscriber("a", false, settings(false)).0);
        pool.submit(subscriber("c", false, settings(false)).0);
        assert_eq!(pool.queued.len(), 1);
        assert_eq!(pool.running[0].subscribers.lock().unwrap().len(), 2);
        assert_eq!(pool.queued[0].subscribers.lock().unwrap().len(), 2);
        assert!(pool.take_ready().is_empty());
        pool.limit = 1;
        pool.running.remove(0);
        assert!(
            pool.take_ready().is_empty(),
            "a lower budget waits for existing workers"
        );
        pool.running.clear();
        let ready = pool.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].path, PathBuf::from("c"));
        assert!(pool.queued.is_empty());
    }

    #[test]
    fn explicit_reanalysis_and_changed_options_are_not_cache_eligible_work() {
        let mut pool = Pool {
            limit: 1,
            ..Default::default()
        };
        pool.submit(subscriber("a", false, settings(false)).0);
        let _running = pool.take_ready();
        pool.submit(subscriber("a", true, settings(false)).0);
        pool.submit(subscriber("a", true, settings(false)).0);
        let mut changed = settings(false);
        changed.options.grid.bpm_range = (110, 220);
        pool.submit(subscriber("a", true, changed).0);
        assert_eq!(pool.running.len(), 1);
        assert!(!pool.running[0].explicit);
        assert_eq!(pool.queued.len(), 2);
        assert!(pool.queued.iter().all(|w| w.explicit));
        assert_eq!(pool.queued[0].subscribers.lock().unwrap().len(), 2);
        assert_eq!(pool.queued[1].options.grid.bpm_range, (110, 220));
    }

    #[test]
    fn coalesced_results_keep_each_subscribers_tag_settings_and_progress() {
        let (dir, file) = temp_mp3("coalesced");
        let path = file.to_str().unwrap();
        let mut pool = Pool {
            limit: 1,
            ..Default::default()
        };
        let (off, off_rx) = subscriber(path, true, settings(false));
        let (on, on_rx) = subscriber(path, true, settings(true));
        let (cancelled, cancelled_rx) = subscriber(path, true, settings(true));
        let cancel = Cancellation(cancelled.cancelled.clone());
        pool.submit(off);
        let work = pool.take_ready().pop().unwrap();
        pool.submit(on);
        pool.submit(cancelled);
        drop(cancel);
        assert!(pool.queued.is_empty());
        let subscribers = std::mem::take(&mut *work.subscribers.lock().unwrap());
        deliver(subscribers, Ok((analysed(), src())));
        let off = off_rx.recv().unwrap().1.unwrap();
        let on = on_rx.recv().unwrap().1.unwrap();
        assert_eq!(off.found, (12000, "8A".into()));
        assert_eq!(on.found, off.found);
        assert!(off.tag.is_none());
        assert_eq!(on.tag, Some(Ok(true)));
        assert_eq!(tag::read_key(&file).as_deref(), Some("<8A>"));
        assert!(matches!(
            cancelled_rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(off_rx.try_recv().is_err() && on_rx.try_recv().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancelled_queued_runs_do_not_consume_a_worker_slot() {
        let mut pool = Pool {
            limit: 1,
            ..Default::default()
        };
        let (sub, rx) = subscriber("cancelled", false, settings(false));
        let cancelled = Cancellation(sub.cancelled.clone());
        pool.submit(sub);
        drop(cancelled);
        pool.submit(subscriber("live", false, settings(false)).0);
        let ready = pool.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].path, PathBuf::from("live"));
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn an_audio_file_changed_during_analysis_gets_fresh_explicit_work() {
        let (dir, file) = temp_mp3("changed-during-analysis");
        let path = file.to_str().unwrap();
        let mut pool = Pool {
            limit: 1,
            ..Default::default()
        };
        pool.submit(subscriber(path, true, settings(false)).0);
        let _running = pool.take_ready();
        std::fs::write(&file, b"changed audio bytes").unwrap();
        pool.submit(subscriber(path, true, settings(false)).0);
        assert_eq!(pool.running.len(), 1);
        assert_eq!(pool.queued.len(), 1);
        assert_ne!(pool.running[0].source, pool.queued[0].source);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
