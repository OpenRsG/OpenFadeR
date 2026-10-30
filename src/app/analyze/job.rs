//! Process-wide bulk scheduling and per-request completion. Compatible queued or in-flight jobs share decoding and
//! analysis; every subscriber keeps its own cue guide, tag settings, cancellation and result channel.

mod submission;
use super::{Outcome, summary};
use openfader::{
    analysis::{
        Analysis, Beat, Phrases, auto,
        cache::{self, Source},
        options::Options,
        tag,
    },
    library::{AnalysisReader, Track},
    store::Store,
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Condvar, LazyLock, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};
#[cfg(test)]
use submission::spawn;
pub(super) use submission::{Tracks, publication, spawn_tracks};

const MAX_RUNS: usize = 8;
const COMMIT_KEYS: usize = 8192;

#[cfg(test)]
pub(super) static TEST_RUNS: Mutex<()> = Mutex::new(());

type Commits = cache::CommitFence;

fn commits() -> &'static Mutex<Commits> {
    static COMMITS: LazyLock<Mutex<Commits>> =
        LazyLock::new(|| Mutex::new(Commits::new(COMMIT_KEYS)));
    &COMMITS
}

/// Library grid and phrases, prepared only when a pool worker has capacity.
#[derive(Default)]
pub struct Guide {
    pub beats: Vec<Beat>,
    pub phrases: Option<Phrases>,
    /// Includes an explicitly stored empty list: the user deleted every cue.
    pub has_cues: bool,
    pub cue_revision: i64,
}

/// The track a job analyses: a row of the submitted list (shared, never copied per job) until preparation swaps in the
/// Collection's listing of it ([`super::listed`]).
#[derive(Clone)]
pub enum Subject {
    Row(Arc<Vec<Track>>, usize),
    Listed(Arc<Track>),
}

impl std::ops::Deref for Subject {
    type Target = Track;
    fn deref(&self) -> &Track {
        match self {
            Self::Row(list, i) => &list[*i],
            Self::Listed(t) => t,
        }
    }
}

impl From<Track> for Subject {
    fn from(t: Track) -> Self {
        Self::Listed(Arc::new(t))
    }
}

pub struct Job {
    pub path: PathBuf,
    /// What the analysis is of: its identity names the stored entry ([`cache`]).
    pub track: Subject,
    /// [`openfader::store::key`] of the track.
    pub key: String,
    /// Asked for by the user (the track menu): analysed again whatever the cache holds, and its key is written to the tag
    /// even when the library has one.
    pub explicit: bool,
    /// Analysed again because an update changed the analysis ([`super::stale`]): the cache entry is replaced, but a cue list
    /// the track has stays whatever the settings say, and no tag is written.
    pub refresh: bool,
    /// The track came with cues of its own (an import): the cache entry is made, but CUE Analysis stores no cues, whatever
    /// "Prohibit overwriting CUEs" says and whether the track has cues yet. The grid is never written here, and the key tag
    /// only for a track the library gave no key.
    pub keep_cues: bool,
    /// The library gave the track a key.
    pub library_key: bool,
    pub guide: Guide,
    pub reader: Option<AnalysisReader>,
    pub generation: u64,
    pub(super) ticket: Option<Arc<Mutex<Generations>>>,
    pub cue_epoch: u64,
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
    pub grid_source: crate::app::prefs::analysis::GridSource,
    pub(super) notice: Option<mpsc::Sender<Notice>>,
    pub(super) learner: Option<Arc<super::Learner>>,
}

pub(super) struct Notice {
    pub ticket: Option<Arc<Mutex<Generations>>>,
    pub generation: u64,
    pub text: String,
}

fn notice(sub: &Subscriber, text: String) {
    if !sub.cancelled.load(Ordering::Relaxed) {
        if let Some(tx) = &sub.settings.notice {
            let _ = tx.send(Notice {
                ticket: sub.job.ticket.clone(),
                generation: sub.job.generation,
                text,
            });
        }
    }
}

/// The cues and tag of one analysed track (`a`), the part of [`spawn_tracks`]'s work that is not decoding.
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
    // A refresh never replaces a cue list, whatever "Prohibit overwriting CUEs" says.
    let cue_options = auto::CueOptions {
        keep_existing: s.options.cue.keep_existing || job.refresh,
        ..s.options.cue
    };
    let cues = !job.keep_cues
        && auto::for_analysis(beats, phrases.as_ref(), &cue_options, g.has_cues).is_some_and(|c| {
            match store.system(|| store.set_auto_cues_if_revision(&job.key, &c, g.cue_revision)) {
                Ok(stored) => stored,
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
        generation: job.generation,
        skipped: false,
        cancelled: false,
        ticket: job.ticket.clone(),
    }
}

/// Lowers the calling thread's priority: nice 19 on Linux (niceness is per thread there), the background QoS class on macOS, the
/// lowest thread priority on Windows.
pub(in crate::app) fn lower_priority() {
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

pub struct Cancellation {
    cancelled: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
    registered: Arc<AtomicBool>,
}

impl Cancellation {
    pub fn total(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    pub fn registered(&self) -> bool {
        self.registered.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn test(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancelled,
            total: Arc::new(AtomicUsize::new(0)),
            registered: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        pool()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel(&self.cancelled);
        wake().notify_all();
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
    track: Subject,
    path: PathBuf,
    key: String,
    explicit: bool,
    source: Option<String>,
    options: Options,
    low_priority: bool,
    generation: u64,
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
pub(super) struct Generations {
    completed: u64,
    pending: Vec<(u64, Weak<AtomicBool>)>,
}

impl Generations {
    fn latest(&mut self) -> u64 {
        self.pending.retain(|(_, run)| {
            run.upgrade()
                .is_some_and(|run| !run.load(Ordering::Relaxed))
        });
        self.pending
            .last()
            .map_or(self.completed, |(generation, _)| {
                self.completed.max(*generation)
            })
    }
}

#[derive(Default)]
struct Pool {
    queued: VecDeque<Arc<Work>>,
    running: Vec<Arc<Work>>,
    by_track: HashMap<String, Vec<Weak<Work>>>,
    limit: usize,
    pending: VecDeque<Subscriber>,
    preparing: Vec<(String, PathBuf)>,
    prepared: usize,
    threads: [usize; 2],
    sequence: u64,
    generations: HashMap<PathBuf, Weak<Mutex<Generations>>>,
    generation_sweep: VecDeque<PathBuf>,
    staging: VecDeque<submission::Submission>,
    staging_active: bool,
    admitted: VecDeque<submission::Submission>,
    retired: Vec<submission::Submission>,
    learning: VecDeque<(Arc<super::Learner>, Arc<Store>, bool)>,
    learning_running: usize,
    runs: Vec<Weak<AtomicBool>>,
}

impl Pool {
    fn admit(&mut self, cancelled: &Arc<AtomicBool>, workers: usize) -> Result<(), String> {
        self.runs.retain(|run| {
            run.upgrade()
                .is_some_and(|run| !run.load(Ordering::Relaxed))
        });
        if self.runs.len() >= workers.max(1).saturating_mul(2).min(MAX_RUNS) {
            return Err(tr!(
                "Analysis is busy: wait for an existing analysis run to finish or cancel it."
            )
            .to_owned());
        }
        self.runs.push(Arc::downgrade(cancelled));
        Ok(())
    }

    fn cancel(&mut self, cancelled: &Arc<AtomicBool>) {
        self.cancel_submissions(cancelled);
        self.pending
            .retain(|s| !Arc::ptr_eq(&s.cancelled, cancelled));
        for work in &self.queued {
            let mut subscribers = work.subscribers.lock().unwrap_or_else(|e| e.into_inner());
            let before = subscribers.len();
            subscribers.retain(|s| !Arc::ptr_eq(&s.cancelled, cancelled));
            self.prepared -= before - subscribers.len();
        }
        let mut index = 0;
        while index < self.queued.len() {
            if self.queued[index].cancelled() {
                let work = self.queued.remove(index).unwrap();
                self.forget(&work);
            } else {
                index += 1;
            }
        }
    }

    fn grow(&mut self, low_priority: bool) -> usize {
        let count = &mut self.threads[usize::from(low_priority)];
        let needed = self.limit.saturating_sub(*count);
        *count += needed;
        needed
    }

    fn configure(&mut self, workers: usize, low_priority: bool) {
        self.limit = workers.max(1);
        for _ in 0..self.grow(low_priority) {
            std::thread::spawn(move || worker(low_priority));
        }
    }

    fn submit(&mut self, subscriber: Subscriber) {
        self.prepared += 1;
        let work = self
            .by_track
            .get(&subscriber.job.key)
            .and_then(|works| works.last())
            .and_then(Weak::upgrade)
            .filter(|w| w.compatible(&subscriber) && !w.cancelled());
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
            generation: subscriber.job.generation,
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

    #[cfg(test)]
    fn take_ready(&mut self) -> Vec<Arc<Work>> {
        let mut ready = Vec::new();
        while self.running.len() < self.limit {
            let Some(index) = self.queued.iter().position(|work| {
                work.cancelled() || !self.running.iter().any(|running| running.key == work.key)
            }) else {
                break;
            };
            let work = self.queued.remove(index).unwrap();
            if work.cancelled() {
                self.prepared -= work
                    .subscribers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .len();
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

fn wake() -> &'static Condvar {
    static WAKE: Condvar = Condvar::new();
    &WAKE
}

fn prepare(sub: &mut Subscriber) -> Result<bool, String> {
    let job = &mut sub.job;
    if let Some(t) = super::listed(&sub.store, &job.track) {
        job.track = t.into();
    }
    job.path = job
        .track
        .file
        .clone()
        .ok_or_else(|| "track has no file".to_owned())?;
    job.key = openfader::store::key(&job.track);
    if sub.store.analysis_locked(&job.key)? {
        notice(
            sub,
            tr!("this track has an Analysis Lock: unlock it first").to_owned(),
        );
        return Ok(false);
    }
    if let Some(learner) = &sub.settings.learner {
        sub.settings.options.cue.tendency = learner.learned(&sub.store)?.tendency;
    }
    job.library_key = !job.track.key.is_empty();
    job.guide.cue_revision = sub.store.cue_revision(&job.key)?;
    if u64::try_from(job.guide.cue_revision)
        .map_err(|_| "negative cue revision in store".to_owned())?
        > job.cue_epoch
    {
        job.keep_cues = true;
    }
    let mut own = job
        .reader
        .as_ref()
        .and_then(|read| read(&*job.track))
        .unwrap_or_default();
    let mut edits = sub.store.edits(&job.key)?;
    let edited_grid = edits.grid.is_some();
    job.guide.has_cues = edits.cues.take().is_some() || !own.cues.is_empty();
    openfader::store::apply(&mut own, &edits);
    job.guide.beats = own.beats;
    job.guide.phrases = own.phrases;
    sub.source = cache::key(&job.path).ok();
    if sub.settings.notice.is_some() {
        let tail = super::notice_for(
            sub.settings.grid_source,
            !job.guide.beats.is_empty(),
            edited_grid,
        );
        notice(sub, tr!("analysing “{}”…{}", sub.job.track.title, tail));
    }
    Ok(true)
}

enum Task {
    Stage(submission::Submission),
    Discard(submission::Submission),
    Prepare(Subscriber, (String, PathBuf)),
    Analyze(Arc<Work>),
    Learn(Arc<super::Learner>, Arc<Store>),
}

impl Pool {
    fn next(&mut self, low_priority: bool) -> Option<Task> {
        if let Some(submission) = self.retired.pop() {
            return Some(Task::Discard(submission));
        }
        if !self.staging_active {
            if let Some(submission) = self.staging.pop_front() {
                self.staging_active = true;
                return Some(Task::Stage(submission));
            }
        }
        self.refill();
        // One key stays ordered through preparation, cache commit, cue/tag writes and result delivery.
        if self.running.len() + self.learning_running < self.limit {
            self.learning
                .retain(|(learner, _, _)| Arc::strong_count(learner) > 1);
            if let Some(index) = self
                .learning
                .iter()
                .position(|(_, _, low)| *low == low_priority)
            {
                let (learner, store, _) = self.learning.remove(index).unwrap();
                self.learning_running += 1;
                return Some(Task::Learn(learner, store));
            }
            let index = self.queued.iter().enumerate().position(|(index, work)| {
                work.low_priority == low_priority
                    && !self
                        .running
                        .iter()
                        .any(|running| running.key == work.key || running.path == work.path)
                    && !self
                        .queued
                        .iter()
                        .take(index)
                        .any(|older| older.key == work.key || older.path == work.path)
            });
            if let Some(index) = index {
                let work = self.queued.remove(index).unwrap();
                self.running.push(work.clone());
                return Some(Task::Analyze(work));
            }
        }
        while self
            .pending
            .front()
            .is_some_and(|s| s.cancelled.load(Ordering::Relaxed))
        {
            self.pending.pop_front();
        }
        if self.prepared + self.preparing.len() >= self.limit.max(1) * 3 {
            return None;
        }
        let index = self.pending.iter().enumerate().position(|(index, s)| {
            (s.settings.low_priority == low_priority || s.cancelled.load(Ordering::Relaxed))
                && s.settings
                    .learner
                    .as_ref()
                    .is_none_or(|learner| learner.result.get().is_some())
                && !self
                    .preparing
                    .iter()
                    .any(|(key, path)| key == &s.job.key || path == &s.job.path)
                && !self
                    .pending
                    .iter()
                    .take(index)
                    .any(|older| older.job.key == s.job.key || older.job.path == s.job.path)
        })?;
        let sub = self.pending.remove(index).unwrap();
        let identity = (sub.job.key.clone(), sub.job.path.clone());
        self.preparing.push(identity.clone());
        Some(Task::Prepare(sub, identity))
    }
}

fn worker(low_priority: bool) {
    if low_priority {
        lower_priority();
    }
    loop {
        let task = {
            let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(task) = pool.next(low_priority) {
                    break task;
                }
                pool = wake().wait(pool).unwrap_or_else(|e| e.into_inner());
            }
        };
        match task {
            Task::Discard(submission) => drop(submission),
            Task::Stage(mut submission) => {
                submission.reserve();
                let retired = {
                    let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
                    pool.stage(submission)
                };
                drop(retired);
                wake().notify_all();
            }
            Task::Learn(learner, store) => {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    learner.learned(&store)
                }))
                .is_err()
                {
                    let _ = learner
                        .result
                        .set(Err("cue tendency learning panicked".into()));
                }
                let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
                pool.learning_running -= 1;
                wake().notify_all();
            }
            Task::Prepare(mut sub, identity) => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if sub.cancelled.load(Ordering::Relaxed) {
                        Ok(false)
                    } else {
                        prepare(&mut sub)
                    }
                }))
                .unwrap_or_else(|_| Err("analysis preparation panicked".into()));
                let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
                pool.preparing.retain(|i| i != &identity);
                if !sub.cancelled.load(Ordering::Relaxed) {
                    match result {
                        Ok(true) => pool.submit(sub),
                        Ok(false) => {
                            let _ = sub.tx.send((
                                sub.job.path,
                                Ok(Outcome {
                                    found: (0, String::new()),
                                    src: Source::Cache,
                                    cues: false,
                                    tag: None,
                                    generation: sub.job.generation,
                                    skipped: true,
                                    cancelled: false,
                                    ticket: sub.job.ticket,
                                }),
                            ));
                        }
                        Err(e) => {
                            let _ = sub.tx.send((sub.job.path, Err(e)));
                        }
                    }
                }
                wake().notify_all();
            }
            Task::Analyze(work) => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cache::get_or_analyze_with(
                        &work.track,
                        &work.options,
                        work.explicit,
                        &|| work.cancelled(),
                        &|key, write| {
                            if work.cancelled() {
                                return false;
                            }
                            commits().lock().unwrap_or_else(|e| e.into_inner()).publish(
                                key,
                                work.generation,
                                write,
                            )
                        },
                    )
                }));
                let subscribers = {
                    let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
                    let subscribers = std::mem::take(
                        &mut *work.subscribers.lock().unwrap_or_else(|e| e.into_inner()),
                    );
                    pool.forget(&work);
                    subscribers
                };
                let prepared = subscribers.len();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match result {
                    Ok(Ok(Some(result))) => deliver(subscribers, Ok(result)),
                    Ok(Ok(None)) => deliver_cancelled(subscribers),
                    Ok(Err(e)) => deliver(subscribers, Err(e)),
                    Err(_) => deliver(subscribers, Err("analysis worker panicked".into())),
                }));
                let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
                pool.prepared -= prepared;
                pool.running.retain(|w| !Arc::ptr_eq(w, &work));
                wake().notify_all();
            }
        }
    }
}

fn deliver_cancelled(subscribers: Vec<Subscriber>) {
    for sub in subscribers {
        if !sub.cancelled.load(Ordering::Relaxed) {
            let _ = sub.tx.send((
                sub.job.path,
                Ok(Outcome {
                    found: (0, String::new()),
                    src: Source::Cache,
                    cues: false,
                    tag: None,
                    generation: sub.job.generation,
                    ticket: sub.job.ticket,
                    skipped: false,
                    cancelled: true,
                }),
            ));
        }
    }
}

fn deliver(subscribers: Vec<Subscriber>, result: Result<(Analysis, Source), String>) {
    for sub in subscribers {
        if sub.cancelled.load(Ordering::Relaxed) {
            continue;
        }
        // Library health lists a file whose analysis failed as corrupt, until it analyses or loads again.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &result {
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
        }))
        .unwrap_or_else(|_| Err("analysis completion panicked".into()));
        if sub.cancelled.load(Ordering::Relaxed) {
            continue;
        }
        if let Some(ticket) = &sub.job.ticket {
            let mut generations = ticket.lock().unwrap_or_else(|e| e.into_inner());
            generations.completed = generations.completed.max(sub.job.generation);
        }
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

pub(super) fn current(ticket: Option<&Arc<Mutex<Generations>>>, generation: u64) -> bool {
    ticket.is_none_or(|ticket| {
        generation >= ticket.lock().unwrap_or_else(|e| e.into_inner()).latest()
    })
}

pub(super) fn learn(
    learner: Arc<super::Learner>,
    store: Arc<Store>,
    workers: usize,
    low_priority: bool,
) {
    let mut pool = pool().lock().unwrap_or_else(|e| e.into_inner());
    pool.configure(workers, low_priority);
    pool.learning
        .retain(|(learner, _, _)| Arc::strong_count(learner) > 1);
    pool.learning.push_back((learner, store, low_priority));
    wake().notify_all();
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
            }
            .into(),
            key: format!("{path}|1"),
            explicit: false,
            refresh: false,
            keep_cues: false,
            library_key: false,
            guide,
            reader: None,
            generation: 0,
            ticket: None,
            cue_epoch: u64::MAX,
        }
    }

    #[test]
    fn a_row_subject_shares_the_submitted_list() {
        let list = Arc::new(vec![
            Track::default(),
            Track {
                title: "b".into(),
                ..Track::default()
            },
        ]);
        let subject = Subject::Row(list.clone(), 1);
        assert_eq!(subject.title, "b");
        let copy = subject.clone();
        assert_eq!(
            Arc::strong_count(&list),
            3,
            "cloned by reference, not by value"
        );
        drop(copy);
    }

    fn settings(tag: bool) -> Settings {
        Settings {
            options: Options::default(),
            tag: tag.then(|| Arc::new(|k: &str| format!("<{k}>")) as TagText),
            workers: 1,
            low_priority: false,
            grid_source: crate::app::prefs::analysis::GridSource::Library,
            notice: None,
            learner: None,
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
        j.guide.cue_revision = store.cue_revision(&j.key).unwrap();
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
    fn keep_cues_mode_never_stores_cues_and_leaves_the_grid_and_library_key_alone() {
        let store = Store::in_memory().unwrap();
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
        for has_cues in [true, false] {
            let mut j = job(
                &format!("/m/kept{has_cues}.mp3"),
                Guide {
                    has_cues,
                    ..Default::default()
                },
            );
            j.keep_cues = true;
            j.library_key = true;
            if has_cues {
                store.set_cues(&j.key, Some(&mine)).unwrap();
                store.set_grid(&j.key, &grid, &beats()).unwrap();
            }
            // Overwriting CUEs is allowed and the key tag is asked for: this mode stores no cues and writes no tag over the library's key.
            let out = finish(&j, &analysed(), src(), &settings(true), &store);
            assert!(!out.cues && out.tag.is_none());
            let edits = store.edits(&j.key).unwrap();
            if has_cues {
                assert_eq!(edits.cues, Some(mine.clone()));
                assert_eq!(edits.grid, Some(grid.clone()));
            } else {
                assert!(edits.cues.is_none());
            }
        }
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
        let _pool = TEST_RUNS.lock().unwrap_or_else(|e| e.into_inner());
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
        let cancel = Cancellation::test(cancelled.cancelled.clone());
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
        let cancelled = Cancellation::test(sub.cancelled.clone());
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
    #[test]
    fn edited_existing_cues_and_explicit_deletions_win_over_generated_cues() {
        let store = Store::in_memory().unwrap();
        let original = vec![openfader::analysis::Cue {
            hot_cue: 1,
            time_ms: 1000,
            loop_end_ms: None,
            color: None,
            comment: None,
        }];
        let mut edited = original.clone();
        edited[0].time_ms = 9000;
        for replacement in [Some(edited), Some(Vec::new()), None, Some(original.clone())] {
            let mut j = job(
                "/m/cue-revisions.mp3",
                Guide {
                    has_cues: true,
                    ..Default::default()
                },
            );
            store.set_cues(&j.key, Some(&original)).unwrap();
            j.guide.cue_revision = store.cue_revision(&j.key).unwrap();
            store.set_cues(&j.key, replacement.as_deref()).unwrap();
            assert!(!finish(&j, &analysed(), src(), &settings(false), &store).cues);
            assert_eq!(store.edits(&j.key).unwrap().cues, replacement);
        }
    }

    #[test]
    fn cue_revision_comparison_and_commit_cannot_race_a_user_write() {
        use std::sync::Barrier;
        for _ in 0..16 {
            let store = Arc::new(Store::in_memory().unwrap());
            let gate = Arc::new(Barrier::new(2));
            let mine = vec![openfader::analysis::Cue {
                hot_cue: 1,
                time_ms: 9000,
                loop_end_ms: None,
                color: None,
                comment: None,
            }];
            let worker_store = store.clone();
            let worker_gate = gate.clone();
            let worker = std::thread::spawn(move || {
                worker_gate.wait();
                worker_store
                    .set_auto_cues_if_revision("race", &[], 0)
                    .unwrap()
            });
            gate.wait();
            store.set_cues("race", Some(&mine)).unwrap();
            worker.join().unwrap();
            assert_eq!(store.edits("race").unwrap().cues, Some(mine));
        }
    }

    #[test]
    fn large_submissions_bound_prepared_payloads_and_reuse_worker_lanes() {
        let store = Arc::new(Store::in_memory().unwrap());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel();
        let mut pool = Pool {
            limit: 2,
            ..Default::default()
        };
        assert_eq!(pool.grow(false), 2);
        assert_eq!(pool.grow(true), 2);
        for i in 0..50_000 {
            pool.pending.push_back(Subscriber {
                job: job(&format!("/missing/large-{i}.wav"), Guide::default()),
                source: None,
                settings: settings(false),
                store: store.clone(),
                tx: tx.clone(),
                cancelled: cancelled.clone(),
            });
            assert_eq!(pool.grow(false), 0, "submissions reuse existing workers");
        }
        for _ in 0..2 {
            let Some(Task::Prepare(sub, identity)) = pool.next(false) else {
                panic!("preparation")
            };
            pool.preparing.retain(|i| i != &identity);
            pool.submit(sub);
            assert!(matches!(pool.next(false), Some(Task::Analyze(_))));
        }
        for _ in 0..4 {
            let Some(Task::Prepare(mut sub, identity)) = pool.next(false) else {
                panic!("bounded preparation")
            };
            sub.job.guide.beats = beats();
            pool.preparing.retain(|i| i != &identity);
            pool.submit(sub);
        }
        assert_eq!(pool.prepared, 6);
        assert!(
            pool.next(false).is_none(),
            "no more heavy guides while the budget is full"
        );
        assert_eq!(pool.pending.len(), 49_994);
        cancelled.store(true, Ordering::Relaxed);
        pool.cancel(&cancelled);
        assert!(pool.pending.is_empty() && pool.queued.is_empty());
        assert_eq!(pool.prepared, 2, "only bounded in-flight work remains");
        assert_eq!(pool.threads, [2, 2]);
    }

    #[test]
    fn incompatible_same_track_work_is_fifo_across_priority_lanes() {
        let mut pool = Pool {
            limit: 2,
            ..Default::default()
        };
        let mut low = settings(false);
        low.low_priority = true;
        pool.submit(subscriber("ordered", true, low).0);
        let mut changed = settings(false);
        changed.options.grid.bpm_range = (110, 220);
        pool.submit(subscriber("ordered", true, changed).0);
        pool.submit(subscriber("other-track", true, settings(false)).0);
        let Some(Task::Analyze(other)) = pool.next(false) else {
            panic!("other tracks remain runnable")
        };
        assert_eq!(other.path, PathBuf::from("other-track"));
        let Some(Task::Analyze(old)) = pool.next(true) else {
            panic!("old settings first")
        };
        assert_eq!(old.path, PathBuf::from("ordered"));
        pool.running.retain(|w| !Arc::ptr_eq(w, &other));
        assert!(
            pool.next(false).is_none(),
            "new settings wait through old cache/cue commit"
        );
        pool.running.retain(|w| !Arc::ptr_eq(w, &old));
        pool.forget(&old);
        let Some(Task::Analyze(new)) = pool.next(false) else {
            panic!("new settings after old")
        };
        assert_eq!(new.options.grid.bpm_range, (110, 220));
    }

    #[test]
    fn preparation_preserves_submission_order_across_priority_lanes() {
        let mut pool = Pool {
            limit: 2,
            ..Default::default()
        };
        let mut low = settings(false);
        low.low_priority = true;
        pool.pending
            .push_back(subscriber("ordered-raw", true, low).0);
        pool.pending
            .push_back(subscriber("ordered-raw", true, settings(false)).0);
        pool.pending
            .push_back(subscriber("unrelated-raw", true, settings(false)).0);
        let Some(Task::Prepare(other, _)) = pool.next(false) else {
            panic!("unrelated preparation")
        };
        assert_eq!(other.job.path, PathBuf::from("unrelated-raw"));
        assert!(
            pool.next(false).is_none(),
            "normal priority must not overtake an older same-track request"
        );
        let Some(Task::Prepare(old, identity)) = pool.next(true) else {
            panic!("old preparation")
        };
        assert_eq!(old.job.path, PathBuf::from("ordered-raw"));
        pool.preparing.retain(|i| i != &identity);
        pool.submit(old);
        let Some(Task::Prepare(new, _)) = pool.next(false) else {
            panic!("new preparation")
        };
        assert_eq!(new.job.path, PathBuf::from("ordered-raw"));
    }

    #[test]
    fn generation_tickets_reject_stale_ui_results_and_release_cancelled_intent() {
        let old = Arc::new(AtomicBool::new(false));
        let new = Arc::new(AtomicBool::new(false));
        let ticket = Arc::new(Mutex::new(Generations {
            completed: 1,
            pending: vec![(1, Arc::downgrade(&old)), (2, Arc::downgrade(&new))],
        }));
        assert!(
            !current(Some(&ticket), 1),
            "latest intent wins before its work finishes"
        );
        new.store(true, Ordering::Relaxed);
        assert!(
            current(Some(&ticket), 1),
            "a cancelled subscriber does not suppress a live run"
        );
        ticket.lock().unwrap().completed = 2;
        assert!(
            !current(Some(&ticket), 1),
            "late timer delivery cannot overwrite newer detection"
        );
        let weak = Arc::downgrade(&ticket);
        drop(ticket);
        assert!(
            weak.upgrade().is_none(),
            "tickets retain no track or analysis after results are dropped"
        );
    }
    #[test]
    fn worker_preparation_panics_release_capacity_and_complete_the_run() {
        let _pool = TEST_RUNS.lock().unwrap_or_else(|e| e.into_inner());
        let mut bad = job("/missing/panic-reader.wav", Guide::default());
        bad.reader = Some(Arc::new(|_| panic!("test source panic")));
        let good = job("/missing/after-reader-panic.wav", Guide::default());
        let (tx, rx) = mpsc::channel();
        let _run = spawn(
            vec![bad, good],
            settings(false),
            Arc::new(Store::in_memory().unwrap()),
            tx,
        );
        for _ in 0..2 {
            assert!(
                rx.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
                    .1
                    .is_err()
            );
        }
    }
    #[test]
    fn cue_edits_while_waiting_for_preparation_are_preserved_but_older_generated_cues_are_replaceable()
     {
        for replacement in [Some(Vec::new()), None] {
            let (mut sub, _rx) = subscriber("/missing/queued-cue-edit.wav", true, settings(false));
            sub.job.cue_epoch = sub.store.cue_generation();
            let key = openfader::store::key(&sub.job.track);
            sub.store.set_cues(&key, replacement.as_deref()).unwrap();
            assert!(prepare(&mut sub).unwrap());
            assert!(
                sub.job.keep_cues,
                "a queued user edit is newer than the request"
            );
            assert!(!finish(&sub.job, &analysed(), src(), &sub.settings, &sub.store).cues);
            assert_eq!(sub.store.edits(&key).unwrap().cues, replacement);
        }
        let (mut sub, _rx) =
            subscriber("/missing/queued-generated-cues.wav", true, settings(false));
        sub.job.cue_epoch = sub.store.cue_generation();
        let key = openfader::store::key(&sub.job.track);
        sub.store.set_auto_cues(&key, &[]).unwrap();
        assert!(prepare(&mut sub).unwrap());
        assert!(
            !sub.job.keep_cues,
            "an older analysis does not masquerade as a user edit"
        );
        assert!(finish(&sub.job, &analysed(), src(), &sub.settings, &sub.store).cues);
    }
    #[test]
    fn undoing_cues_during_analysis_or_queued_preparation_advances_the_user_revision() {
        let (mut sub, _rx) = subscriber("/missing/undo-cue-edit.wav", true, settings(false));
        let key = openfader::store::key(&sub.job.track);
        let mine = vec![openfader::analysis::Cue {
            hot_cue: 1,
            time_ms: 9000,
            loop_end_ms: None,
            color: None,
            comment: None,
        }];
        sub.store.set_cues(&key, Some(&mine)).unwrap();
        sub.job.cue_epoch = sub.store.cue_generation();
        assert!(prepare(&mut sub).unwrap());
        let revision = sub.job.guide.cue_revision;
        assert!(sub.store.undo().unwrap().is_some());
        assert!(sub.store.cue_revision(&key).unwrap() > revision);
        assert!(!finish(&sub.job, &analysed(), src(), &sub.settings, &sub.store).cues);
        assert!(prepare(&mut sub).unwrap());
        assert!(
            sub.job.keep_cues,
            "undo while waiting for a preparation slot is also a user edit"
        );
        assert!(!finish(&sub.job, &analysed(), src(), &sub.settings, &sub.store).cues);
        assert!(sub.store.edits(&key).unwrap().cues.is_none());
    }
    #[test]
    fn auto_learning_runs_once_on_the_shared_pool_before_cue_settings_are_prepared() {
        let _pool = TEST_RUNS.lock().unwrap_or_else(|e| e.into_inner());
        use std::sync::atomic::AtomicUsize;
        let reads = Arc::new(AtomicUsize::new(0));
        let source_reads = reads.clone();
        let learner = Arc::new(super::super::Learner {
            tracks: Arc::new(
                (0..40)
                    .map(|i| Track {
                        title: format!("teacher-{i}"),
                        ..Default::default()
                    })
                    .collect(),
            ),
            reader: Some(Arc::new(move |_| {
                source_reads.fetch_add(1, Ordering::Relaxed);
                Some(Analysis {
                    beats: beats(),
                    cues: [8000, 40000, 72000]
                        .into_iter()
                        .enumerate()
                        .map(|(i, time_ms)| openfader::analysis::Cue {
                            hot_cue: i as u16 + 1,
                            time_ms,
                            loop_end_ms: None,
                            color: None,
                            comment: None,
                        })
                        .collect(),
                    ..Default::default()
                })
            })),
            result: std::sync::OnceLock::new(),
        });
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        let store = Arc::new(Store::in_memory().unwrap());
        learn(learner.clone(), store.clone(), 2, false);
        let mut auto_settings = settings(false);
        auto_settings.workers = 2;
        auto_settings.learner = Some(learner.clone());
        auto_settings.options.cue.mode = auto::CueMode::Auto;
        let (tx, rx) = mpsc::channel();
        let _run = spawn(
            (0..4)
                .map(|i| job(&format!("/missing/learned-{i}.wav"), Guide::default()))
                .collect(),
            auto_settings.clone(),
            store,
            tx,
        );
        for _ in 0..4 {
            assert!(
                rx.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
                    .1
                    .is_err()
            );
        }
        let learned = learner.result.get().unwrap().as_ref().unwrap();
        assert_eq!(learned.found, 40);
        assert_eq!(learned.tendency.unwrap().hot, 3);
        assert_eq!(
            reads.load(Ordering::Relaxed),
            40,
            "runs share one collection scan, not a scan per track"
        );
        let (mut sub, _rx) = subscriber("/missing/learned-options.wav", true, auto_settings);
        assert!(prepare(&mut sub).unwrap());
        assert_eq!(sub.settings.options.cue.tendency, learned.tendency);
        let mut manual = settings(false);
        manual.options.cue.hot = 5;
        let (mut sub, _rx) = subscriber("/missing/manual-options.wav", true, manual);
        assert!(prepare(&mut sub).unwrap());
        assert_eq!(sub.settings.options.cue.hot, 5);
        assert!(sub.settings.options.cue.tendency.is_none());
    }

    #[test]
    fn collection_learning_and_analysis_share_the_same_execution_budget() {
        let mut pool = Pool {
            limit: 1,
            ..Default::default()
        };
        pool.submit(subscriber("running-before-learner", false, settings(false)).0);
        let _running = pool.take_ready();
        let learner = Arc::new(super::super::Learner {
            tracks: Arc::new(Vec::new()),
            reader: None,
            result: std::sync::OnceLock::new(),
        });
        pool.learning.push_back((
            learner.clone(),
            Arc::new(Store::in_memory().unwrap()),
            false,
        ));
        pool.submit(subscriber("after-learner", false, settings(false)).0);
        assert!(
            pool.next(false).is_none(),
            "a collection learner cannot exceed the running bulk budget"
        );
        pool.running.clear();
        assert!(matches!(pool.next(false), Some(Task::Learn(_, _))));
        assert!(
            pool.next(false).is_none(),
            "bulk work waits for the learner's execution slot"
        );
    }
    #[test]
    fn an_explicitly_empty_cue_list_is_existing_for_refresh_and_no_overwrite() {
        let (mut sub, _rx) = subscriber("/missing/explicit-empty.wav", true, settings(false));
        let key = openfader::store::key(&sub.job.track);
        sub.store.set_cues(&key, Some(&[])).unwrap();
        sub.job.cue_epoch = sub.store.cue_generation();
        assert!(prepare(&mut sub).unwrap());
        assert!(sub.job.guide.has_cues);
        sub.job.refresh = true;
        assert!(!finish(&sub.job, &analysed(), src(), &sub.settings, &sub.store).cues);
        sub.job.refresh = false;
        sub.settings.options.cue.keep_existing = true;
        assert!(!finish(&sub.job, &analysed(), src(), &sub.settings, &sub.store).cues);
        assert_eq!(sub.store.edits(&key).unwrap().cues, Some(Vec::new()));
        sub.settings.options.cue.keep_existing = false;
        assert!(finish(&sub.job, &analysed(), src(), &sub.settings, &sub.store).cues);
    }
    #[test]
    fn a_late_bare_path_alias_cannot_replace_a_newer_generated_cue_list_for_the_same_key() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (mut old, rx) = subscriber("/alias/old.wav", true, settings(false));
        let mut new = job("/alias/new.wav", Guide::default());
        old.store = store.clone();
        old.job.key = "shared-key".into();
        new.key = old.job.key.clone();
        old.job.generation = 1;
        new.generation = 2;
        let mut fence = Commits::new(8);
        assert!(fence.publish(&new.key, 2, &mut || {}));
        let mut changed = settings(false);
        changed.options.cue.hot = 1;
        assert!(finish(&new, &analysed(), src(), &changed, &store).cues);
        let newest = store.edits(&new.key).unwrap().cues;
        assert!(!fence.publish(&old.job.key, 1, &mut || panic!("stale cache write")));
        deliver_cancelled(vec![old]);
        let cancelled = rx.recv().unwrap().1.unwrap();
        assert!(cancelled.cancelled && !cancelled.cues && cancelled.tag.is_none());
        assert_eq!(store.edits(&new.key).unwrap().cues, newest);
        assert!(store.cues_are_auto(&new.key).unwrap());
    }
    #[test]
    fn raw_run_admission_is_bounded_and_completion_or_cancellation_releases_it() {
        let mut pool = Pool::default();
        let runs: Vec<_> = (0..MAX_RUNS)
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect();
        for run in &runs {
            pool.admit(run, 4).unwrap();
        }
        let ninth = Arc::new(AtomicBool::new(false));
        assert!(
            pool.admit(&ninth, 4).is_err(),
            "repeated clicks get an error rather than another retained track snapshot"
        );
        runs[0].store(true, Ordering::Relaxed);
        pool.admit(&ninth, 4).unwrap();
        assert_eq!(pool.runs.len(), MAX_RUNS);
        runs[1].store(true, Ordering::Relaxed);
        let replacement = Arc::new(AtomicBool::new(false));
        pool.admit(&replacement, 4).unwrap();
        assert_eq!(pool.runs.len(), MAX_RUNS);
    }
    #[test]
    fn low_power_raw_admission_allows_two_overlapping_runs_but_rejects_more() {
        let mut pool = Pool::default();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        pool.admit(&first, 1).unwrap();
        pool.admit(&second, 1).unwrap();
        assert!(pool.admit(&Arc::new(AtomicBool::new(false)), 1).is_err());
    }

    #[test]
    fn repeated_submissions_reuse_worker_threads() {
        let _pool = TEST_RUNS.lock().unwrap_or_else(|e| e.into_inner());
        let threads = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let observed = threads.clone();
        let reader: AnalysisReader = Arc::new(move |_| {
            observed.lock().unwrap().insert(std::thread::current().id());
            None
        });
        let store = Arc::new(Store::in_memory().unwrap());
        for run in 0..2 {
            let jobs = (0..128)
                .map(|i| {
                    let mut job = job(
                        &format!("/missing/reused-workers-{run}-{i}.wav"),
                        Guide::default(),
                    );
                    job.reader = Some(reader.clone());
                    job
                })
                .collect();
            let (tx, rx) = mpsc::channel();
            let run = spawn(
                jobs,
                Settings {
                    workers: 2,
                    ..settings(false)
                },
                store.clone(),
                tx,
            )
            .unwrap();
            for _ in 0..128 {
                assert!(
                    rx.recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap()
                        .1
                        .is_err()
                );
            }
            drop(run);
        }
        let retained = pool().lock().unwrap().threads[0];
        let observed = threads.lock().unwrap().len();
        assert!(
            observed > 0 && observed <= retained,
            "track count does not create new worker threads"
        );
    }
}
