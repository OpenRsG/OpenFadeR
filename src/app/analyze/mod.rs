//! Bulk analysis ("Analyze tracks"): a pool of worker threads decodes and analyses library tracks through the
//! persistent cache ([`openfader::analysis::cache`], a hit skips the decode). Which tracks, what is computed and what is
//! done with the result follow Preferences > Analysis ([`super::prefs::analysis`]):
//!
//! * Auto Analysis: at start-up (and on ANALYZE NOW) every track lacking a BPM / grid or a key that the enabled parts
//!   would fill is analysed; with Auto Analysis off nothing runs by itself. [`request`] is the explicit path: the browser's
//!   track menu hands it the selected tracks, which are analysed again whatever the cache holds (tracks with the analysis
//!   lock are skipped).
//! * Track Analysis Setting / mode / precision / BPM range and the cue options reach the analysis code
//!   as [`openfader::analysis::options`], set from the settings by [`sync`] on every change.
//! * Analysis Process: [`workers`] threads, at low priority when power saving ([`job`]).
//! * Results land in [`Detected`] (session map, path → BPM/key); the library's files are never touched except by Key
//!   Analysis "Write the value to the ID3 tag" ([`job`]) and CUE Analysis stores cues in the store ([`cue`]).
//!   Progress shows in the track info panel ([`super::trackinfo`]).
//!
//! Cancelling = dropping the receiver: the workers stop after the track they are on. Dropping the [`Analyzer`] does that.
//!
//! This base owns the types the children share ([`Found`], [`Outcome`]) and the run on the UI thread. Children: `detected`
//! (session results, which tracks are pending), `progress` (counters and text), `job` (worker pool), `cue` (library grid
//! and cue habits), `sync` (settings to options).

mod cue;
mod detected;
mod job;
mod progress;
mod sync;

pub use cue::{Learnt, library_cues};
pub use detected::{Detected, keys_differ};

use super::{
    App, camelot,
    prefs::{
        self,
        analysis::{CueSetting, Process},
    },
    trackinfo,
};
use job::{Guide, Job, Settings, TagText};
use openfader::{
    analysis::{Analysis, cache::Source, options},
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

/// Settings table scope that marks the tracks CUE Analysis set cues on (the CUE Analysis playlist); the row key is
/// [`store::key`].
const MARKER: &str = "cue_analysis";

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
}

struct Ctx {
    app: slint::Weak<App>,
    lib: Rc<dyn Library>,
}

/// A running pool: its timer (dropping it cancels the run) and whether it finished.
struct Run {
    _timer: Rc<Timer>,
    done: Rc<Cell<bool>>,
}

thread_local! {
    static DETECTED: Detected = Detected::default();
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
    static RUNS: RefCell<Vec<Run>> = const { RefCell::new(Vec::new()) };
    static LEARNT: RefCell<Option<Learnt>> = const { RefCell::new(None) };
}

pub fn detected() -> Detected {
    DETECTED.with(Clone::clone)
}

/// BPM of the first beat of the grid and the key.
fn summary(a: &Analysis) -> Found {
    (
        a.beats.first().map_or(0, |b| b.bpm_x100 as i64),
        a.key.clone().unwrap_or_default(),
    )
}

/// Stores `cues` as the whole cue list of `key` and marks the track as cue-analysed ([`MARKER`]; no store API fits better).
fn store_cues(store: &Store, key: &str, cues: &[openfader::analysis::Cue]) -> Result<(), String> {
    store.set_cues(key, Some(cues))?;
    store.set_device_setting(MARKER, key, "1")
}

/// Keys ([`store::key`]) of the tracks CUE Analysis set cues on.
fn analysed(store: &Store) -> HashSet<String> {
    store
        .device_settings(MARKER)
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
    sync::workers(
        process,
        std::thread::available_parallelism().map_or(1, |n| n.get()),
    )
}

/// What CUE Analysis Auto learnt from the collection (empty until Auto is chosen).
pub fn tendency_status() -> Learnt {
    LEARNT.with(|l| l.borrow().clone().unwrap_or_default())
}

/// Keep it alive as long as the window: dropping it cancels every run and forgets the library.
pub struct Analyzer;

impl Drop for Analyzer {
    fn drop(&mut self) {
        RUNS.with(|r| r.borrow_mut().clear());
        CTX.with(|c| *c.borrow_mut() = None);
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
    prefs::on_action("analyze_now", run_pending);
    if prefs::with(|p| p.analysis.auto_analysis) {
        run_pending();
    }
    Analyzer
}

/// The settings as analysis options; chooses CUE Analysis Auto's tendency from the collection when Auto is selected.
fn set_options(p: &prefs::Prefs) {
    let a = &p.analysis;
    if a.cue_mode == CueSetting::Auto {
        if LEARNT.with(|l| l.borrow().is_none()) {
            let lib = CTX.with(|c| c.borrow().as_ref().map(|c| c.lib.clone()));
            if let Some(lib) = lib {
                let learnt = cue::learn(&*lib, &analysed(&store::global()));
                LEARNT.with(|l| *l.borrow_mut() = Some(learnt));
            }
        }
    } else {
        LEARNT.with(|l| *l.borrow_mut() = None);
    }
    options::set(sync::options(
        a,
        LEARNT.with(|l| l.borrow().as_ref().and_then(|l| l.tendency)),
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
    let tracks = match lib.tracks(None) {
        Ok(t) => t,
        Err(e) => {
            return trackinfo::set_progress(&app, &format!("analysis: library query failed: {e}"));
        }
    };
    let todo = detected::pending(&tracks, &detected(), &options::get().parts);
    start_run(&app, jobs(&*lib, &todo, false), prefs::with(settings));
}

/// Explicit analysis (the track menu's ANALYZE TRACK): `tracks` are analysed again, whatever the cache holds and whether
/// the library has their BPM and key; tracks without a file or with the analysis lock are skipped. Does nothing before
/// [`wire`].
pub fn request(tracks: &[Track]) {
    let Some((app, lib)) = context() else { return };
    let refs: Vec<&Track> = tracks.iter().collect();
    start_run(&app, jobs(&*lib, &refs, true), prefs::with(settings));
}

fn settings(p: &prefs::Prefs) -> Settings {
    let (a, notation) = (&p.analysis, p.key_notation);
    Settings {
        options: options::get(),
        tag: (a.key_write_tag && a.part_key)
            .then(|| Arc::new(move |k: &str| camelot::display(k, notation)) as TagText),
        workers: workers(a.process),
        low_priority: a.process == Process::PowerSaving,
    }
}

/// The jobs for `tracks`: what the library knows about each is gathered here, on the UI thread.
fn jobs(lib: &dyn Library, tracks: &[&Track], explicit: bool) -> Vec<Job> {
    let store = store::global();
    tracks
        .iter()
        .filter_map(|t| {
            let (path, key) = (t.file.clone()?, store::key(t));
            if store.analysis_locked(&key).unwrap_or(false) {
                return None;
            }
            let guide = lib
                .analysis(t)
                .map(|a| Guide {
                    has_cues: !a.cues.is_empty(),
                    beats: a.beats,
                    phrases: a.phrases,
                })
                .unwrap_or_default();
            Some(Job {
                path,
                key,
                explicit,
                library_key: !t.key.is_empty(),
                guide,
            })
        })
        .collect()
}

/// Starts the pool on `jobs` and a timer that collects its results into [`Detected`] and the progress text.
fn start_run(app: &App, jobs: Vec<Job>, settings: Settings) {
    if jobs.is_empty() {
        return trackinfo::set_progress(app, "");
    }
    let have = detected();
    let mut progress = Progress::new(jobs.len());
    trackinfo::set_progress(app, &progress.text());
    let (tx, rx) = mpsc::channel();
    job::spawn(jobs, settings, store::global(), tx);
    let (timer, done) = (Rc::new(Timer::default()), Rc::new(Cell::new(false)));
    let (weak, weak_timer, finished) = (app.as_weak(), Rc::downgrade(&timer), done.clone());
    timer.start(TimerMode::Repeated, TICK, move || {
        let Some(app) = weak.upgrade() else { return };
        for (path, r) in rx.try_iter() {
            progress.done += 1;
            match r {
                Ok(o) => {
                    progress.record(&o);
                    have.insert(path.clone(), o.found);
                    trackinfo::detected_changed(&app, &path);
                }
                Err(e) => {
                    progress.failed += 1;
                    eprintln!("analyze: {e}");
                }
            }
        }
        trackinfo::set_progress(&app, &progress.text());
        if progress.done == progress.total {
            finished.set(true);
            weak_timer.upgrade().inspect(|t| t.stop());
        }
    });
    RUNS.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|x| !x.done.get());
        r.push(Run {
            _timer: timer,
            done,
        });
    });
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
        store_cues(&store, "a.mp3|10", &[cue(0, 0), cue(1, 8000)]).unwrap();
        assert_eq!(store.edits("a.mp3|10").unwrap().cues.unwrap().len(), 2);
        assert_eq!(analysed(&store), HashSet::from(["a.mp3|10".to_owned()]));
        assert!(
            store.edits("b.mp3|10").unwrap().cues.is_none(),
            "other tracks are untouched"
        );
    }
}
