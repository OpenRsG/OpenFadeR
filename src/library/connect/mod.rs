//! Connections (base): the libraries of other apps and plain folders that are synced into our Collection. A connection is
//! a [`Connection`] row of the store (a library plugin and the file or folder it reads, `plugin` "" for a folder), and a
//! sync copies what its source lists into the Collection and keeps the mapping, so a later sync only touches what changed.
//! Our Collection is the truth: the source is read, and written back only in two-way mode.
//!
//! Children: [`sync`] (source → Collection, incremental by a hash of what the source says per track), [`detect`] (the
//! libraries the installed plugins say other apps keep in their default places), [`writeback`] (Collection → source file,
//! after a backup and a dry run).

mod detect;
mod sync;
mod writeback;

pub use crate::store::connect::{Conn as Connection, Mode, Wins};
pub use detect::{detect, expand, Found};
pub use sync::{run, sync, Report};
pub use writeback::{apply, plan, Plan};

use super::{folder::Folder, Library};
use crate::store::{Store, R};
use std::path::Path;

/// `source_node` id of the folder the connection's playlists sit under (no source playlist has an empty id).
const ROOT: &str = "";

/// The track's id in `source_map`: its file, else the plugin's analysis ref. `None`: nothing to map it by.
pub(crate) fn foreign_id(t: &super::Track) -> Option<String> {
    t.file
        .as_deref()
        .map(|f| f.to_string_lossy().into_owned())
        .or_else(|| t.analysis_ref.clone())
}

/// Opens the source of `c` (readable from a worker thread).
pub fn open(c: &Connection) -> Result<Box<dyn Library + Send>, String> {
    if c.plugin.is_empty() {
        Ok(Box::new(Folder::open(&c.path)?))
    } else {
        Ok(Box::new(crate::plugin::library::open_with(
            &c.plugin, &c.path,
        )?))
    }
}

/// The state of `c`'s source, cheap to read: a library file by modification time and size; a folder by the newest
/// modification time, the number and the total size of its files. A plain folder connection looks at the audio files at
/// any depth (what [`Folder`] scans), a plugin's folder (device root, `Engine Library`, `_Serato_`) at every file up to
/// [`STAMP_DEPTH`] folders down, so an in-place edit of the library file inside is noticed. Another text than the last
/// sync's means the source changed; empty means gone.
pub fn stamp(c: &Connection) -> String {
    let tick = |m: &std::fs::Metadata| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    let Ok(meta) = std::fs::metadata(&c.path) else {
        return String::new();
    };
    if !meta.is_dir() {
        return format!("{}:{}", tick(&meta), meta.len());
    }
    let (mut newest, mut n, mut bytes) = (tick(&meta), 0u64, 0u64);
    let plain = c.plugin.is_empty();
    let depth = if plain { usize::MAX } else { STAMP_DEPTH };
    super::walk_files(&c.path, depth, |p| {
        if plain && !super::folder::is_audio(&p) {
            return;
        }
        if let Ok(m) = std::fs::metadata(&p) {
            newest = newest.max(tick(&m));
            bytes += m.len();
            n += 1;
        }
    });
    format!("{newest}:{n}:{bytes}")
}

/// How many folders below a plugin's folder [`stamp`] looks.
const STAMP_DEPTH: usize = 3;

/// Connects `path` (a library file a plugin reads, or a folder): the plugin that claims it, else a folder is a plain
/// folder connection. A file no plugin claims (a library without extension such as `_Serato_/database V2`, which the
/// plugins' format lists cannot name) is retried as its parent folder when a plugin claims that. Already connected: that
/// connection. The journal position now is where its write-back starts.
pub fn adopt(store: &Store, path: &Path) -> R<Connection> {
    if !path.exists() {
        return Err(format!("{}: no such file or directory", path.display()));
    }
    if path.is_file() && crate::plugin::library::claim(path).is_none() {
        if let Some(dir) = path
            .parent()
            .filter(|d| crate::plugin::library::claim(d).is_some())
        {
            return adopt(store, dir);
        }
    }
    let (plugin, label) = match crate::plugin::library::claim(path) {
        Some((plugin, kind)) => (plugin, kind),
        None if path.is_dir() => (
            String::new(),
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
        ),
        None => {
            return Err(format!(
                "no library plugin reads {} (enable one in Preferences > Extensions)",
                path.display()
            ))
        }
    };
    connect(store, &plugin, path, &label)
}

/// [`adopt`] with the plugin chosen (a detected library, or a Media Browser row).
pub fn connect(store: &Store, plugin: &str, path: &Path, label: &str) -> R<Connection> {
    store.connection_add(plugin, path, label, store.journal_seq()?)
}

/// The own music folder, always connected.
pub fn own_folder(store: &Store, path: &Path) -> R<Connection> {
    connect(
        store,
        "",
        path,
        &path
            .file_name()
            .map_or_else(|| "Music".into(), |n| n.to_string_lossy().into_owned()),
    )
}

#[cfg(test)]
mod tests;
