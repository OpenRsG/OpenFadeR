//! Where an export writes: everything the run makes on the device is recorded, so a failed or cancelled run can put the device
//! back exactly as it was (see the crate doc, "All or nothing").

use crate::tag::{self, Edit, Tags};
use openfader_plugin::library::{
    audio_slot,
    commit::{self, Commit},
    host, place_audio, Track,
};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The error a cancelled job returns; the host knows it.
pub const CANCELLED: &str = "cancelled";

/// A failed write, worded for the user. A cancel stays the plain `cancelled`.
pub fn write_err(e: impl std::fmt::Display) -> String {
    let e = e.to_string();
    if e == CANCELLED {
        return e;
    }
    format!("Cannot write to the stick: {e}. Nothing on the stick was changed; check that it has room and is not write-protected, then try again.")
}

fn tag_err(path: &Path, e: tag::Error) -> String {
    format!(
        "The tags of {} cannot be written ({e:?}). The stick was left as it was; check that the file is not damaged or read-only, then export again.",
        path.display()
    )
}

/// How to put an edited file back: the bytes the edits replaced, as edits of the new file.
struct Restore {
    path: PathBuf,
    edits: Vec<Edit>,
    mtime: Option<SystemTime>,
}

impl Restore {
    /// The inverse of `edits` (sorted by start) on the file at `path`, before they are applied. An edit of equal length keeps
    /// only the window of bytes that differ (a tag holding a cover would otherwise cost megabytes per track).
    fn of(path: &Path, edits: &[Edit]) -> std::io::Result<Self> {
        let mut f = File::open(path)?;
        let (mut delta, mut inverse) = (0i64, Vec::new());
        for e in edits {
            let mut old = vec![0; (e.end - e.start) as usize];
            f.seek(SeekFrom::Start(e.start))?;
            f.read_exact(&mut old)?;
            let at = (e.start as i64 + delta) as u64;
            if old.len() == e.data.len() {
                let head = old.iter().zip(&e.data).take_while(|(a, b)| a == b).count();
                if head == old.len() {
                    continue;
                }
                let tail = old
                    .iter()
                    .rev()
                    .zip(e.data.iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();
                inverse.push(Edit {
                    start: at + head as u64,
                    end: at + (old.len() - tail) as u64,
                    data: old[head..old.len() - tail].to_vec(),
                });
            } else {
                delta += e.data.len() as i64 - old.len() as i64;
                inverse.push(Edit {
                    start: at,
                    end: at + e.data.len() as u64,
                    data: old,
                });
            }
        }
        Ok(Self {
            path: path.to_owned(),
            edits: inverse,
            mtime: fs::metadata(path).and_then(|m| m.modified()).ok(),
        })
    }

    fn undo(&mut self) {
        let _ = tag::apply(&self.path, &mut self.edits);
        if let Some(t) = self.mtime {
            let _ = fs::OpenOptions::new()
                .write(true)
                .open(&self.path)
                .and_then(|f| f.set_modified(t));
        }
    }
}

pub struct Writer {
    root: PathBuf,
    /// Files and folders this run made, oldest first.
    created: Vec<PathBuf>,
    /// The files among `created`: they are removed on a failure, so their edits need no undo.
    fresh: HashSet<PathBuf>,
    /// Edits of files the device had before the run, newest last.
    restores: Vec<Restore>,
    pub commit: Option<Commit>,
    /// The device had the host's `.openfader` folder before the run (it stays).
    had_dir: bool,
    /// The commit was asked to switch the files: from then on the new library is the one in use (a recovery finishes it), so
    /// what the run made must stay.
    pub switching: bool,
}

impl Writer {
    pub fn new(root: &Path, commit: Commit, had_dir: bool) -> Self {
        Self {
            root: root.to_owned(),
            created: Vec::new(),
            fresh: HashSet::new(),
            restores: Vec::new(),
            commit: Some(commit),
            had_dir,
            switching: false,
        }
    }

    /// Creates `dir`, recording the folders that were missing.
    pub fn make_dirs(&mut self, dir: &Path) -> Result<(), String> {
        let mut missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| !d.exists())
            .map(Path::to_path_buf)
            .collect();
        fs::create_dir_all(dir).map_err(|e| write_err(format!("{}: {e}", dir.display())))?;
        missing.reverse();
        self.created.extend(missing);
        Ok(())
    }

    fn make(&mut self, file: &Path) -> Result<(), String> {
        if let Some(dir) = file.parent() {
            self.make_dirs(dir)?;
        }
        self.created.push(file.to_owned());
        self.fresh.insert(file.to_owned());
        Ok(())
    }

    /// Copies the audio of `t` to its place on the device unless it is there (flushed, counted in the job's bytes).
    pub fn copy_audio(&mut self, t: &Track) -> Result<(), String> {
        let (rel, there) = audio_slot(&self.root, t).map_err(write_err)?;
        if !there {
            self.make(&self.root.join(&rel))?;
            place_audio(&self.root, t).map_err(write_err)?;
        }
        Ok(())
    }

    /// A new file `to`: a copy of `from` (flushed, counted in the job's bytes; a cancel stops it).
    pub fn copy_new(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        self.make(to)?;
        let tmp = to.with_extension("part");
        let done = copy_counted(from, &tmp)
            .and_then(|()| fs::rename(&tmp, to).map_err(|e| format!("{}: {e}", to.display())));
        if let Err(e) = done {
            let _ = fs::remove_file(&tmp);
            return Err(write_err(e));
        }
        if let Some(dir) = to.parent() {
            commit::sync_dir(dir);
        }
        Ok(())
    }

    /// Writes `tags` into the file at `path`; counts the bytes written. A file the device had before is put back by [`undo`].
    pub fn tag_file(&mut self, path: &Path, tags: &Tags, fixed: bool) -> Result<(), String> {
        let edits = tag::plan(path, tags, fixed).map_err(|e| tag_err(path, e))?;
        self.edit(path, edits)
    }

    /// Carries out `edits` on the file at `path` and counts their bytes.
    pub fn edit(&mut self, path: &Path, mut edits: Vec<Edit>) -> Result<(), String> {
        if edits.is_empty() {
            return Ok(());
        }
        edits.sort_by_key(|e| e.start);
        let bytes: u64 = edits.iter().map(|e| e.data.len() as u64).sum();
        let mut restore = if self.fresh.contains(path) {
            None
        } else {
            Some(
                Restore::of(path, &edits)
                    .map_err(|e| write_err(format!("{}: {e}", path.display())))?,
            )
        };
        // An edit of equal length is written in place and may stop half way: it must be undoable from the start. One that
        // changes the length goes through a temporary file and is whole or not there.
        let in_place = edits.iter().all(|e| e.end - e.start == e.data.len() as u64);
        let early = in_place && restore.is_some();
        if early {
            self.restores.extend(restore.take());
        }
        let changed = tag::apply(path, &mut edits).map_err(|e| tag_err(path, e))?;
        match (changed, early) {
            (false, true) => drop(self.restores.pop()),
            (true, false) => self.restores.extend(restore),
            _ => {}
        }
        if host::add_bytes(bytes) {
            return Err(CANCELLED.into());
        }
        Ok(())
    }

    /// Stages `bytes` as the new content of `target`, counting them in the job's bytes.
    pub fn stage(&mut self, target: &Path, bytes: &[u8]) -> Result<(), String> {
        let commit = self.commit.as_mut().ok_or_else(|| write_err("no commit"))?;
        commit.stage(target, bytes).map_err(|e| {
            let _ = fs::remove_file(Commit::staged_path(target));
            write_err(e)
        })?;
        if host::add_bytes(bytes.len() as u64) {
            return Err(CANCELLED.into());
        }
        Ok(())
    }

    /// Puts the device back as it was: the staged files and the journal go, the edited files get their bytes (and times) back,
    /// and what the run made is removed again, newest first.
    pub fn undo(&mut self) {
        self.commit = None;
        for r in self.restores.iter_mut().rev() {
            r.undo();
        }
        if !self.had_dir {
            let _ = fs::remove_dir(self.root.join(commit::DIR));
        }
        for p in self.created.iter().rev() {
            let _ = if p.is_dir() {
                fs::remove_dir(p)
            } else {
                fs::remove_file(p)
            };
        }
    }
}

/// Copies `src` to a new `to` in 1 MiB blocks, counting them in the job's bytes, and flushes `to` to the device.
fn copy_counted(src: &Path, to: &Path) -> Result<(), String> {
    let at = |p: &Path| {
        let p = p.to_owned();
        move |e: std::io::Error| format!("{}: {e}", p.display())
    };
    let mut from = File::open(src).map_err(at(src))?;
    let mut out = File::create(to).map_err(at(to))?;
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = from.read(&mut buf).map_err(at(src))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(at(to))?;
        if host::add_bytes(n as u64) {
            return Err(CANCELLED.into());
        }
    }
    out.sync_all().map_err(at(to))
}
