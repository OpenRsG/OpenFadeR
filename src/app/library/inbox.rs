//! INBOX ([`openfader::inbox`]): the folders watched while the app runs, and the settings rows (Preferences > Advanced > Database
//! > Inbox). A timer polls the watched folders every few seconds; the pass itself (a `read_dir` per folder, reading what is new)
//! runs on a thread, and what it found comes back to the UI thread: status line, browser reload, analysis of the new tracks as
//! after an import (when Auto Analysis is on), and the plugin event `inbox_added`.
//!
//! | key | item | effect |
//! | --- | --- | --- |
//! | `inbox_folders` | WATCHED FOLDERS | audio files and zips arriving there join INBOX; empty = nothing watched |
//! | `inbox_move_files` | Move the file with the track | a move to another stage carries the file to that stage's folder |
//! | `inbox_root` | LIBRARY ROOT | the folder holding one folder per stage (empty: the Music folder) |
//! | `inbox_pattern` | PATH PATTERN | what is under the stage's folder, see [`openfader::inbox::target`] |

use super::{settings, Ctx};
use crate::app::prefs::{self, Prefs, Row};
use openfader::inbox::{self, Config, DEFAULT_PATTERN, SETTLE};
use slint::{Timer, TimerMode};
use std::{cell::Cell, path::PathBuf, rc::Rc, time::Duration};

/// Folder list separator inside the one-line settings value.
const SEP: char = ';';
/// Time between two passes over the watched folders.
const EVERY: Duration = Duration::from_secs(5);

thread_local! {
    static POLL: Timer = Timer::default();
    /// A pass is running on its thread.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

fn of(p: &Prefs) -> Config {
    let text = |k: &str| p.extra(k).unwrap_or("").trim().to_owned();
    let root = text("inbox_root");
    let pattern = text("inbox_pattern");
    Config {
        folders: text("inbox_folders")
            .split(SEP)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect(),
        move_files: p.extra_bool("inbox_move_files", false),
        root: if root.is_empty() {
            settings::music_dir().unwrap_or_default()
        } else {
            PathBuf::from(root)
        },
        pattern: if pattern.is_empty() {
            DEFAULT_PATTERN.to_owned()
        } else {
            pattern
        },
    }
}

/// What the user set, read live.
pub fn config() -> Config {
    prefs::with(of)
}

pub fn rows(p: &Prefs) -> Vec<Row> {
    let (c, text) = (of(p), |k: &str| p.extra(k).unwrap_or("").to_owned());
    vec![
        Row::header(tr!("INBOX")),
        Row::text(
            "inbox_folders",
            tr!("WATCHED FOLDERS"),
            &text("inbox_folders"),
        )
        .hint(tr!("new music and zips join INBOX · separate with ;")),
        Row::check(
            "inbox_move_files",
            tr!("Move the file with the track"),
            c.move_files,
        )
        .hint(tr!("to its stage's folder under the library root")),
        Row::text("inbox_root", tr!("LIBRARY ROOT"), &text("inbox_root"))
            .hint(tr!("empty: the Music folder"))
            .enabled(c.move_files),
        Row::text("inbox_pattern", tr!("PATH PATTERN"), &text("inbox_pattern"))
            .hint(DEFAULT_PATTERN)
            .enabled(c.move_files),
    ]
}

pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    let mine = key == "inbox_move_files";
    if mine {
        p.set_extra(key, i == 1);
    }
    mine
}

/// A pass found `found`: says what arrived, shows it and starts its analysis.
fn done(c: &Rc<Ctx>, found: inbox::Scan) {
    BUSY.set(false);
    let mut said = Vec::new();
    if !found.added.is_empty() {
        said.push(tr_n!(
            "{n} new track in INBOX",
            "{n} new tracks in INBOX",
            found.added.len()
        ));
        let tracks: Vec<_> = found
            .added
            .iter()
            .filter_map(|&id| c.store.collection_track(id).ok().flatten())
            .collect();
        openfader::plugin::event::fire(
            "inbox_added",
            serde_json::json!({"files": tracks.iter().filter_map(|t| t.file.as_ref().map(|f| f.to_string_lossy())).collect::<Vec<_>>()}),
        );
        c.reload(None);
        c.analyze(tracks, false);
    }
    if found.linked > 0 {
        said.push(tr!(
            "{} already in the Collection (linked, not added)",
            found.linked
        ));
    }
    if let Some(first) = found.failed.first() {
        said.push(tr!("inbox: {}", first));
    }
    if !said.is_empty() {
        c.say(&said.join(" · "));
    }
}

fn tick(c: &Rc<Ctx>) {
    let folders = config().folders;
    if folders.is_empty() || BUSY.replace(true) {
        return;
    }
    let store = c.store.clone();
    c.spawn(move || inbox::scan(&store, &folders, SETTLE), done);
}

/// Registers the text rows and starts watching (once, UI thread).
pub(super) fn wire(c: &Rc<Ctx>) {
    for key in ["inbox_folders", "inbox_root", "inbox_pattern"] {
        prefs::on_text(key, move |v| prefs::update(|p| p.set_extra(key, v.trim())));
    }
    let k = c.clone();
    POLL.with(|t| t.start(TimerMode::Repeated, EVERY, move || tick(&k)));
}
