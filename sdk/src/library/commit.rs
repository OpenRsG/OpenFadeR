//! One change of a device's library files, all or nothing: a stick pulled out, a crash or a power cut at any moment leaves the
//! device with the whole old set or (once the host has finished it, [`recover`]) the whole new set, never a mix such as a new
//! OneLibrary next to an old Device Library.
//!
//! A plugin stages each new library file next to the one it replaces (`<name>.ofnew`, flushed to the device), reads the staged
//! files back to check them, then [`Commit::commit`] switches them. The journal `/.openfader/journal.json` on the device says
//! where the change is:
//! - `staging`: new files are being written; the old set is untouched and in use. Recovery deletes the staged files.
//! - `switching`: every staged file is whole and checked; they are being renamed over the old ones. Recovery finishes the
//!   renames (each one replaces a file whole), so the new set is complete.
//!
//! The journal sits outside the plugins' folders so players never see it. The host recovers a device when it shows up and before
//! any job writes to it.

use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::path::{Path, PathBuf};

/// The host's folder at the root of a device.
pub const DIR: &str = ".openfader";
/// The journal, relative to the root.
pub const JOURNAL: &str = ".openfader/journal.json";
/// Suffix of a staged file.
pub const STAGED: &str = "ofnew";

/// Where a change stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Staging,
    Switching,
}

/// The journal of a change in progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// The plugin writing ("rekordbox-device").
    pub owner: String,
    pub state: State,
    /// Library files being replaced, relative to the root with `/` separators.
    pub files: Vec<String>,
    /// Unix seconds the change started.
    pub started: i64,
}

/// What [`recover`] did with an interrupted change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovered {
    /// It was still staging: the staged files were deleted, the old library is the one in use.
    RolledBack { owner: String },
    /// It was switching: the remaining files were put in place, the new library is complete.
    Finished { owner: String },
}

thread_local! {
    /// Test hook: fail after this many steps of a commit, leaving the device as a pull-out would.
    static FAIL_AT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Test hook: the next commits on this thread stop dead after `step` steps (0: after the `switching` journal is written,
/// n: after the n-th rename), leaving their files and journal as a pulled stick would. `None` turns it off.
#[doc(hidden)]
pub fn fail_at(step: Option<usize>) {
    FAIL_AT.with(|f| f.set(step));
}

fn failing(step: usize) -> bool {
    FAIL_AT.with(|f| f.get() == Some(step))
}

/// A set of library files being replaced on the device mounted at `root`.
#[must_use = "a commit that is dropped deletes what it staged"]
pub struct Commit {
    root: PathBuf,
    owner: String,
    /// `(target, staged)`, absolute.
    files: Vec<(PathBuf, PathBuf)>,
    done: bool,
}

impl Commit {
    /// Starts a change. An interrupted one left on the device is recovered first.
    pub fn new(root: &Path, owner: &str) -> Result<Self, String> {
        recover(root)?;
        Ok(Self {
            root: root.to_owned(),
            owner: owner.to_owned(),
            files: Vec::new(),
            done: false,
        })
    }

    /// The path `target` is staged at.
    pub fn staged_path(target: &Path) -> PathBuf {
        let mut name = target.file_name().unwrap_or_default().to_owned();
        name.push(".");
        name.push(STAGED);
        target.with_file_name(name)
    }

    /// Stages `bytes` as the new content of `target` (flushed to the device).
    pub fn stage(&mut self, target: &Path, bytes: &[u8]) -> Result<(), String> {
        let staged = Self::staged_path(target);
        if let Some(dir) = staged.parent() {
            std::fs::create_dir_all(dir).map_err(at(dir))?;
        }
        write_synced(&staged, bytes)?;
        self.add(target, staged)
    }

    /// Registers a file the caller built itself at [`Self::staged_path`]`(target)` (a database written through its own
    /// library); it is flushed to the device here.
    pub fn stage_built(&mut self, target: &Path) -> Result<(), String> {
        let staged = Self::staged_path(target);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&staged)
            .and_then(|f| f.sync_all())
            .map_err(at(&staged))?;
        self.add(target, staged)
    }

    fn add(&mut self, target: &Path, staged: PathBuf) -> Result<(), String> {
        self.files.retain(|(t, _)| t != target);
        self.files.push((target.to_owned(), staged));
        self.journal(State::Staging)
    }

    /// `(target, staged)` of every staged file, to read them back before [`Self::commit`].
    pub fn staged(&self) -> impl Iterator<Item = (&Path, &Path)> {
        self.files.iter().map(|(t, s)| (t.as_path(), s.as_path()))
    }

    /// Puts every staged file in place. Once the `switching` journal is on the device the change counts as made: a failure after
    /// that (a rename refused, the stick pulled) leaves the journal and the remaining staged files for [`recover`] to finish.
    pub fn commit(mut self) -> Result<(), String> {
        self.journal(State::Switching)?;
        self.done = true;
        if failing(0) {
            return Err("injected failure".into());
        }
        for (n, (target, staged)) in self.files.iter().enumerate() {
            std::fs::rename(staged, target).map_err(at(target))?;
            if failing(n + 1) {
                return Err("injected failure".into());
            }
        }
        sync_parents(self.files.iter().map(|(t, _)| t.as_path()));
        clear(&self.root)
    }

    fn journal(&self, state: State) -> Result<(), String> {
        let j = Journal {
            owner: self.owner.clone(),
            state,
            files: self.files.iter().map(|(t, _)| rel(&self.root, t)).collect(),
            started: now(),
        };
        let path = self.root.join(JOURNAL);
        std::fs::create_dir_all(self.root.join(DIR)).map_err(at(&path))?;
        let tmp = path.with_extension("tmp");
        write_synced(
            &tmp,
            &serde_json::to_vec_pretty(&j).map_err(|e| e.to_string())?,
        )?;
        std::fs::rename(&tmp, &path).map_err(at(&path))?;
        sync_dir(&self.root.join(DIR));
        Ok(())
    }
}

impl Drop for Commit {
    /// Not committed (an error or a cancel on the way): the staged files go, the old set stays in use.
    fn drop(&mut self) {
        if !self.done {
            for (_, staged) in &self.files {
                let _ = std::fs::remove_file(staged);
            }
            let _ = clear(&self.root);
        }
    }
}

/// The change left in progress on the device at `root`, if any.
pub fn pending(root: &Path) -> Option<Journal> {
    serde_json::from_slice(&std::fs::read(root.join(JOURNAL)).ok()?).ok()
}

/// Finishes or undoes a change a pulled stick or a crash interrupted (see the module doc); `None`: there was none.
pub fn recover(root: &Path) -> Result<Option<Recovered>, String> {
    let Some(j) = pending(root) else {
        // A journal that cannot be parsed was cut while being written: its change never started switching.
        if root.join(JOURNAL).is_file() {
            clear(root)?;
        }
        return Ok(None);
    };
    let targets: Vec<PathBuf> = j.files.iter().map(|f| root.join(f)).collect();
    let r = match j.state {
        State::Staging => {
            for t in &targets {
                let _ = std::fs::remove_file(Commit::staged_path(t));
            }
            Recovered::RolledBack { owner: j.owner }
        }
        State::Switching => {
            for t in &targets {
                let staged = Commit::staged_path(t);
                if staged.is_file() {
                    std::fs::rename(&staged, t).map_err(at(t))?;
                }
            }
            sync_parents(targets.iter().map(PathBuf::as_path));
            Recovered::Finished { owner: j.owner }
        }
    };
    clear(root)?;
    Ok(Some(r))
}

fn clear(root: &Path) -> Result<(), String> {
    let path = root.join(JOURNAL);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            sync_dir(&root.join(DIR));
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Writes `bytes` to a new `path` and flushes it to the device.
pub fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    std::fs::File::create(path)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .map_err(at(path))
}

/// Flushes the folder entry of `dir` (the names in it) to the device; a no-op where folders cannot be opened (Windows).
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn sync_parents<'a>(files: impl Iterator<Item = &'a Path>) {
    let mut dirs: Vec<&Path> = files.filter_map(Path::parent).collect();
    dirs.sort();
    dirs.dedup();
    dirs.into_iter().for_each(sync_dir);
}

fn rel(root: &Path, p: &Path) -> String {
    let r = p.strip_prefix(root).unwrap_or(p);
    let parts: Vec<_> = r.iter().map(|c| c.to_string_lossy()).collect();
    parts.join("/")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn at(p: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", p.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "of-commit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("LIB")).unwrap();
        d
    }

    fn old(root: &Path) -> [PathBuf; 2] {
        let (a, b) = (root.join("LIB/a.db"), root.join("LIB/b.pdb"));
        std::fs::write(&a, "old a").unwrap();
        std::fs::write(&b, "old b").unwrap();
        [a, b]
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    fn staged_pair(root: &Path) -> Commit {
        let [a, b] = old(root);
        let mut c = Commit::new(root, "test").unwrap();
        c.stage(&a, b"new a").unwrap();
        c.stage(&b, b"new b").unwrap();
        c
    }

    #[test]
    fn a_commit_replaces_every_file_and_leaves_no_journal() {
        let r = root();
        staged_pair(&r).commit().unwrap();
        assert_eq!(
            (read(&r.join("LIB/a.db")), read(&r.join("LIB/b.pdb"))),
            ("new a".into(), "new b".into())
        );
        assert!(pending(&r).is_none());
        assert!(!Commit::staged_path(&r.join("LIB/a.db")).exists());
    }

    #[test]
    fn a_dropped_commit_leaves_the_old_files() {
        let r = root();
        drop(staged_pair(&r));
        assert_eq!(read(&r.join("LIB/a.db")), "old a");
        assert!(pending(&r).is_none());
        assert!(!Commit::staged_path(&r.join("LIB/b.pdb")).exists());
    }

    #[test]
    fn a_pull_out_at_any_step_recovers_to_one_whole_generation() {
        // Staging cut short: the journal says staging, recovery keeps the old set.
        let r = root();
        let c = staged_pair(&r);
        std::mem::forget(c);
        assert_eq!(pending(&r).unwrap().state, State::Staging);
        assert!(matches!(
            recover(&r).unwrap(),
            Some(Recovered::RolledBack { .. })
        ));
        assert_eq!(
            (read(&r.join("LIB/a.db")), read(&r.join("LIB/b.pdb"))),
            ("old a".into(), "old b".into())
        );
        // Switching cut after 0, 1 or 2 renames: recovery completes the new set.
        for step in 0..=2 {
            let r = root();
            fail_at(Some(step));
            assert!(staged_pair(&r).commit().is_err());
            fail_at(None);
            assert_eq!(pending(&r).unwrap().state, State::Switching);
            assert!(matches!(
                recover(&r).unwrap(),
                Some(Recovered::Finished { .. })
            ));
            assert_eq!(
                (read(&r.join("LIB/a.db")), read(&r.join("LIB/b.pdb"))),
                ("new a".into(), "new b".into()),
                "step {step}"
            );
            assert!(pending(&r).is_none());
        }
    }

    #[test]
    fn a_cut_journal_counts_as_no_change() {
        let r = root();
        let [a, _] = old(&r);
        std::fs::create_dir_all(r.join(DIR)).unwrap();
        std::fs::write(r.join(JOURNAL), "{\"owner\": \"te").unwrap();
        std::fs::write(Commit::staged_path(&a), "half").unwrap();
        assert_eq!(recover(&r).unwrap(), None);
        assert_eq!(read(&a), "old a");
        assert!(!r.join(JOURNAL).exists());
    }
}
