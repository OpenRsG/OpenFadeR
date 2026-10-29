//! One producer per compatible preparation, with independently consumed outcomes and deck focus.

use openfader::audio::{self, stemfile};
use openfader::stems::{Job, Phase, Quality};
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    path: PathBuf,
    version: Version,
    file_digest: Option<[u8; 32]>,
    pcm_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Version {
    bytes: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    file_id: (u64, u64, i64, i64),
}

impl Version {
    pub(super) fn at(file: &Path) -> Option<Self> {
        Self::of(&std::fs::metadata(file).ok()?)
    }

    fn of(meta: &std::fs::Metadata) -> Option<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        meta.is_file().then_some(Self {
            bytes: meta.len(),
            modified: meta.modified().ok()?,
            created: meta.created().ok(),
            #[cfg(unix)]
            file_id: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
        })
    }
}

impl Source {
    /// Runs only on the bounded lookup workers; decoded identity covers independently loaded mixes.
    pub(super) fn identify(
        file: &Path,
        pcm: &audio::Track16,
        stem_file: bool,
        before: Option<Version>,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Self> {
        if cancelled() {
            return None;
        }
        let path = std::fs::canonicalize(file).ok()?;
        let version = Version::at(&path)?;
        if Some(&version) != before.as_ref() {
            return None;
        }
        let pcm_digest = pcm.content_digest(cancelled)?;
        // A stem file's other four tracks can change without changing its decoded mix.
        let file_digest = if stem_file {
            let mut file = std::fs::File::open(&path).ok()?;
            let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
            let mut bytes = [0u8; 1 << 16];
            loop {
                if cancelled() {
                    return None;
                }
                let n = file.read(&mut bytes).ok()?;
                if n == 0 {
                    break;
                }
                digest.update(&bytes[..n]);
            }
            Some(digest.finish().as_ref().try_into().expect("SHA-256"))
        } else {
            None
        };
        let source = Self {
            path,
            version,
            file_digest,
            pcm_digest,
        };
        (!cancelled() && source.is_current()).then_some(source)
    }

    pub(super) fn is_current(&self) -> bool {
        std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| Version::of(&m))
            .as_ref()
            == Some(&self.version)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    source: Source,
    rate: u32,
    frames: usize,
    quality: Quality,
    multithread: bool,
    from_file: bool,
    parts: Option<stemfile::Info>,
}

pub(super) trait Producer {
    fn stems(&self) -> Option<Arc<audio::Stems>>;
    fn progress(&self) -> (Phase, f32);
    fn focus(&self, play: usize, cues: &[usize]);
    fn poll(&self) -> Option<Result<(), String>>;
}

impl Producer for Job {
    fn stems(&self) -> Option<Arc<audio::Stems>> {
        Job::stems(self).cloned()
    }
    fn progress(&self) -> (Phase, f32) {
        self.progress.get()
    }
    fn focus(&self, play: usize, cues: &[usize]) {
        Job::focus(self, play, cues);
    }
    fn poll(&self) -> Option<Result<(), String>> {
        Job::poll(self)
    }
}

struct Focus {
    id: usize,
    at: Option<(usize, Vec<usize>)>,
}

struct Shared<P> {
    producer: P,
    outcome: Option<Result<(), String>>,
    subscribers: Vec<Focus>,
    next: usize,
}

impl<P: Producer> Shared<P> {
    fn focus(&self) {
        let mut positions = self.subscribers.iter().filter_map(|s| s.at.as_ref());
        let Some((play, hot)) = positions.next() else {
            self.producer.focus(0, &[]);
            return;
        };
        let mut cues = hot.clone();
        for (position, hot) in positions {
            cues.push(*position);
            cues.extend_from_slice(hot);
        }
        self.producer.focus(*play, &cues);
    }
}

type Registry<P> = Vec<(Key, Weak<RefCell<Shared<P>>>)>;

thread_local! {
    // Weak entries never keep an output or its memory reservation alive after the decks release it.
    static JOBS: RefCell<Registry<Job>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct Subscription<P: Producer = Job> {
    group: Rc<RefCell<Shared<P>>>,
    id: usize,
    delivered: Cell<bool>,
}

fn subscribe<P: Producer>(
    registry: &mut Registry<P>,
    key: Option<Key>,
    start: impl FnOnce() -> P,
) -> Subscription<P> {
    registry.retain(|(_, shared)| shared.strong_count() > 0);
    let existing = key.as_ref().and_then(|key| {
        registry
            .iter()
            .find_map(|(other, shared)| (key == other).then(|| shared.upgrade()).flatten())
    });
    let group = existing.unwrap_or_else(|| {
        let group = Rc::new(RefCell::new(Shared {
            producer: start(),
            outcome: None,
            subscribers: Vec::new(),
            next: 0,
        }));
        if let Some(key) = key {
            registry.push((key, Rc::downgrade(&group)));
        }
        group
    });
    let id = {
        let mut shared = group.borrow_mut();
        let id = shared.next;
        shared.next += 1;
        shared.subscribers.push(Focus { id, at: None });
        id
    };
    Subscription {
        group,
        id,
        delivered: Cell::new(false),
    }
}

impl Subscription {
    pub(super) fn start(
        file: PathBuf,
        pcm: Arc<audio::Track16>,
        quality: Quality,
        multithread: bool,
        from_file: bool,
        parts: Option<stemfile::Info>,
        source: Option<Source>,
    ) -> Self {
        let key = source.map(|source| Key {
            source,
            rate: pcm.rate,
            frames: pcm.frames(),
            quality,
            multithread,
            from_file,
            parts: parts.clone(),
        });
        JOBS.with_borrow_mut(|registry| {
            subscribe(registry, key, || match parts {
                Some(info) => Job::from_stem_file(file, pcm, info),
                None => Job::start(file, pcm, quality, multithread),
            })
        })
    }
}

impl<P: Producer> Subscription<P> {
    pub(super) fn stems(&self) -> Option<Arc<audio::Stems>> {
        self.group.borrow().producer.stems()
    }

    pub(super) fn progress(&self) -> (Phase, f32) {
        self.group.borrow().producer.progress()
    }

    pub(super) fn focus(&self, play: usize, cues: &[usize]) {
        let mut shared = self.group.borrow_mut();
        if let Some(subscriber) = shared.subscribers.iter_mut().find(|s| s.id == self.id) {
            subscriber.at = Some((play, cues.to_vec()));
            shared.focus();
        }
    }

    pub(super) fn poll(&self) -> Option<Result<(), String>> {
        if self.delivered.get() {
            return None;
        }
        let mut shared = self.group.borrow_mut();
        if shared.outcome.is_none() {
            shared.outcome = shared.producer.poll();
        }
        let outcome = shared.outcome.clone();
        if outcome.is_some() {
            self.delivered.set(true);
        }
        outcome
    }
}

impl<P: Producer> Drop for Subscription<P> {
    fn drop(&mut self) {
        let mut shared = self.group.borrow_mut();
        shared.subscribers.retain(|s| s.id != self.id);
        if !shared.subscribers.is_empty() {
            shared.focus();
        }
    }
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
