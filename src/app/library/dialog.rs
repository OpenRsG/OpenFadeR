//! What the path-entry and confirmation dialogs ask ([`Ask`], [`Confirm`]), and the external file chooser (kdialog or
//! zenity) behind their Browse button.

use super::settings;
use openfader::library::{Track, playlist_file::Format};
use std::path::{Path, PathBuf};

/// What the path-entry dialog is asking for.
#[derive(Clone)]
pub enum Ask {
    ImportFiles,
    ImportFolder,
    ImportPlaylist,
    /// A collection file read by the `i`th library plugin of [`openfader::plugin::library::readers`].
    ImportCollection(usize),
    BackupTo {
        music: bool,
    },
    Restore,
    ExportList(Format),
    /// Missing tracks to relocate: the file (or folder) to take them from.
    Relocate(Vec<Track>),
}

/// What the confirmation dialog is asking.
#[derive(Clone)]
pub enum Confirm {
    BackupMusic,
    Restore(PathBuf),
    Remove(Vec<Track>),
    RemoveMissing(Vec<Track>),
}

/// How a file chooser picks.
#[derive(Clone, Copy, PartialEq)]
pub enum Pick {
    Files,
    File,
    Folder,
    Save,
}

impl Ask {
    pub(super) fn pick(&self) -> Pick {
        match self {
            Ask::ImportFiles => Pick::Files,
            Ask::ImportFolder | Ask::BackupTo { .. } => Pick::Folder,
            Ask::ExportList(_) => Pick::Save,
            _ => Pick::File,
        }
    }

    /// (dialog title, explanation, initial text).
    pub(super) fn prompt(&self) -> (&'static str, &'static str, String) {
        let music = || {
            settings::music_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        match self {
            Ask::ImportFiles => (
                "IMPORT TRACK",
                "Path of a music file, or several separated by | (mp3, m4a, aac, flac, wav, aiff).",
                music(),
            ),
            Ask::ImportFolder => (
                "IMPORT FOLDER",
                "Every music file below this folder joins the Collection.",
                music(),
            ),
            Ask::ImportPlaylist => (
                "IMPORT PLAYLIST",
                "An .m3u, .m3u8 or .pls file: its tracks join the Collection and a playlist of the same name lists them.",
                music(),
            ),
            Ask::ImportCollection(i) => (
                "IMPORT COLLECTION",
                "A collection file of the library plugin you chose: its tracks, playlists, ratings, comments, cues and beat grids are imported.",
                openfader::plugin::library::readers()
                    .into_iter()
                    .nth(*i)
                    .map(|r| {
                        if r.path.is_empty() {
                            downloaded(&r.formats).unwrap_or_default()
                        } else {
                            r.path
                        }
                    })
                    .unwrap_or_default(),
            ),
            Ask::BackupTo { .. } => (
                "BACKUP LIBRARY",
                "Folder for the backup zip (and for the music files, in a backup folder beside it).",
                openfader::paths::output_dir("OPENFADER_EXPORT_DIR")
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            Ask::Restore => (
                "RESTORE LIBRARY",
                "A backup zip made by Backup Library. The Collection, playlists, cues and every other stored edit are replaced.",
                String::new(),
            ),
            Ask::ExportList(f) => (
                match f {
                    Format::M3u8 => "EXPORT PLAYLIST (M3U8)",
                    Format::Pls => "EXPORT PLAYLIST (PLS)",
                    Format::Text => "EXPORT PLAYLIST (TEXT)",
                },
                "File to write (an existing file is never overwritten).",
                String::new(),
            ),
            Ask::Relocate(_) => (
                "RELOCATE",
                "The new file of the first track — the others are looked for in its folder — or a folder to look in for all of them.",
                String::new(),
            ),
        }
    }
}

/// The newest file of one of the plugin's `formats` in the Downloads or music folder: where a downloaded collection lands.
fn downloaded(formats: &[String]) -> Option<String> {
    let downloads = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads"));
    [downloads, settings::music_dir()]
        .into_iter()
        .flatten()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| formats.iter().any(|f| x.eq_ignore_ascii_case(f)))
        })
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
        .map(|p| p.display().to_string())
}

/// A file chooser program on this machine (kdialog or zenity), if any.
pub fn chooser() -> Option<&'static str> {
    let on_path = |p: &str| {
        std::env::var_os("PATH")
            .is_some_and(|v| std::env::split_paths(&v).any(|d| d.join(p).is_file()))
    };
    ["kdialog", "zenity"].into_iter().find(|p| on_path(p))
}

/// Asks the chooser program for a path (blocks until the user answers; run it on a thread). Several files come back
/// separated by `|`.
pub fn browse(pick: Pick, start: &str) -> Option<String> {
    let mut cmd = std::process::Command::new(chooser()?);
    let start = if start.is_empty() { "." } else { start };
    if cmd.get_program() == "kdialog" {
        match pick {
            Pick::Files => cmd.args([
                "--multiple",
                "--separate-output",
                "--getopenfilename",
                start,
            ]),
            Pick::File => cmd.args(["--getopenfilename", start]),
            Pick::Folder => cmd.args(["--getexistingdirectory", start]),
            Pick::Save => cmd.args(["--getsavefilename", start]),
        };
    } else {
        cmd.arg("--file-selection")
            .arg(format!("--filename={start}"));
        match pick {
            Pick::Files => cmd.args(["--multiple", "--separator=|"]),
            Pick::File => &mut cmd,
            Pick::Folder => cmd.arg("--directory"),
            Pick::Save => cmd.args(["--save", "--confirm-overwrite"]),
        };
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout)
        .trim()
        .replace('\n', "|");
    (!text.is_empty()).then_some(text)
}

/// Opens `dir` (or the folder of a file) in the desktop's file manager.
pub fn show_in_folder(path: &Path) {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let _ = open_external(&dir.to_string_lossy());
}

/// Opens a web address, file or folder in the desktop's program for it.
pub fn open_external(target: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot open {target}: {e}"))
}
