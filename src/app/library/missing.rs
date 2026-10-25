//! Missing File Manager (File > Display All Missing Files): the tracks whose audio file is gone, with
//! RELOCATE (new file for the first track, the others from its folder, or a folder for all), AUTO RELOCATE (search the
//! Auto Relocate Search Folders and the library's connected folders by file name, else by content print, else by the track's audio fingerprint, [`relocate`]) and DELETE (remove from the Collection). Ticking nothing
//! acts on every listed track. Also the Track menu's Relocate / Auto Relocate for the chosen track.
//!
//! A scan stats every track's file on a thread (a USB stick is slow); [`is_missing`] answers from the last scan.

use super::{Ask, Confirm, Ctx, settings};
use crate::app::{LibraryState, MissingRow};
use openfader::{
    library::{print, relocate, Track},
    store::Store,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

#[derive(Default)]
pub struct State {
    rows: Vec<Track>,
    /// Where each row's file went, once [`propose`] found out (parallel to `rows`; shorter until then).
    found: Vec<Option<PathBuf>>,
    ticked: Vec<bool>,
}

thread_local! {
    /// Files the last scan did not find.
    static MARKS: RefCell<HashSet<PathBuf>> = RefCell::default();
    static VERSION: Cell<u64> = const { Cell::new(0) };
}

/// Whether the last scan found no file at `path` (always false before the first scan finishes).
pub fn is_missing(path: &Path) -> bool {
    MARKS.with(|m| m.borrow().contains(path))
}

pub(in crate::app) fn version() -> u64 {
    VERSION.with(Cell::get)
}

pub(in crate::app) fn update_marks(marks: HashSet<PathBuf>) {
    MARKS.with(|m| {
        if *m.borrow() != marks {
            *m.borrow_mut() = marks;
            VERSION.with(|v| v.set(v.get().wrapping_add(1)));
        }
    });
}

fn show(c: &Ctx) {
    let st = c.state.borrow();
    let rows: Vec<MissingRow> = st
        .missing
        .rows
        .iter()
        .zip(&st.missing.ticked)
        .enumerate()
        .map(|(i, (t, &on))| MissingRow {
            title: format!("{} — {}", t.title, t.artist).into(),
            path: t
                .file
                .as_ref()
                .map_or_else(String::new, |f| f.display().to_string())
                .into(),
            found: st
                .missing
                .found
                .get(i)
                .and_then(Option::as_ref)
                .map_or_else(String::new, |p| tr!("found: {}", p.display()))
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
        Err(e) => return status(c, &tr!("scan failed: {}", e)),
    };
    let app = c.app();
    let s = app.global::<LibraryState>();
    s.set_busy(true);
    status(c, tr!("Searching…"));
    c.spawn(
        move || {
            tracks
                .into_iter()
                .filter(|t| t.file.as_deref().is_some_and(|f| !f.exists()))
                .collect::<Vec<Track>>()
        },
        |c, gone| {
            update_marks(gone.iter().filter_map(|t| t.file.clone()).collect());
            let msg = if gone.is_empty() {
                tr!("File is not missing: every track has its file").to_owned()
            } else {
                tr_n!(
                    "File is Missing: {n} track",
                    "File is Missing: {n} tracks",
                    gone.len()
                )
            };
            c.state.borrow_mut().missing = State {
                ticked: vec![false; gone.len()],
                rows: gone,
                ..State::default()
            };
            c.app().global::<LibraryState>().set_busy(false);
            status(c, &msg);
            show(c);
            propose(c);
        },
    );
}

/// Looks for where each missing track went (by name, else by content print: no decoding, [`relocate::locate`]) and shows it
/// under the old path; AUTO RELOCATE does that and then decodes.
fn propose(c: &Rc<Ctx>) {
    let (store, folders, tracks) = (
        c.store.clone(),
        search_folders(&c.store),
        c.state.borrow().missing.rows.clone(),
    );
    if folders.is_empty() || tracks.is_empty() {
        return;
    }
    let ids: Vec<_> = tracks.iter().map(|t| t.id).collect();
    c.spawn(
        move || {
            let idx = relocate::index(&folders);
            let mut by_print = relocate::ByPrint::new(&idx);
            tracks
                .iter()
                .map(|t| relocate::locate(&idx, &mut by_print, &store, t).map(Path::to_path_buf))
                .collect::<Vec<_>>()
        },
        move |c, found| {
            // A rescan in between made other rows: those answers are not for them.
            if c.state.borrow().missing.rows.iter().map(|t| t.id).eq(ids) {
                c.state.borrow_mut().missing.found = found;
                show(c);
            }
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
        Ok(0) => c.say(&tr!(
            "nothing relocated: no file found at {}",
            path.display()
        )),
        Ok(n) => {
            c.say(&tr_n!(
                "relocated {} of {n} track",
                "relocated {} of {n} tracks",
                tracks.len(),
                n
            ));
            changed(c);
        }
        Err(e) => c.say(&tr!("relocate failed: {}", e)),
    }
}

/// Auto Relocate: finds each of `tracks` by name in the search folders, else by content print ([`relocate::locate`]: a moved
/// and renamed file), else by its audio ([`relocate::ByAudio`]; at most [`relocate::MAX_DECODES`] candidates are decoded per track).
pub fn auto(c: &Rc<Ctx>, tracks: Vec<Track>) {
    let folders = search_folders(&c.store);
    if folders.is_empty() {
        return c.say(tr!("no search folder: add one in Preferences > Advanced > Database > Auto Relocate Search Folders"));
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
    status(c, &tr!("Searching {}…", names));
    c.spawn(
        move || {
            let idx = relocate::index(&folders);
            let mut by_print = relocate::ByPrint::new(&idx);
            let mut by_audio = relocate::ByAudio::new(&idx);
            let total = tracks.len();
            let mut found = 0;
            for t in &tracks {
                let new = relocate::locate(&idx, &mut by_print, &store, t)
                    .or_else(|| by_audio.find(&store, t))
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
            c.say(&tr!("Auto Relocate: {} of {} found", found, total));
            changed(c);
        },
    );
}

/// The folders a search looks in: the Auto Relocate Search Folders and the folders of the library's connections.
fn search_folders(store: &Store) -> Vec<PathBuf> {
    let mut folders = settings::search_folders(&settings::get());
    for conn in store.connections().unwrap_or_default() {
        if conn.path.is_dir() && !folders.contains(&conn.path) {
            folders.push(conn.path);
        }
    }
    folders
}

/// Makes the content prints of the Collection's files on a low-priority thread, slowly, once the app has started: what finds a
/// moved file again and matches device copies. Files that cannot be read are skipped for this run.
fn fill_prints(store: Arc<Store>) {
    std::thread::spawn(move || {
        crate::app::analyze::lower_priority();
        std::thread::sleep(Duration::from_secs(10));
        let mut done = HashSet::new();
        loop {
            let before = done.len();
            print::fill_pass(&store, 200, &mut done);
            if done.len() == before {
                break;
            }
        }
    });
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
        if t.is_empty() || k.protected(tr!("remove tracks")) {
            return;
        }
        let n = t.len();
        let text = tr_n!(
            "Remove {n} missing track from the Collection? Playlist places and edits of them are dropped; no file is touched.",
            "Remove {n} missing tracks from the Collection? Playlist places and edits of them are dropped; no file is touched.",
            n
        );
        k.confirm(Confirm::RemoveMissing(t), tr!("DELETE FROM COLLECTION"), &text, tr!("OK"), "");
    });
    // Mark missing files from the start; no dialog, no status. The shared list (no copy on the UI thread).
    let tracks = c.lib.tracks_all().unwrap_or_default();
    c.spawn(
        move || {
            tracks
                .iter()
                .filter_map(|t| t.file.as_ref())
                .filter(|f| !f.exists())
                .cloned()
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
    fill_prints(c.store.clone());
}
