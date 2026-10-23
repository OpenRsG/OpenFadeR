//! Crash reports: a panic hook writes `crash-<timestamp>.md` in the logs folder; the next start offers it ([`pending`]).
//!
//! A panic on another thread may leave the app running: [`super::finish`] marks such a report seen when the app ends normally, so
//! only a run that really stopped is offered. A report is "seen" when its name ends in `.seen.md`.

use super::report::Panic;
use parking_lot::Mutex;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Crash reports kept in the logs folder; older ones are deleted at start.
const KEEP: usize = 10;
/// Crash files this run wrote (see the module doc).
static WRITTEN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static BUSY: AtomicBool = AtomicBool::new(false);

/// A report from an earlier run that nobody has looked at.
pub struct Pending {
    pub path: PathBuf,
    pub text: String,
}

/// Writes the report as a new `crash-<timestamp>.md`.
pub fn write(dir: &Path, text: &str) -> io::Result<PathBuf> {
    let path = crate::paths::timestamped(dir, "crash", "md")?;
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?
        .write_all(text.as_bytes())?;
    Ok(path)
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.starts_with("crash-") && n.ends_with(".md")
        })
        .collect();
    // The timestamp orders them; a `-2` suffix (same second) comes after the plain name, which `.md` would sort after it.
    v.sort_by_key(|p| {
        p.to_string_lossy()
            .trim_end_matches(".md")
            .trim_end_matches(".seen")
            .to_owned()
    });
    v
}

/// The newest crash report not yet seen.
pub fn pending(dir: &Path) -> Option<Pending> {
    let path = files(dir)
        .into_iter()
        .rev()
        .find(|p| !p.to_string_lossy().ends_with(".seen.md"))?;
    let text = fs::read_to_string(&path).ok()?;
    Some(Pending { path, text })
}

/// `crash-X.md` becomes `crash-X.seen.md`.
pub fn mark_seen(path: &Path) {
    let s = path.to_string_lossy();
    if let Some(stem) = s.strip_suffix(".md").filter(|s| !s.ends_with(".seen")) {
        let _ = fs::rename(path, format!("{stem}.seen.md"));
    }
}

/// Deletes all but the newest [`KEEP`] crash reports.
pub fn prune(dir: &Path) {
    let all = files(dir);
    for old in &all[..all.len().saturating_sub(KEEP)] {
        let _ = fs::remove_file(old);
    }
}

/// Marks what this run wrote as seen: the app ended normally, so those panics did not stop it.
pub fn survived() {
    for p in WRITTEN.lock().drain(..) {
        mark_seen(&p);
    }
}

/// Installs the hook: the usual message first, then the report. A panic inside the hook, or a second one at the same time, adds nothing.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if BUSY.swap(true, Ordering::SeqCst) {
            return;
        }
        let p = Panic {
            message: info.payload_as_str().unwrap_or("(not a text)").to_owned(),
            location: info.location().map_or(String::new(), |l| {
                format!("{}:{}:{}", l.file(), l.line(), l.column())
            }),
            thread: std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_owned(),
            backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        };
        // What was printed just before is still in the pipe to the log thread.
        std::thread::sleep(Duration::from_millis(60));
        if let Some(dir) = super::dir() {
            match write(&dir, &super::snapshot(Some(&p))) {
                Ok(path) => {
                    eprintln!("crash report: {}", path.display());
                    WRITTEN.lock().push(path);
                }
                Err(e) => eprintln!("crash report: cannot write in {}: {e}", dir.display()),
            }
        }
        BUSY.store(false, Ordering::SeqCst);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_report_is_offered_once_and_only_the_newest_ten_are_kept() {
        let dir = std::env::temp_dir().join(format!("openfader-crash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(pending(&dir).is_none());
        let old = write(&dir, "first").unwrap();
        let new = write(&dir, "second").unwrap();
        assert_ne!(old, new, "two reports in one second keep both");
        let p = pending(&dir).unwrap();
        assert_eq!(
            (p.path.as_path(), p.text.as_str()),
            (new.as_path(), "second")
        );
        mark_seen(&new);
        assert_eq!(
            pending(&dir).unwrap().text,
            "first",
            "a seen report is not offered again"
        );
        mark_seen(&old);
        assert!(pending(&dir).is_none());
        for _ in 0..12 {
            write(&dir, "more").unwrap();
        }
        prune(&dir);
        assert_eq!(files(&dir).len(), KEEP);
    }
}
