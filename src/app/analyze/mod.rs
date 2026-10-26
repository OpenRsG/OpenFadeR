//! Bulk analysis ("Analyze tracks"): a pool of worker threads decodes and analyses library tracks through the
//! persistent cache ([`openfader::analysis::cache`], a hit skips the decode). Which tracks, what is computed and what is
//! done with the result follow Preferences > Analysis ([`super::prefs::analysis`]):
//!
//! * Auto Analysis: at start-up (and on ANALYZE NOW) every track lacking a BPM / grid or a key that the enabled parts
//!   would fill is analysed; with Auto Analysis off nothing runs by itself. [`request`] is the explicit path: the browser's
//!   track menu hands it the selected tracks, which are analysed again whatever the cache holds (tracks with the analysis
//!   lock are skipped). [`request_keep_cues`] is the path of imports that bring cues: the cache entry is made, no cues are stored.
//! * Track Analysis Setting / mode / precision / BPM range and the cue options reach the analysis code
//!   as [`openfader::analysis::options`], set from the settings by [`sync`] on every change.
//! * Analysis Process: [`workers`] threads, at low priority when power saving ([`job`]).
//! * Results land in [`Detected`] (session map, path → BPM/key); the library's files are never touched except by Key
//!   Analysis "Write the value to the ID3 tag" ([`job`]) and CUE Analysis stores cues in the store ([`cue`]).
//!   Progress shows in the track info panel ([`super::trackinfo`]).
//!
//! Cancelling drops the run's subscription: queued work is skipped and a shared in-flight result still reaches other runs.
//!
//! This base owns the types the children share ([`Found`], [`Outcome`]) and the run on the UI thread. Children: `detected`
//! (session results, which tracks are pending), `progress` (counters and text), `job` (worker pool), `cue` (library grid
//! and cue habits), `sync` (settings to options), `key_model` (the NEURAL key detector's model download).

mod cue;
mod detected;
mod job;
mod key_model;
mod progress;
mod stale;
mod sync;

pub use cue::{library_cues, Learnt};
pub use detected::{keys_differ, Detected};
pub(in crate::app) use job::lower_priority;

use super::{
    App, camelot,
    prefs::{
        self,
        analysis::{CueSetting, GridSource, Process},
    },
    trackinfo,
};
use job::{Guide, Job, Settings, TagText};
use openfader::{
    analysis::{Analysis, cache, cache::Source, options},
    library::{Library, Track},
    store::{self, Store},
};
use progress::Progress;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};

/// Results are drained (and the browser told) this often.
const TICK: Duration = Duration::from_millis(500);

/// (BPM × 100, Camelot key); 0 / "" = this part was not found.
pub type Found = (i64, String);

/// What one finished track gave back to the UI thread.
struct Outcome {
    found: Found,
    src: Source,
    /// CUE Analysis stored cues for the track.
    cues: bool,
    /// The tag write: `Some(Ok(true))` written, `Ok(false)` already that key, `Err` refused.
    tag: Option<Result<bool, String>>,
    generation: u64,
    skipped: bool,
    cancelled: bool,
    ticket: Option<Arc<std::sync::Mutex<job::Generations>>>,
}

struct Ctx {
    app: slint::Weak<App>,
    lib: Rc<dyn Library>,
}

/// A running pool: its timer (dropping it cancels the run) and whether it finished.
struct Run {
    _timer: Rc<Timer>,
    done: Rc<Cell<bool>>,
    _cancel: Option<job::Cancellation>,
    progress: Option<Rc<RefCell<Progress>>>,
}

thread_local! {
    static DETECTED: Detected = Detected::default();
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
    static RUNS: RefCell<Vec<Run>> = const { RefCell::new(Vec::new()) };
    static LEARNT: RefCell<Option<Learnt>> = const { RefCell::new(None) };
    static LEARNER: RefCell<Option<Arc<Learner>>> = const { RefCell::new(None) };
}

pub fn detected() -> Detected {
    DETECTED.with(Clone::clone)
}

struct Learner {
    tracks: Arc<Vec<Track>>,
    reader: Option<openfader::library::AnalysisReader>,
    result: std::sync::OnceLock<Result<Learnt, String>>,
}

impl Learner {
    fn learned(self: &Arc<Self>, store: &Store) -> Result<Learnt, String> {
        self.result
            .get_or_init(|| {
                let learnt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cue::learn(
                        &self.tracks,
                        &|t| {
                            store::with_edits(
                                self.reader.as_ref().and_then(|read| read(t)),
                                t,
                                store,
                            )
                        },
                        &analysed(store),
                    )
                }))
                .map_err(|_| "cue tendency learning panicked".to_owned())?;
                let (learner, ready) = (self.clone(), learnt.clone());
                let _ = slint::invoke_from_event_loop(move || learner.publish(ready));
                Ok(learnt)
            })
            .clone()
    }

    fn publish(self: &Arc<Self>, learnt: Learnt) {
        let current = LEARNER.with_borrow(|current| {
            current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, self))
        });
        if current && prefs::with(|p| p.analysis.cue_mode == CueSetting::Auto) {
            LEARNT.set(Some(learnt));
            prefs::with(set_options);
        }
    }
}

/// BPM of the first beat of the grid and the key.
fn summary(a: &Analysis) -> Found {
    (
        a.beats.first().map_or(0, |b| b.bpm_x100 as i64),
        a.key.clone().unwrap_or_default(),
    )
}

/// A deck analysed `track` when it loaded it: the lists (track table, preview strips, related tracks) show the BPM, key and
/// waveform it found, as they do after a bulk run, instead of waiting for ANALYZE NOW. Tracks without a file, and tracks the
/// library already gives a BPM and key, change nothing the lists show.
pub fn noted(track: &Track, a: &Analysis) {
    if let Some(file) = track.file.as_deref() {
        detected().note(file, summary(a));
    }
}

/// Keys ([`store::key`]) of the tracks whose cue list CUE Analysis set and nobody edited since.
fn analysed(store: &Store) -> HashSet<String> {
    store
        .device_settings(store::AUTO_CUES)
        .map(|m| m.into_keys().collect())
        .unwrap_or_default()
}

/// The CUE Analysis playlist (shown when VIEW > Layout > "CUE Analysis Playlist" is on): keys ([`store::key`]) of the
/// tracks whose cues CUE Analysis set. Changes when [`Detected::version`] moves (each finished track bumps it).
pub fn cue_analysed() -> HashSet<String> {
    analysed(&store::global())
}

/// Worker threads of Analysis Process `process` on this machine.
pub fn workers(process: Process) -> usize {
    crate::app::prefs::lowpower::analysis_workers(sync::workers(
        process,
        std::thread::available_parallelism().map_or(1, |n| n.get()),
    ))
}

/// What CUE Analysis Auto learnt from the collection (empty until Auto is chosen).
pub fn tendency_status() -> Learnt {
    LEARNT
        .with_borrow(|learnt| learnt.clone())
        .or_else(|| {
            LEARNER.with_borrow(|learner| {
                learner.as_ref().and_then(|learner| {
                    learner
                        .result
                        .get()
                        .and_then(|result| result.as_ref().ok())
                        .cloned()
                })
            })
        })
        .unwrap_or_default()
}

/// Keep it alive as long as the window: dropping it cancels every run and forgets the library.
pub struct Analyzer;

impl Drop for Analyzer {
    fn drop(&mut self) {
        RUNS.with(|r| r.borrow_mut().clear());
        CTX.with(|c| *c.borrow_mut() = None);
        LEARNER.set(None);
        LEARNT.set(None);
    }
}

/// Connects the analysis to the settings and the library: hands the settings to the analysis code (and again on every
/// change), registers ANALYZE NOW, and starts the background run when Auto Analysis is on.
pub fn wire(app: &App, lib: Rc<dyn Library>) -> Analyzer {
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            app: app.as_weak(),
            lib,
        })
    });
    set_options(&prefs::get());
    prefs::on_change(set_options);
    key_model::wire(app);
    prefs::on_action("analyze_now", run_pending);
    let auto = prefs::with(|p| p.analysis.auto_analysis);
    stale::wire(app);
    if cache::legacy_dir().is_some_and(|d| d.is_dir()) {
        migrate_then(auto);
    } else {
        startup(auto);
    }
    Analyzer
}

/// The runs at start: the background run for tracks lacking a BPM or key when Auto Analysis is on, then the check for
/// tracks an older analysis made ([`stale`]); that one also runs with Auto Analysis off.
fn startup(auto: bool) {
    if auto {
        run_pending();
    }
    stale::check();
}

/// Moves the old per-path analysis entries into the data folder on a worker thread ([`cache::migrate`]); the background run
/// (`then_run`) starts once it has finished, so no track is analysed again for an entry that was only waiting to move.
fn migrate_then(then_run: bool) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let tracks = store::global().collection_tracks().unwrap_or_default();
        let m = cache::migrate(&tracks);
        if m.moved > 0 {
            eprintln!("analysis: {} entries moved to the data folder", m.moved);
        }
        let _ = tx.send(());
    });
    let (timer, done) = (Rc::new(Timer::default()), Rc::new(Cell::new(false)));
    let (weak_timer, finished) = (Rc::downgrade(&timer), done.clone());
    timer.start(TimerMode::Repeated, TICK, move || {
        if rx.try_recv() == Err(mpsc::TryRecvError::Empty) {
            return;
        }
        finished.set(true);
        weak_timer.upgrade().inspect(|t| t.stop());
        startup(then_run);
    });
    RUNS.with(|r| {
        r.borrow_mut().push(Run {
            _timer: timer,
            done,
            _cancel: None,
            progress: None,
        })
    });
}

/// The settings as analysis options; chooses CUE Analysis Auto's tendency from the collection when Auto is selected.
fn set_options(p: &prefs::Prefs) {
    let a = &p.analysis;
    if a.cue_mode == CueSetting::Auto {
        if LEARNT.with_borrow(Option::is_none) && LEARNER.with_borrow(Option::is_none) {
            let lib = CTX.with(|c| c.borrow().as_ref().map(|c| c.lib.clone()));
            if let Some(lib) = lib {
                let learner = Arc::new(Learner {
                    tracks: lib.tracks_all().unwrap_or_default(),
                    reader: lib.analysis_reader(),
                    result: std::sync::OnceLock::new(),
                });
                LEARNER.set(Some(learner.clone()));
                job::learn(
                    learner,
                    store::global(),
                    workers(a.process),
                    a.process == Process::PowerSaving,
                );
            }
        }
    } else {
        LEARNT.with(|l| *l.borrow_mut() = None);
        LEARNER.set(None);
    }
    options::set(sync::options(
        a,
        LEARNT.with(|l| l.borrow().as_ref().and_then(|l| l.tendency)),
        openfader::analysis::key::neural_ready(),
    ));
}

fn context() -> Option<(App, Rc<dyn Library>)> {
    CTX.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|c| Some((c.app.upgrade()?, c.lib.clone())))
    })
}

/// Analyses what the enabled parts would fill in (the Auto Analysis set), whatever the Auto Analysis setting says.
fn run_pending() {
    let Some((app, lib)) = context() else { return };
    let tracks = match lib.tracks_all() {
        Ok(t) => t,
        Err(e) => {
            return trackinfo::set_progress(&app, &tr!("analysis: library query failed: {}", e));
        }
    };
    let todo = detected::pending(&tracks, &detected(), &options::get().parts);
    start_run(&app, jobs(&*lib, &todo, false), prefs::with(settings));
}

/// Explicit analysis (the track menu's ANALYZE TRACK): `tracks` are analysed again, whatever the cache holds and whether
/// the library has their BPM and key; tracks without a file or with the analysis lock are skipped. A run that is not
/// `explicit` (files just imported) uses the cache and leaves a key tag the file has alone. Does nothing before [`wire`].
pub fn request(tracks: &[Track], explicit: bool) {
    let Some((app, lib)) = context() else { return };
    let refs: Vec<&Track> = tracks.iter().collect();
    start_run(&app, jobs(&*lib, &refs, explicit), prefs::with(settings));
}

/// [`request`] for tracks that came with cues (an import): the analysis fills the cache (waveform, grid, key) and stores no cues,
/// so the cue list they came with stays whatever "Prohibit overwriting CUEs" says. Never explicit: a key tag the library gave
/// is left alone.
pub fn request_keep_cues(tracks: &[Track]) {
    let Some((app, lib)) = context() else { return };
    let refs: Vec<&Track> = tracks.iter().collect();
    let jobs = jobs(&*lib, &refs, false)
        .into_iter()
        .map(|j| Job { keep_cues: true, ..j })
        .collect();
    start_run(&app, jobs, prefs::with(settings));
}

/// What [`request`] would run for `tracks`, captured without reading source analysis. The shared workers prepare each track.
/// `None` before [`wire`] or when no track has a file; locked tracks are skipped by the worker.
pub fn batch(tracks: &[Track]) -> Option<Batch> {
    let (_, lib) = context()?;
    let refs: Vec<&Track> = tracks.iter().collect();
    let jobs = jobs(&*lib, &refs, false);
    (!jobs.is_empty()).then(|| Batch {
        jobs,
        settings: prefs::with(settings),
    })
}

/// The jobs of [`batch`], with the settings of the moment.
pub struct Batch {
    jobs: Vec<Job>,
    settings: Settings,
}

/// A batch result keeps its submission identity until the UI publishes it.
pub struct Published {
    path: std::path::PathBuf,
    found: Found,
    generation: u64,
    ticket: Option<Arc<std::sync::Mutex<job::Generations>>>,
}

impl Batch {
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Runs the jobs on the shared pool and waits; `progress(done)` is called as each track ends. A `cancelled` run drops what is
    /// queued. Returns what the tracks that were analysed gave, for [`publish`].
    pub fn run(
        self,
        store: Arc<Store>,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<Published>, String> {
        let total = self.jobs.len();
        let (tx, rx) = mpsc::channel();
        let _run = job::spawn(self.jobs, self.settings, store, tx)?;
        let (mut done, mut found) = (0, Vec::new());
        while done < total && !cancelled() {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok((path, r)) => {
                    done += 1;
                    progress(done);
                    match r {
                        Ok(o) if o.cancelled => return Err("cancelled".into()),
                        Ok(o) if !o.skipped => found.push(Published {
                            path,
                            found: o.found,
                            generation: o.generation,
                            ticket: o.ticket,
                        }),
                        Ok(_) => {}
                        Err(e) => eprintln!("analyze: {e}"),
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(found)
    }
}

/// Shows what a [`Batch`] found as a background run's results are shown (the browser's BPM and key cells).
pub fn publish(found: Vec<Published>) {
    let Some((app, _)) = context() else { return };
    let have = detected();
    for Published {
        path,
        found,
        generation,
        ticket,
    } in found
    {
        if job::current(ticket.as_ref(), generation) {
            have.insert(path.clone(), found);
            trackinfo::detected_changed(&app, &path);
        }
    }
}

fn notice_for(source: GridSource, library_beats: bool, edited: bool) -> &'static str {
    if source == GridSource::Library && library_beats && !edited {
        tr!(
            " · library grid in use: PREFS > ANALYSIS > BEAT GRID FROM > OPENFADER ANALYSIS shows ours"
        )
    } else {
        ""
    }
}

fn settings(p: &prefs::Prefs) -> Settings {
    let (a, notation) = (&p.analysis, p.key_notation);
    Settings {
        options: options::get(),
        tag: (a.key_write_tag && a.part_key)
            .then(|| Arc::new(move |k: &str| camelot::display(k, notation)) as TagText),
        workers: workers(a.process),
        low_priority: a.process == Process::PowerSaving,
        grid_source: a.grid_source,
        notice: None,
        learner: (a.cue_mode == CueSetting::Auto)
            .then(|| LEARNER.with_borrow(Clone::clone))
            .flatten(),
    }
}

/// `t` as the Collection lists it when it names a file the Collection has (a file just imported is handed over bare): the row
/// carries the id that records the audio fingerprint, and the tags the job decides by.
fn listed(store: &Store, t: &Track) -> Track {
    let row = match (t.id, t.file.as_deref()) {
        (None, Some(f)) => store.id_of_path(f).ok().flatten(),
        _ => None,
    };
    row.and_then(|id| store.collection_track(id).ok().flatten())
        .unwrap_or_else(|| t.clone())
}

/// Lightweight submissions only: source analysis, Collection metadata and locks are read by the shared workers.
fn jobs(lib: &dyn Library, tracks: &[&Track], explicit: bool) -> Vec<Job> {
    let reader = lib.analysis_reader();
    tracks
        .iter()
        .filter_map(|t| {
            let path = t.file.clone()?;
            let key =
                t.id.map(store::id_key)
                    .unwrap_or_else(|| format!("pending:{}", path.display()));
            Some(Job {
                path,
                track: (*t).clone(),
                key,
                explicit,
                refresh: false,
                keep_cues: false,
                library_key: !t.key.is_empty(),
                guide: Guide::default(),
                reader: reader.clone(),
                generation: 0,
                ticket: None,
                cue_epoch: 0,
            })
        })
        .collect()
}

fn progress_text() -> String {
    RUNS.with(|runs| {
        let mut total = Progress::default();
        for progress in runs.borrow().iter().filter_map(|r| r.progress.as_ref()) {
            total.include(&progress.borrow());
        }
        total.text()
    })
}

/// Starts the pool on `jobs` and a timer that collects its results into [`Detected`] and the progress text.
fn start_run(app: &App, jobs: Vec<Job>, mut settings: Settings) {
    if jobs.is_empty() {
        return;
    }
    let have = detected();
    key_model::note(app);
    let progress = Rc::new(RefCell::new(Progress::new(jobs.len())));
    let (tx, rx) = mpsc::channel();
    let (notice_tx, notice_rx) = mpsc::channel();
    if jobs.len() == 1 && jobs[0].explicit {
        settings.notice = Some(notice_tx);
    }
    let cancel = match job::spawn(jobs, settings, store::global(), tx) {
        Ok(cancel) => cancel,
        Err(error) => {
            trackinfo::set_progress(app, &error);
            crate::app::toast::show(app, &error);
            return;
        }
    };
    let run_progress = progress.clone();
    let (timer, done) = (Rc::new(Timer::default()), Rc::new(Cell::new(false)));
    let (weak, weak_timer, finished) = (app.as_weak(), Rc::downgrade(&timer), done.clone());
    timer.start(TimerMode::Repeated, TICK, move || {
        let Some(app) = weak.upgrade() else { return };
        for notice in notice_rx.try_iter() {
            if job::current(notice.ticket.as_ref(), notice.generation) {
                app.global::<crate::app::BrowserState>()
                    .set_status(notice.text.clone().into());
                crate::app::toast::show(&app, &notice.text);
            }
        }
        let mut progress = progress.borrow_mut();
        for (path, r) in rx.try_iter() {
            progress.done += 1;
            match r {
                Ok(o) => {
                    if o.cancelled {
                        progress.cancelled += 1;
                    } else if o.skipped {
                        progress.skipped += 1;
                    } else {
                        progress.record(&o);
                        if job::current(o.ticket.as_ref(), o.generation) {
                            have.insert(path.clone(), o.found);
                            trackinfo::detected_changed(&app, &path);
                        }
                    }
                }
                Err(e) => {
                    progress.failed += 1;
                    eprintln!("analyze: {e}");
                }
            }
        }
        let complete = progress.done == progress.total;
        drop(progress);
        trackinfo::set_progress(&app, &progress_text());
        if complete {
            finished.set(true);
            RUNS.with(|runs| {
                if let Some(run) = runs
                    .borrow_mut()
                    .iter_mut()
                    .find(|run| Rc::ptr_eq(&run.done, &finished))
                {
                    run._cancel.take();
                }
            });
            weak_timer.upgrade().inspect(|t| t.stop());
        }
    });
    RUNS.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|x| !x.done.get());
        r.push(Run {
            _timer: timer,
            done,
            _cancel: Some(cancel),
            progress: Some(run_progress.clone()),
        });
    });
    trackinfo::set_progress(app, &progress_text());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_cues_replace_the_list_and_mark_the_track() {
        let store = Store::in_memory().unwrap();
        assert!(analysed(&store).is_empty());
        let cue = |hot_cue, time_ms| openfader::analysis::Cue {
            hot_cue,
            time_ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        };
        store
            .set_auto_cues("a.mp3|10", &[cue(0, 0), cue(1, 8000)])
            .unwrap();
        assert_eq!(store.edits("a.mp3|10").unwrap().cues.unwrap().len(), 2);
        assert_eq!(analysed(&store), HashSet::from(["a.mp3|10".to_owned()]));
        assert!(
            store.edits("b.mp3|10").unwrap().cues.is_none(),
            "other tracks are untouched"
        );
    }

    #[test]
    fn a_file_just_imported_is_analysed_as_its_collection_row_so_its_fingerprint_is_kept() {
        let dir = std::env::temp_dir().join(format!("openfader-listed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.wav");
        std::fs::write(&file, b"audio").unwrap();
        let store = Store::in_memory().unwrap();
        let row = Track {
            title: "A".into(),
            key: "8A".into(),
            file: Some(file.clone()),
            ..Track::default()
        };
        store.collection_put(&[row], false).unwrap();
        let bare = Track {
            file: Some(file),
            ..Track::default()
        };
        let t = listed(&store, &bare);
        assert!(t.id.is_some(), "the fingerprint is stored by row id");
        assert_eq!(t.key, "8A");
        let other = Track {
            file: Some(dir.join("gone.wav")),
            ..Track::default()
        };
        assert_eq!(
            listed(&store, &other),
            other,
            "not in the Collection: as given"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn library_grid_notice_only_when_ours_stays_hidden() {
        assert!(!notice_for(GridSource::Library, true, false).is_empty());
        assert!(notice_for(GridSource::Ours, true, false).is_empty());
        assert!(notice_for(GridSource::Library, false, false).is_empty());
        assert!(notice_for(GridSource::Library, true, true).is_empty());
    }
    #[test]
    fn a_blocked_analysis_reader_does_not_block_submission_or_read_on_the_ui_thread() {
        let _pool = job::TEST_RUNS.lock().unwrap_or_else(|e| e.into_inner());
        use openfader::library::{AnalysisReader, Playlist};
        use std::sync::{Condvar, Mutex};
        struct SlowLibrary(AnalysisReader);
        impl Library for SlowLibrary {
            fn kind(&self) -> &str {
                "blocked test source"
            }
            fn playlists(&self) -> Result<Vec<Playlist>, String> {
                Ok(Vec::new())
            }
            fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
                Ok(Vec::new())
            }
            fn analysis(&self, _: &Track) -> Option<Analysis> {
                panic!("source read on the UI thread")
            }
            fn analysis_reader(&self) -> Option<AnalysisReader> {
                Some(self.0.clone())
            }
        }
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = release.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let reader: AnalysisReader = Arc::new(move |_| {
            let _ = entered_tx.send(());
            let (lock, wake) = &*worker_release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            None
        });
        let (returned_tx, returned_rx) = mpsc::channel();
        let submit = std::thread::spawn(move || {
            let track = Track {
                file: Some("/missing/blocked-reader.wav".into()),
                ..Default::default()
            };
            let jobs = jobs(&SlowLibrary(reader), &[&track], true);
            let settings = Settings {
                options: options::Options::default(),
                tag: None,
                workers: 1,
                low_priority: false,
                grid_source: GridSource::Library,
                notice: None,
                learner: None,
            };
            let (tx, rx) = mpsc::channel();
            let cancel =
                job::spawn(jobs, settings, Arc::new(Store::in_memory().unwrap()), tx).unwrap();
            assert!(returned_tx.send((cancel, rx)).is_ok());
        });
        let returned = returned_rx.recv_timeout(Duration::from_secs(2));
        let entered = entered_rx.recv_timeout(Duration::from_secs(2));
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        submit.join().unwrap();
        let (_cancel, rx) = returned.expect("submission returns while the source is blocked");
        entered.expect("source reading starts only on a pool worker");
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().1.is_err());
    }
    #[test]
    fn old_or_disabled_auto_learners_cannot_replace_current_cue_settings() {
        let original_mode = prefs::with(|p| p.analysis.cue_mode);
        let original_options = options::get();
        let learner = || {
            Arc::new(Learner {
                tracks: Arc::new(Vec::new()),
                reader: None,
                result: std::sync::OnceLock::new(),
            })
        };
        let old = learner();
        let current = learner();
        LEARNER.set(Some(current.clone()));
        LEARNT.set(None);
        prefs::set_for_test(|p| p.analysis.cue_mode = CueSetting::Auto);
        old.publish(Learnt {
            found: 99,
            tendency: None,
        });
        assert!(LEARNT.with_borrow(Option::is_none));
        prefs::set_for_test(|p| p.analysis.cue_mode = CueSetting::Manual);
        current.publish(Learnt {
            found: 40,
            tendency: None,
        });
        assert!(LEARNT.with_borrow(Option::is_none));
        prefs::set_for_test(|p| p.analysis.cue_mode = CueSetting::Auto);
        current.publish(Learnt {
            found: 40,
            tendency: None,
        });
        assert_eq!(
            LEARNT.with_borrow(|learnt| learnt.as_ref().unwrap().found),
            40
        );
        LEARNER.set(None);
        LEARNT.set(None);
        prefs::set_for_test(|p| p.analysis.cue_mode = original_mode);
        options::set(original_options);
    }
}
