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
use job::{Settings, TagText};
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
    time::{Duration, Instant},
};

/// Results are drained (and the browser told) this often.
const TICK: Duration = Duration::from_millis(500);
const UI_BATCH: usize = 128;

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
    let mut jobs = jobs(&*lib, tracks, false);
    jobs.pending = Some((options::get().parts, detected().reader()));
    start_run(&app, jobs, prefs::with(settings));
}

/// Explicit analysis (the track menu's ANALYZE TRACK): `tracks` are analysed again, whatever the cache holds and whether
/// the library has their BPM and key; tracks without a file or with the analysis lock are skipped. A run that is not
/// `explicit` (files just imported) uses the cache and leaves a key tag the file has alone. Does nothing before [`wire`].
pub fn request(tracks: Arc<Vec<Track>>, explicit: bool) {
    let Some((app, lib)) = context() else { return };
    start_run(&app, jobs(&*lib, tracks, explicit), prefs::with(settings));
}

/// Explicit browser analysis of row indices into a shared list, without cloning selected track metadata.
pub fn request_selected(tracks: Arc<Vec<Track>>, rows: Vec<usize>) {
    let Some((app, lib)) = context() else { return };
    let mut jobs = jobs(&*lib, tracks, true);
    jobs.rows = Some(rows);
    start_run(&app, jobs, prefs::with(settings));
}

/// [`request`] for tracks that came with cues (an import): the analysis fills the cache (waveform, grid, key) and stores no cues,
/// so the cue list they came with stays whatever "Prohibit overwriting CUEs" says. Never explicit: a key tag the library gave
/// is left alone.
pub fn request_keep_cues(tracks: Arc<Vec<Track>>) {
    let Some((app, lib)) = context() else { return };
    let mut jobs = jobs(&*lib, tracks, false);
    jobs.keep_cues = true;
    start_run(&app, jobs, prefs::with(settings));
}

/// What [`request`] would run for `tracks`, captured without reading source analysis. The shared workers prepare each track.
/// `None` before [`wire`] or for an empty snapshot; workers exclude tracks without a file and skip locked tracks.
pub fn batch(tracks: Arc<Vec<Track>>) -> Option<Batch> {
    let (_, lib) = context()?;
    let jobs = jobs(&*lib, tracks, false);
    (jobs.len() > 0).then(|| Batch {
        jobs,
        settings: prefs::with(settings),
    })
}

/// The jobs of [`batch`], with the settings of the moment.
pub struct Batch {
    jobs: job::Tracks,
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
    /// Candidate count before workers exclude tracks without a file.
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
        let (tx, rx) = mpsc::channel();
        let run = job::spawn_tracks(self.jobs, self.settings, store, tx)?;
        let (mut done, mut found) = (0, Vec::new());
        while !cancelled() {
            if run.registered() && done == run.total() {
                break;
            }
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
    let weak = app.as_weak();
    let mut found = found.into_iter();
    let pump: Rc<RefCell<dyn FnMut() -> Option<Duration>>> = Rc::new(RefCell::new(move || {
        let app = weak.upgrade()?;
        let Some(publication) = job::publication() else {
            return Some(TICK);
        };
        let (paths, count) = publish_chunk(&have, &mut found);
        drop(publication);
        trackinfo::detected_changed(&app, &paths);
        (count == UI_BATCH).then_some(Duration::ZERO)
    }));
    schedule(pump, Rc::new(Cell::new(false)), Duration::ZERO);
}

fn publish_one(have: &Detected, result: Published) -> Option<std::path::PathBuf> {
    if !job::current(result.ticket.as_ref(), result.generation) {
        return None;
    }
    have.insert(result.path.clone(), result.found);
    Some(result.path)
}

fn publish_chunk(
    have: &Detected,
    found: &mut impl Iterator<Item = Published>,
) -> (Vec<std::path::PathBuf>, usize) {
    let mut paths = Vec::new();
    let mut count = 0;
    for result in found.take(UI_BATCH) {
        count += 1;
        if let Some(path) = publish_one(have, result) {
            paths.push(path);
        }
    }
    (paths, count)
}

fn receive(
    have: &Detected,
    progress: &mut Progress,
    rx: &mpsc::Receiver<(std::path::PathBuf, Result<Outcome, String>)>,
) -> (Vec<std::path::PathBuf>, usize) {
    let mut paths = Vec::new();
    let mut count = 0;
    for (path, result) in rx.try_iter().take(UI_BATCH) {
        count += 1;
        progress.done += 1;
        match result {
            Ok(o) if o.cancelled => progress.cancelled += 1,
            Ok(o) if o.skipped => progress.skipped += 1,
            Ok(o) => {
                progress.record(&o);
                if let Some(path) = publish_one(
                    have,
                    Published {
                        path,
                        found: o.found,
                        generation: o.generation,
                        ticket: o.ticket,
                    },
                ) {
                    paths.push(path);
                }
            }
            Err(e) => {
                progress.failed += 1;
                eprintln!("analyze: {e}");
            }
        }
    }
    (paths, count)
}

fn receive_notices(rx: &mpsc::Receiver<job::Notice>) -> (Option<String>, usize) {
    let mut latest = None;
    let mut count = 0;
    for notice in rx.try_iter().take(UI_BATCH) {
        count += 1;
        if job::current(notice.ticket.as_ref(), notice.generation) {
            latest = Some(notice.text);
        }
    }
    (latest, count)
}

fn schedule(
    pump: Rc<RefCell<dyn FnMut() -> Option<Duration>>>,
    queued: Rc<Cell<bool>>,
    delay: Duration,
) {
    if queued.replace(true) {
        return;
    }
    Timer::single_shot(delay, move || {
        queued.set(false);
        if let Some(delay) = pump.borrow_mut()() {
            schedule(pump.clone(), queued, delay);
        }
    });
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
fn jobs(lib: &dyn Library, tracks: Arc<Vec<Track>>, explicit: bool) -> job::Tracks {
    job::Tracks {
        tracks,
        rows: None,
        reader: lib.analysis_reader(),
        explicit,
        refresh: false,
        keep_cues: false,
        pending: None,
    }
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
fn start_run(app: &App, jobs: job::Tracks, mut settings: Settings) {
    if jobs.len() == 0 {
        return;
    }
    let have = detected();
    key_model::note(app);
    let progress = Rc::new(RefCell::new(Progress::new(jobs.len())));
    let (tx, rx) = mpsc::channel();
    let (notice_tx, notice_rx) = mpsc::channel();
    if jobs.len() == 1 && jobs.explicit {
        settings.notice = Some(notice_tx);
    }
    let cancel = match job::spawn_tracks(jobs, settings, store::global(), tx) {
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
    let last_progress = Cell::new(Instant::now());
    let pump: Rc<RefCell<dyn FnMut() -> Option<Duration>>> = Rc::new(RefCell::new(move || {
        let _live = weak_timer.upgrade()?;
        let app = weak.upgrade()?;
        if finished.get() {
            return None;
        }
        let Some(publication) = job::publication() else {
            return Some(TICK);
        };
        let (latest_notice, notices) = receive_notices(&notice_rx);
        let mut progress = progress.borrow_mut();
        RUNS.with_borrow(|runs| {
            if let Some(cancel) = runs
                .iter()
                .find(|r| Rc::ptr_eq(&r.done, &finished))
                .and_then(|r| r._cancel.as_ref())
            {
                progress.total = cancel.total();
            }
        });
        let (paths, results) = receive(&have, &mut progress, &rx);
        let complete = progress.done == progress.total;
        drop(progress);
        drop(publication);
        if let Some(text) = latest_notice {
            app.global::<crate::app::BrowserState>()
                .set_status(text.clone().into());
            crate::app::toast::show(&app, &text);
        }
        trackinfo::detected_changed(&app, &paths);
        if complete || (results > 0 && last_progress.get().elapsed() >= TICK) {
            trackinfo::set_progress(&app, &progress_text());
            last_progress.set(Instant::now());
        }
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
        (!complete && (results == UI_BATCH || notices == UI_BATCH)).then_some(Duration::ZERO)
    }));
    let queued = Rc::new(Cell::new(false));
    let (tick_pump, tick_queued) = (pump.clone(), queued.clone());
    timer.start(TimerMode::Repeated, TICK, move || {
        schedule(tick_pump.clone(), tick_queued.clone(), Duration::ZERO);
    });
    schedule(pump, queued, Duration::ZERO);
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
    fn notice_bursts_coalesce_the_last_current_text_per_bounded_turn() {
        let (tx, rx) = mpsc::channel();
        for i in 0..UI_BATCH * 2 + 1 {
            tx.send(job::Notice {
                ticket: None,
                generation: 0,
                text: i.to_string(),
            })
            .unwrap();
        }
        for (count, last) in [
            (UI_BATCH, UI_BATCH - 1),
            (UI_BATCH, UI_BATCH * 2 - 1),
            (1, UI_BATCH * 2),
        ] {
            assert_eq!(receive_notices(&rx), (Some(last.to_string()), count));
        }
        assert_eq!(receive_notices(&rx), (None, 0));
    }

    fn cached(generation: u64) -> Outcome {
        Outcome {
            found: (12300, "8A".into()),
            src: Source::Cache,
            cues: false,
            tag: None,
            generation,
            skipped: false,
            cancelled: false,
            ticket: None,
        }
    }

    #[test]
    fn ten_and_hundred_thousand_result_bursts_yield_without_losing_identity_or_progress() {
        for total in [10_000, 100_000] {
            let have = Detected::default();
            let (tx, rx) = mpsc::channel();
            for i in 0..total {
                tx.send((format!("/burst/{i}.wav").into(), Ok(cached(i as u64))))
                    .unwrap();
            }
            drop(tx);
            let mut progress = Progress::new(total);
            let mut turns = 0;
            while progress.done < total {
                let before = progress.done;
                let (paths, count) = receive(&have, &mut progress, &rx);
                assert!(count > 0 && count <= UI_BATCH);
                assert_eq!(progress.done - before, count);
                assert_eq!(paths.len(), count);
                turns += 1;
            }
            assert_eq!(turns, total.div_ceil(UI_BATCH));
            assert_eq!(have.version(), total as u64);
            assert_eq!(progress.failed + progress.skipped + progress.cancelled, 0);
            for i in 0..total {
                assert_eq!(
                    have.get(&std::path::PathBuf::from(format!("/burst/{i}.wav"))),
                    Some((12300, "8A".into()))
                );
            }
        }
    }

    #[test]
    fn published_batches_yield_and_cancelled_skipped_and_failed_outcomes_still_finish() {
        let have = Detected::default();
        let mut found = (0..UI_BATCH * 3 + 1).map(|i| Published {
            path: format!("/published/{i}.wav").into(),
            found: (12500, "9A".into()),
            generation: i as u64,
            ticket: None,
        });
        for expected in [UI_BATCH, UI_BATCH, UI_BATCH, 1, 0] {
            let (paths, count) = publish_chunk(&have, &mut found);
            assert_eq!((paths.len(), count), (expected, expected));
        }
        assert_eq!(have.version(), (UI_BATCH * 3 + 1) as u64);
        let (tx, rx) = mpsc::channel();
        let mut cancelled = cached(0);
        cancelled.cancelled = true;
        let mut skipped = cached(1);
        skipped.skipped = true;
        tx.send(("/cancelled".into(), Ok(cancelled))).unwrap();
        tx.send(("/skipped".into(), Ok(skipped))).unwrap();
        tx.send(("/failed".into(), Err("test failure".into())))
            .unwrap();
        tx.send(("/ok".into(), Ok(cached(3)))).unwrap();
        let mut progress = Progress::new(4);
        let (paths, count) = receive(&have, &mut progress, &rx);
        assert_eq!(count, 4);
        assert_eq!(paths, [std::path::PathBuf::from("/ok")]);
        assert_eq!(
            (
                progress.done,
                progress.cancelled,
                progress.skipped,
                progress.failed
            ),
            (4, 1, 1, 1)
        );
        assert!(have.get(std::path::Path::new("/cancelled")).is_none());
        assert!(have.get(std::path::Path::new("/skipped")).is_none());
        assert!(progress.text().contains('1'));
    }

    #[test]
    #[ignore = "warm-cache producer benchmark; requires OPENFADER_ANALYSIS_BENCH=1 and isolated XDG_DATA_HOME under /tmp"]
    fn warm_cache_submission_and_result_burst_benchmark() {
        assert_eq!(
            std::env::var("OPENFADER_ANALYSIS_BENCH").as_deref(),
            Ok("1")
        );
        let data = std::path::PathBuf::from(
            std::env::var_os("XDG_DATA_HOME").expect("isolated data root"),
        );
        assert!(
            data.starts_with(std::env::temp_dir()),
            "never write fixtures to user data"
        );
        let _pool = job::TEST_RUNS.lock().unwrap_or_else(|e| e.into_inner());
        let root =
            std::env::temp_dir().join(format!("openfader-warm-admission-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let options = options::Options::default();
        let fixture = Analysis {
            beats: vec![openfader::analysis::Beat {
                bar_pos: 1,
                bpm_x100: 12300,
                time_ms: 0,
            }],
            key: Some("8A".into()),
            ..Default::default()
        };
        for total in [10_000, 100_000] {
            let tracks: Arc<Vec<Track>> = Arc::new(
                (0..total)
                    .map(|i| {
                        let path = root.join(format!("{i}.wav"));
                        std::fs::write(&path, b"cache hit needs no decoder").unwrap();
                        let t = Track {
                            id: Some(i as i64 + 1),
                            file: Some(path),
                            ..Default::default()
                        };
                        cache::store(&t, &fixture, options.profile()).unwrap();
                        t
                    })
                    .collect(),
            );
            let capture_started = Instant::now();
            let source = job::Tracks {
                tracks: tracks.clone(),
                rows: Some((0..total).collect()),
                reader: None,
                explicit: false,
                refresh: false,
                keep_cues: true,
                pending: None,
            };
            let snapshot_capture = capture_started.elapsed();
            let settings = Settings {
                options,
                tag: None,
                workers: 4,
                low_priority: false,
                grid_source: GridSource::Ours,
                notice: None,
                learner: None,
            };
            let (tx, rx) = mpsc::channel();
            let store = Arc::new(Store::in_memory().unwrap());
            let started = Instant::now();
            let run = job::spawn_tracks(source, settings, store, tx).unwrap();
            let admission = started.elapsed();
            let outcomes: Vec<_> = rx.iter().collect();
            let produced = started.elapsed();
            assert_eq!(outcomes.len(), total);
            assert!(run.registered());
            for (_, result) in &outcomes {
                assert!(
                    matches!(result, Ok(o) if matches!(o.src, Source::Cache) && !o.cancelled && !o.skipped)
                );
            }
            let (tx, rx) = mpsc::channel();
            for outcome in outcomes {
                tx.send(outcome).unwrap();
            }
            drop(tx);
            let have = Detected::default();
            let mut progress = Progress::new(total);
            let (mut turns, mut longest) = (0, Duration::ZERO);
            let consumer_started = Instant::now();
            while progress.done < total {
                let turn = Instant::now();
                let (_, count) = receive(&have, &mut progress, &rx);
                longest = longest.max(turn.elapsed());
                assert!(count > 0 && count <= UI_BATCH);
                turns += 1;
            }
            assert_eq!(have.version(), total as u64);
            let consumer = consumer_started.elapsed();
            let end_to_end = started.elapsed();
            eprintln!(
                "analysis warm n={total} snapshot_capture={snapshot_capture:?} admission={admission:?} producer={produced:?} consumer={consumer:?} end_to_end={end_to_end:?} consumer_turns={turns} longest_consumer_turn={longest:?}"
            );
            drop(run);
            for track in tracks.iter() {
                let name = format!("{:016x}.bin", cache::fnv1a(store::key(track).bytes()));
                std::fs::remove_file(cache::dir().unwrap().join(name)).unwrap();
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

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
            let jobs = jobs(&SlowLibrary(reader), vec![track].into(), true);
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
                job::spawn_tracks(jobs, settings, Arc::new(Store::in_memory().unwrap()), tx)
                    .unwrap();
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
