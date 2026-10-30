//! Library sources (base). Every source maps its rows to the shared [`Track`] / [`Playlist`] types (those of the library
//! plugin SDK: one definition for the host and the plugins) and implements [`Library`]; [`open`] picks the source from
//! the path: the library plugin that claims it ([`crate::plugin::library`]), else a plain folder is [`folder`]. The base
//! also owns the "Histories" folder convention ([`is_history`]) and `OPENFADER_PATH_MAP` remapping ([`pathmap`]).
//!
//! Children: [`own`] (our own Collection, the library the app opens), [`folder`] (plain directory of audio), [`snapshot`]
//! (the owned export input of a library), [`playlist_file`] (M3U / M3U8 / PLS files), [`relocate`] (finding moved files by
//! name), [`import`] (files, playlists and other libraries into the Collection), [`translate`] (fitting a collection to what
//! a format can hold, with a loss report), [`open_format`] (our documented JSON / CSV export, `docs/library-format.md`).
//! Every other format is a library plugin.

pub mod connect;
pub mod folder;
pub mod import;
pub mod import_report;
pub mod open_format;
pub mod own;
pub mod playlist_file;
pub mod print;
pub mod relocate;
pub mod snapshot;
pub mod translate;

mod pathmap;

use openfader_plugin::library::{playlist_tree, PlaylistRow};
pub use openfader_plugin::library::{Playlist, Remote, Track};
use pathmap::path_map_from_env;
pub use pathmap::{location, parse_path_map, remap_path, PathMap};

use std::path::{Path, PathBuf};

/// Calls `visit` on every file below `root`, descending at most `max_depth` folders (symlinked
/// directories are not followed; order unspecified).
fn walk_files(root: &Path, max_depth: usize, mut visit: impl FnMut(PathBuf)) {
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() && depth < max_depth => stack.push((e.path(), depth + 1)),
                Ok(t) if t.is_file() => visit(e.path()),
                _ => {}
            }
        }
    }
}

/// Today's local date as `YYYY-MM-DD`, the `date_added` of imported tracks.
pub(crate) fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub use crate::plugin::library::load_ref;

/// Whether a [`Playlist::id`] names a history playlist or the "Histories" folder.
pub fn is_history(id: &str) -> bool {
    id.starts_with(HISTORY)
}

/// Id of the synthetic "Histories" folder; each history playlist's id is this followed by the
/// source's own id.
const HISTORY: &str = "history:";

/// [`Library::analysis`] without the library: what a worker thread reads a track's analysis with (the library itself is
/// not `Send`).
pub type AnalysisReader =
    std::sync::Arc<dyn Fn(&Track) -> Option<crate::analysis::Analysis> + Send + Sync>;

pub trait Library {
    /// Source name for the window title, e.g. "Folder".
    fn kind(&self) -> &str;
    /// Folders and plain playlists in depth-first tree order; histories sit under a last
    /// top-level "Histories" folder (see [`is_history`]).
    fn playlists(&self) -> Result<Vec<Playlist>, String>;
    /// `None`: all tracks. `Some(id)`: that playlist's tracks in playlist order.
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String>;
    /// All tracks, shared: what callers that only read use, so a library that keeps its list (the Collection) hands it out
    /// without a copy.
    fn tracks_all(&self) -> Result<std::sync::Arc<Vec<Track>>, String> {
        self.tracks(None).map(std::sync::Arc::new)
    }
    /// The tracks already carry the store's text and rating edits (the Collection reads them with its rows): the store's
    /// overlay hands them on as they are.
    fn edited(&self) -> bool {
        false
    }
    /// Runs the search `query` in the search entry `playlist` (a playlist id this library listed) in the background;
    /// the results appear in `tracks(Some(playlist))` once the source has answered. `false`: `playlist` is no search entry.
    fn search(&self, _playlist: &str, _query: &str) -> bool {
        false
    }
    /// Beat grid, cues and waveforms the source itself holds for `track`; `None` when it has none. Callers fall back to the
    /// analysis cache and to detecting from the audio.
    fn analysis(&self, _track: &Track) -> Option<crate::analysis::Analysis> {
        None
    }
    /// [`Library::analysis`] for a worker thread (bulk reads that must not run on the UI thread); `None` when the source
    /// has no analysis to read.
    fn analysis_reader(&self) -> Option<AnalysisReader> {
        None
    }
}

/// Opens whatever `path` is: the library plugin that claims it ([`crate::plugin::library::open`], the best `probe` score
/// wins), else a folder as [`folder::Folder`]; a file no plugin claims is an error. `Send`, so a slow source (a USB stick)
/// can be read on a worker thread.
pub fn open(path: &Path) -> Result<Box<dyn Library + Send>, String> {
    if !path.exists() {
        return Err(format!(
            "{}: no such file or directory (USB not mounted?)",
            path.display()
        ));
    }
    if let Some(lib) = crate::plugin::library::open(path)? {
        return Ok(Box::new(lib));
    }
    if path.is_dir() {
        return Ok(Box::new(folder::Folder::open(path)?));
    }
    Err(format!(
        "no enabled library reader handles {} (enable its format in Preferences > Library formats, or its add-on in Preferences > Extensions)",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_falls_back_to_a_folder_and_refuses_an_unclaimed_file() {
        let dir = std::env::temp_dir().join(format!("ofr-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.xyz");
        std::fs::write(&file, b"?").unwrap();
        assert_eq!(open(&dir).map(|l| l.kind().to_owned()), Ok("Folder".into()));
        assert!(open(&file).is_err());
        assert!(open(&dir.join("gone")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
