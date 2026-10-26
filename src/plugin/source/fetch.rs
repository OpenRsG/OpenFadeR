//! Downloading a plugin track: the plugin's `fetch` request writes the audio into a per-source folder, and a finished
//! download found there is reused without asking the plugin again.

use super::super::{Registry, registry};
use crate::library::Remote;
use parking_lot::{Condvar, Mutex};
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};
use std::time::Duration;

#[derive(Clone)]
struct Cancellation {
    flag: Arc<AtomicBool>,
    state: Arc<AtomicU8>,
}

impl Cancellation {
    fn new(cancelled: bool) -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(cancelled)),
            state: Arc::new(AtomicU8::new(u8::from(cancelled))),
        }
    }

    fn cancel(&self) {
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.flag.store(true, Ordering::Release);
        }
    }

    fn commit(&self) -> Result<(), String> {
        // Cancellation wins before this CAS; afterwards a completed download may finish its atomic
        // rename and stay cached even if its receiver is gone. Drop never waits for disk operations.
        self.state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| "request cancelled".into())
    }
}

impl std::ops::Deref for Cancellation {
    type Target = AtomicBool;
    fn deref(&self) -> &AtomicBool {
        &self.flag
    }
}

type Work = (Remote, Cancellation, mpsc::Sender<Result<PathBuf, String>>);
static WORK: LazyLock<(Mutex<VecDeque<Work>>, Condvar)> = LazyLock::new(|| {
    for _ in 0..4 {
        std::thread::spawn(|| {
            loop {
                let (remote, cancelled, back) = {
                    let (queue, ready) = &*WORK;
                    let mut queue = queue.lock();
                    while queue.is_empty() {
                        ready.wait(&mut queue);
                    }
                    queue.pop_front().expect("not empty")
                };
                if !cancelled.load(Ordering::Relaxed) {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        fetch_cancellable(&remote, cancelled.clone())
                    }))
                    .unwrap_or_else(|_| Err("download worker panicked".into()));
                    if !cancelled.load(Ordering::Relaxed) {
                        let _ = back.send(result);
                    }
                }
            }
        });
    }
    (Mutex::new(VecDeque::new()), Condvar::new())
});

/// A bounded remote download. Dropping it cancels its execution and request-owned output.
pub struct FetchJob {
    rx: Receiver<Result<PathBuf, String>>,
    cancelled: Cancellation,
}

impl FetchJob {
    pub fn try_recv(&self) -> Result<Result<PathBuf, String>, TryRecvError> {
        self.rx.try_recv()
    }
}

impl Drop for FetchJob {
    fn drop(&mut self) {
        self.cancelled.cancel();
        WORK.0
            .lock()
            .retain(|(_, c, _)| !Arc::ptr_eq(&c.flag, &self.cancelled.flag));
    }
}

pub fn fetch_async(remote: Remote) -> FetchJob {
    let (back, rx) = mpsc::channel();
    let cancelled = Cancellation::new(false);
    let (queue, ready) = &*WORK;
    let mut queue = queue.lock();
    queue.retain(|(_, cancelled, _)| !cancelled.load(Ordering::Relaxed));
    if queue.len() == 8 {
        let _ = back.send(Err("download queue is full".into()));
    } else {
        queue.push_back((remote, cancelled.clone(), back));
        ready.notify_one();
    }
    FetchJob { rx, cancelled }
}

fn fetch_cancellable(r: &Remote, cancelled: Cancellation) -> Result<PathBuf, String> {
    let base = crate::paths::data_dir().ok_or("set HOME or XDG_DATA_HOME to download")?;
    fetch_in_cancellable(registry(), &base.join("plugins"), r, cancelled)
}

struct Partial(PathBuf);

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Time a download may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(900);
/// Extensions of files a plugin may leave in the download folder while it is still writing them.
const UNFINISHED: [&str; 3] = ["part", "tmp", "ytdl"];

/// `id` as a file name: ASCII letters, digits, `-` and `_` stay, every other byte becomes `%XX`, so two ids never
/// share a name.
pub(super) fn file_name(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The finished download of `name` in `dir`: a non-empty file whose name without extension is `name`.
pub(super) fn downloaded(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem().is_some_and(|s| s == name)
                && !crate::paths::has_extension(p, &UNFINISHED)
                && p.metadata().is_ok_and(|m| m.is_file() && m.len() > 0)
        })
}

/// The audio file of a plugin track: the earlier download if there is one, else what the plugin's `fetch` request
/// writes into `<data dir>/plugins/<plugin>/<source>/`. Blocks until the download is done: call it from a worker.
pub fn fetch(r: &Remote) -> Result<PathBuf, String> {
    let base = crate::paths::data_dir().ok_or("set HOME or XDG_DATA_HOME to download")?;
    fetch_in(registry(), &base.join("plugins"), r)
}

/// [`fetch`] with the plugin registry and the download root given.
pub fn fetch_in(registry: &Registry, root: &Path, r: &Remote) -> Result<PathBuf, String> {
    fetch_in_cancellable(registry, root, r, Cancellation::new(false))
}

fn fetch_in_cancellable(
    registry: &Registry,
    root: &Path,
    r: &Remote,
    cancelled: Cancellation,
) -> Result<PathBuf, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("request cancelled".into());
    }
    let plugin = registry
        .get(&r.plugin)
        .ok_or_else(|| format!("plugin {} is not installed", r.plugin))?;
    let dir = root.join(&r.plugin).join(&r.source);
    let name = file_name(&r.id);
    if let Some(done) = downloaded(&dir, &name) {
        cancelled.commit()?;
        return Ok(done);
    }
    crate::paths::at(std::fs::create_dir_all(&dir), &dir)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let partial = Partial(dir.join(format!(
        ".fetch-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    crate::paths::at(std::fs::create_dir(&partial.0), &partial.0)?;
    let reply = plugin.call_cancellable(
        &json!({"op": "fetch", "source": r.source, "id": r.id, "dir": partial.0.to_string_lossy(), "name": name}),
        FETCH_TIMEOUT,
        cancelled.flag.clone(),
    )?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("request cancelled".into());
    }
    let file = reply["file"]
        .as_str()
        .map(|f| partial.0.join(f)) // an absolute path stays as it is
        .ok_or_else(|| format!("{}: the reply has no `file`", r.plugin))?;
    if !file.is_file() {
        return Err(format!("{}: {} was not written", r.plugin, file.display()));
    }
    if !file.starts_with(&partial.0) {
        cancelled.commit()?;
        return Ok(file);
    }
    let extension = file
        .extension()
        .ok_or_else(|| format!("{}: downloaded file has no extension", r.plugin))?;
    let dest = dir.join(format!("{}.{}", name, extension.to_string_lossy()));
    static COMMIT: Mutex<()> = Mutex::new(());
    let _commit = COMMIT.lock();
    if cancelled.load(Ordering::Relaxed) {
        return Err("request cancelled".into());
    }
    if let Some(done) = downloaded(&dir, &name) {
        cancelled.commit()?;
        return Ok(done);
    }
    cancelled.commit()?;
    if dest.metadata().is_ok_and(|m| m.len() == 0) {
        crate::paths::at(std::fs::remove_file(&dest), &dest)?;
    }
    crate::paths::at(std::fs::rename(&file, &dest), &dest)?;
    Ok(dest)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::plugin::tests::{registry_of, temp};
    use std::time::Instant;

    #[test]
    fn cancellation_and_publication_have_one_atomic_winner() {
        let before = Cancellation::new(false);
        before.cancel();
        assert!(before.commit().is_err());
        assert!(before.load(Ordering::Acquire));
        let after = Cancellation::new(false);
        after.commit().unwrap();
        after.cancel();
        assert!(
            !after.load(Ordering::Acquire),
            "committed downloads remain valid cache entries"
        );
    }

    #[test]
    fn replacing_a_blocked_fetch_cancels_its_child_and_partial_not_the_shared_plugin() {
        let root = temp("fetch-cancel-owned");
        let registry = Arc::new(registry_of(
            &root,
            "fetch-cancel-owned",
            json!({"persistent": true}),
            r#"
while IFS= read -r line; do
  case "$line" in
    *'"op":"fetch"'*)
      dir=$(printf '%s' "$line" | sed 's/.*"dir":"\([^"]*\)".*/\1/')
      printf 'partial' > "$dir/request.part"
      printf 'started' > fetch-started
      sleep 60
      printf 'abandoned' > "$dir/request.mp3"
      printf '{"file":"request.mp3"}\n'
      ;;
    *)
      id=${line##*\"id\":}; id=${id%%[!0-9]*}
      printf '{"id":%s,"pid":%s}\n' "$id" "$$"
      ;;
  esac
done
"#,
        ));
        let plugin = registry.get("fetch-cancel-owned").unwrap();
        let before = plugin
            .call(&json!({"op":"ping"}), Duration::from_secs(3))
            .unwrap()["pid"]
            .clone();
        let cache = root.join("downloads/fetch-cancel-owned/source");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("valid.mp3"), b"keep").unwrap();
        let cancelled = Cancellation::new(false);
        let (back, rx) = mpsc::channel();
        let old = FetchJob {
            rx,
            cancelled: cancelled.clone(),
        };
        let (done, answer) = mpsc::channel();
        let (reg, downloads) = (registry.clone(), root.join("downloads"));
        let worker = std::thread::spawn(move || {
            let remote = Remote {
                plugin: "fetch-cancel-owned".into(),
                source: "source".into(),
                id: "request".into(),
            };
            let result = fetch_in_cancellable(&reg, &downloads, &remote, cancelled);
            let _ = back.send(result.clone());
            let _ = done.send(result);
        });
        let started = root.join("fetch-cancel-owned/fetch-started");
        let until = Instant::now() + Duration::from_secs(3);
        while !started.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(started.exists(), "blocked fetch actually started");
        drop(old);
        assert!(
            answer
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap_err()
                .contains("cancelled")
        );
        worker.join().unwrap();
        assert_eq!(std::fs::read(cache.join("valid.mp3")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read_dir(&cache).unwrap().count(),
            1,
            "no abandoned staging or output"
        );
        let after = plugin
            .call(&json!({"op":"ping"}), Duration::from_secs(3))
            .unwrap()["pid"]
            .clone();
        assert_eq!(
            before, after,
            "cancellation cannot kill another request's persistent process"
        );
    }
    #[test]
    fn a_cancelled_fetch_never_publishes_a_preexisting_cached_success() {
        let root = temp("fetch-cancel-cache");
        let registry = registry_of(&root, "cached", json!({}), "exit 99");
        let dir = root.join("downloads/cached/source");
        std::fs::create_dir_all(&dir).unwrap();
        let valid = dir.join("track.mp3");
        std::fs::write(&valid, b"valid").unwrap();
        let remote = Remote {
            plugin: "cached".into(),
            source: "source".into(),
            id: "track".into(),
        };
        let result = fetch_in_cancellable(
            &registry,
            &root.join("downloads"),
            &remote,
            Cancellation::new(true),
        );
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(std::fs::read(&valid).unwrap(), b"valid");
    }

    #[test]
    fn isolated_persistent_fetch_preserves_its_track_id_and_settings() {
        let root = temp("fetch-inputs");
        let registry = registry_of(
            &root,
            "inputs",
            json!({"persistent": true}),
            r#"
while IFS= read -r line; do
  case "$line" in *'"id":"track"'*'"settings":'*|*'"settings":'*'"id":"track"'*) ;; *) exit 12 ;; esac
  dir=$(printf '%s' "$line" | sed 's/.*"dir":"\([^"]*\)".*/\1/')
  printf 'audio' > "$dir/track.mp3"
  printf '{"file":"track.mp3"}\n'
done
"#,
        );
        let remote = Remote {
            plugin: "inputs".into(),
            source: "source".into(),
            id: "track".into(),
        };
        let file = fetch_in(&registry, &root.join("downloads"), &remote).unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"audio");
    }
}
