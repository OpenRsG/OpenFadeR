//! A process-wide, background-filled cache of [`Profile`]s by audio file, so ranking a library does not read thousands of
//! cache files on the UI thread. One worker thread drains the queue; [`Index::version`] changes whenever it finishes a file.

use super::{Profile, Theirs, profile};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

#[derive(Default)]
struct State {
    done: HashMap<PathBuf, Arc<Profile>>,
    queue: VecDeque<(PathBuf, Option<Theirs>)>,
    queued: HashSet<PathBuf>,
    working: bool,
    /// Files finished in the current batch (since the queue last went empty) and the batch's size: the progress line.
    finished: usize,
    total: usize,
    version: u64,
}

#[derive(Default)]
pub struct Index(Mutex<State>);

/// The one index of this process.
pub fn index() -> &'static Arc<Index> {
    static INDEX: LazyLock<Arc<Index>> = LazyLock::new(Arc::default);
    &INDEX
}

impl Index {
    /// Queues every `(audio, the library's analysis reader)` not known yet, and with `retry` also the known ones that had
    /// no features (the track may have been analysed since). Starts the worker when it is idle.
    pub fn request(
        self: &Arc<Self>,
        files: impl IntoIterator<Item = (PathBuf, Option<Theirs>)>,
        retry: bool,
    ) {
        let mut s = self.0.lock();
        if s.queue.is_empty() && !s.working {
            (s.finished, s.total) = (0, 0);
        }
        for (f, analysis) in files {
            let known = s
                .done
                .get(&f)
                .is_some_and(|p| !retry || p.features.is_some());
            if !known && s.queued.insert(f.clone()) {
                s.queue.push_back((f, analysis));
            }
        }
        s.total = s.finished + s.queue.len();
        if !s.queue.is_empty() && !s.working {
            s.working = true;
            let me = self.clone();
            std::thread::spawn(move || me.work());
        }
    }

    fn work(&self) {
        loop {
            let job = {
                let mut s = self.0.lock();
                let job = s.queue.pop_front();
                s.working = job.is_some();
                job
            };
            let Some((f, theirs)) = job else { return };
            let p = Arc::new(profile(&f, theirs));
            let mut s = self.0.lock();
            s.queued.remove(&f);
            s.done.insert(f, p);
            s.finished += 1;
            s.version += 1;
        }
    }

    pub fn get(&self, audio: &Path) -> Option<Arc<Profile>> {
        self.0.lock().done.get(audio).cloned()
    }

    /// `(finished, total)` of the current batch; `finished == total` when idle.
    pub fn progress(&self) -> (usize, usize) {
        let s = self.0.lock();
        (s.finished, s.total.max(s.finished))
    }

    pub fn pending(&self) -> usize {
        self.0.lock().queue.len()
    }

    /// Counts finished files: a change means rankings may differ.
    pub fn version(&self) -> u64 {
        self.0.lock().version
    }

    /// Puts a ready profile in (what the worker does), for tests and callers that already hold one.
    pub fn insert(&self, audio: PathBuf, p: Profile) {
        let mut s = self.0.lock();
        s.done.insert(audio, Arc::new(p));
        s.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_fills_the_index_and_marks_a_file_without_features_as_seen() {
        let ix = Arc::new(Index::default());
        let f = PathBuf::from("/nonexistent/a.mp3");
        ix.request([(f.clone(), None)], false);
        let t = std::time::Instant::now();
        while ix.get(&f).is_none() && t.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(ix.get(&f).unwrap().features, None);
        assert_eq!(ix.progress().0, 1);
        let v = ix.version();
        ix.request([(f.clone(), None)], false); // known: nothing queued
        assert_eq!((ix.pending(), ix.version()), (0, v));
    }
}
