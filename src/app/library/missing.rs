//! Missing File Manager (File > Display All Missing Files): the tracks whose audio file is gone, with
//! RELOCATE (new file for the first track, the others from its folder, or a folder for all), AUTO RELOCATE (search the
//! Auto Relocate Search Folders by file name, [`relocate`]) and DELETE (remove from the Collection). Ticking nothing
//! acts on every listed track. Also the Track menu's Relocate / Auto Relocate for the chosen track.
//!
//! A scan stats every track's file on a thread (a USB stick is slow); [`is_missing`] answers from the last scan.

use super::{Ask, Confirm, Ctx, settings};
use crate::app::{LibraryState, MissingRow, plural};
use openfader::library::{Track, relocate};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Default)]
pub struct State {
    rows: Vec<Track>,
    ticked: Vec<bool>,
}

thread_local! {
    /// Files the last scan did not find.
    static MARKS: RefCell<HashSet<PathBuf>> = RefCell::default();
}

/// Whether the last scan found no file at `path` (always false before the first scan finishes).
pub fn is_missing(path: &Path) -> bool {
    MARKS.with(|m| m.borrow().contains(path))
}

fn show(c: &Ctx) {
    let st = c.state.borrow();
    let rows: Vec<MissingRow> = st
        .missing
        .rows
        .iter()
        .zip(&st.missing.ticked)
        .map(|(t, &on)| MissingRow {
            title: format!("{} — {}", t.title, t.artist).into(),
            path: t
                .file
                .as_ref()
                .map_or_else(String::new, |f| f.display().to_string())
                .into(),
            selected: on,
        })
        .collect();
    c.app()
        .global::<LibraryState>()
        .set_missing_rows(ModelRc::new(VecModel::from(rows)));
}

fn status(c: &Ctx, text: &str) {
    c.app()
        .global::<LibraryState>()
        .set_missing_status(text.into());
}

/// Looks for missing files and refreshes the manager's rows when the thread is done.
fn scan(c: &Rc<Ctx>) {
    let tracks = match c.lib.tracks(None) {
        Ok(t) => t,
        Err(e) => return status(c, &format!("scan failed: {e}")),
    };
    let app = c.app();
    let s = app.global::<LibraryState>();
    s.set_busy(true);
    status(c, "Searching…");
    c.spawn(
        move || {
            tracks
                .into_iter()
                .filter(|t| t.file.as_deref().is_some_and(|f| !f.exists()))
                .collect::<Vec<Track>>()
        },
        |c, gone| {
            MARKS.with(|m| *m.borrow_mut() = gone.iter().filter_map(|t| t.file.clone()).collect());
            let msg = if gone.is_empty() {
                "File is not missing: every track has its file".to_owned()
            } else {
                format!(
                    "File is Missing: {} track{}",
                    gone.len(),
                    plural(gone.len())
                )
            };
            c.state.borrow_mut().missing = State {
                ticked: vec![false; gone.len()],
                rows: gone,
            };
            c.app().global::<LibraryState>().set_busy(false);
            status(c, &msg);
            show(c);
        },
    );
}

pub fn open(c: &Rc<Ctx>) {
    c.app().global::<LibraryState>().set_missing_open(true);
    scan(c);
}

/// The tracks an action applies to: the ticked ones, else all.
fn targets(c: &Ctx) -> Vec<Track> {
    let st = c.state.borrow();
    let ticked: Vec<Track> = st
        .missing
        .rows
        .iter()
        .zip(&st.missing.ticked)
        .filter(|(_, on)| **on)
        .map(|(t, _)| t.clone())
        .collect();
    if ticked.is_empty() {
        st.missing.rows.clone()
    } else {
        ticked
    }
}

/// Moves `tracks` to their new files: `path` a folder = each track's name there; a file = the first track's new file, the
/// others by name next to it. Returns how many were relocated.
fn move_files(c: &Ctx, tracks: &[Track], path: &Path) -> Result<usize, String> {
    let mut n = 0;
    for (i, t) in tracks.iter().enumerate() {
        let Some(lost) = &t.file else { continue };
        let new = if path.is_dir() {
            relocate::beside(&path.join("x"), lost)
        } else if i == 0 {
            path.is_file().then(|| path.to_path_buf())
        } else {
            relocate::beside(path, lost)
        };
        if let Some(new) = new {
            c.store.relocate(t, &new)?;
            n += 1;
        }
    }
    Ok(n)
}

/// RELOCATE answered with `path`.
pub fn relocate_to(c: &Rc<Ctx>, tracks: Vec<Track>, path: PathBuf) {
    match move_files(c, &tracks, &path) {
        Ok(0) => c.say(&format!(
            "nothing relocated: no file found at {}",
            path.display()
        )),
        Ok(n) => {
            c.say(&format!(
                "relocated {n} of {} track{}",
                tracks.len(),
                plural(tracks.len())
            ));
            changed(c);
        }
        Err(e) => c.say(&format!("relocate failed: {e}")),
    }
}

/// Auto Relocate: finds each of `tracks` by name in the search folders.
pub fn auto(c: &Rc<Ctx>, tracks: Vec<Track>) {
    let folders = settings::search_folders(&settings::get());
    if folders.is_empty() {
        return c.say("no search folder: add one in Preferences > Advanced > Database > Auto Relocate Search Folders");
    }
    let (store, names) = (
        c.store.clone(),
        folders
            .iter()
            .map(|f| f.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    );
    c.app().global::<LibraryState>().set_busy(true);
    status(c, &format!("Searching {names}…"));
    c.spawn(
        move || {
            let idx = relocate::index(&folders);
            let total = tracks.len();
            let mut found = 0;
            for t in &tracks {
                let new = t
                    .file
                    .as_deref()
                    .and_then(|f| relocate::find(&idx, f))
                    .map(Path::to_path_buf);
                if let Some(new) = new {
                    if store.relocate(t, &new).is_ok() {
                        found += 1;
                    }
                }
            }
            (found, total)
        },
        |c, (found, total)| {
            c.app().global::<LibraryState>().set_busy(false);
            c.say(&format!("Auto Relocate: {found} of {total} found"));
            changed(c);
        },
    );
}

/// DELETE confirmed: out of the Collection.
pub fn remove(c: &Rc<Ctx>, tracks: Vec<Track>) {
    c.remove_and_say(&tracks);
    changed(c);
}

/// Something moved: the browser re-reads and, when the manager is open, it rescans.
fn changed(c: &Rc<Ctx>) {
    c.reload(None);
    scan(c);
}

pub fn wire(c: &Rc<Ctx>) {
    let app = c.app();
    let s = app.global::<LibraryState>();
    let k = c.clone();
    s.on_missing_toggle(move |i| {
        if let Some(on) = k.state.borrow_mut().missing.ticked.get_mut(i as usize) {
            *on ^= true;
        }
        show(&k);
    });
    let k = c.clone();
    s.on_missing_all(move || {
        {
            let mut st = k.state.borrow_mut();
            let all = st.missing.ticked.iter().all(|&t| t);
            st.missing.ticked.iter_mut().for_each(|t| *t = !all);
        }
        show(&k);
    });
    let k = c.clone();
    s.on_missing_rescan(move || scan(&k));
    let k = c.clone();
    s.on_missing_relocate(move || {
        let t = targets(&k);
        if !t.is_empty() {
            k.ask_path(Ask::Relocate(t), None);
        }
    });
    let k = c.clone();
    s.on_missing_auto(move || auto(&k, targets(&k)));
    let k = c.clone();
    s.on_missing_delete(move || {
        let t = targets(&k);
        if t.is_empty() || k.protected("remove tracks") {
            return;
        }
        let n = t.len();
        let text = format!(
            "Remove {n} missing track{} from the Collection? Playlist places and edits of them are dropped; no file is touched.",
            plural(n)
        );
        k.confirm(Confirm::RemoveMissing(t), "DELETE FROM COLLECTION", &text, "OK", "");
    });
    // Mark missing files from the start; no dialog, no status.
    let tracks = c.lib.tracks(None).unwrap_or_default();
    c.spawn(
        move || {
            tracks
                .into_iter()
                .filter_map(|t| t.file)
                .filter(|f| !f.exists())
                .collect::<HashSet<PathBuf>>()
        },
        |c, gone| {
            let any = !gone.is_empty();
            MARKS.with(|m| *m.borrow_mut() = gone);
            if any {
                c.reload(None); // rows show "[!]" for the files that are gone
            }
        },
    );
}
