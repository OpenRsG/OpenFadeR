//! Library management ↔ ui/library.slint (global `LibraryState`) and the window's menu bar (File / Track / Playlist / View /
//! Help; ui/app.slint). Base: the shared context [`Ctx`] (library, store, threads, status), the path-entry and
//! confirmation dialogs every action goes through, and the dispatch of menu actions ([`act`]).
//!
//! Children, one per feature: [`import`] (File > Import: tracks, folder, playlist file, a collection file through a library plugin),
//! [`track`] (the Track menu on the chosen track), [`missing`] (Missing File Manager, relocate, auto relocate),
//! [`dupes`] (duplicate finder + manager), [`backup`] (Backup / Restore Library), [`playlist`] (Playlist menu: files,
//! sort, renumber), [`export`] (Export Collection: a library plugin writes it), [`settings`] (Preferences > Advanced > Database / Browse),
//! [`dialog`] (what the dialogs ask, the file chooser), [`undo`] (Edit > Undo / Redo over the store's change journal).
//!
//! The browser owns what is selected; the menus read it through [`browser::context`](super::browser::context) and make it
//! reload through `BrowserState.reload`. Our Collection data lives in the store ([`openfader::store`]); the opened library
//! source is never written.
//!
//! Long work (reading tags of a folder, scanning for missing files, copying music) runs on a thread; [`Ctx::spawn`] hands
//! the result back to the UI thread.

mod backup;
mod dialog;
pub mod dupes;
mod export;
mod import;
mod missing;
mod playlist;
pub mod settings;
mod track;
mod undo;

use super::{analyze, deck::Decks, plural, prefs, toast, App, BrowserState, LibraryState};
use openfader::{
    library::{Library, Track},
    store::{self, Store},
};
use slint::{ComponentHandle, Weak};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

pub use dialog::{browse, chooser, open_external, show_in_folder, Ask, Confirm, Pick};
pub use missing::is_missing;

#[derive(Default)]
pub struct State {
    ask: Option<Ask>,
    confirm: Option<Confirm>,
    missing: missing::State,
    dupes: dupes::State,
}

/// Everything the library actions share. One per window (see [`wire`]).
pub struct Ctx {
    app: Weak<App>,
    pub lib: Rc<dyn Library>,
    pub store: Arc<Store>,
    pub decks: Decks,
    pub state: RefCell<State>,
}

thread_local! {
    /// The context, for work coming back from a thread ([`Ctx::spawn`]).
    static CTX: RefCell<Option<Rc<Ctx>>> = const { RefCell::new(None) };
}

impl Ctx {
    pub fn app(&self) -> App {
        self.app.unwrap()
    }

    /// Status line and toast.
    pub fn say(&self, msg: &str) {
        let app = self.app();
        app.global::<BrowserState>().set_status(msg.into());
        toast::show(&app, msg);
    }

    /// The browser re-reads the library (and shows playlist node `select`, if given).
    pub fn reload(&self, select: Option<i64>) {
        self.app()
            .global::<BrowserState>()
            .invoke_reload(select.map_or(-1, |n| n as i32));
    }

    /// Library Protection (Preferences > Advanced > Browse): says so and returns true when `what` must not happen.
    pub fn protected(&self, what: &str) -> bool {
        let on = settings::get().protect;
        if on {
            self.say(&format!(
                "Library Protection is on: cannot {what} (Preferences > Advanced > Browse)"
            ));
        }
        on
    }

    /// The files the Collection lists (what an import must not add twice).
    pub fn known(&self) -> HashSet<PathBuf> {
        self.lib
            .tracks(None)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.file)
            .collect()
    }

    /// Removes `tracks` from the Collection; playlist places of keys no listed track carries any more go too.
    pub fn remove_tracks(&self, tracks: &[Track]) {
        let done = self
            .store
            .action("Remove from Collection", || -> Result<(), String> {
                for t in tracks {
                    self.store.collection_remove(t)?;
                }
                let left: HashSet<String> = self
                    .lib
                    .tracks(None)
                    .unwrap_or_default()
                    .iter()
                    .map(store::key)
                    .collect();
                for k in tracks.iter().map(store::key).filter(|k| !left.contains(k)) {
                    self.store.forget_entries(&k)?;
                }
                Ok(())
            });
        match done {
            Ok(()) => self.reload(None),
            Err(e) => self.say(&format!("store: {e}")),
        }
    }

    /// [`Ctx::remove_tracks`], then says how many left the Collection.
    pub fn remove_and_say(&self, tracks: &[Track]) {
        self.remove_tracks(tracks);
        self.say(&format!(
            "removed {} track{} from the Collection",
            tracks.len(),
            plural(tracks.len())
        ));
    }

    /// Opens the path-entry dialog for `ask` (`initial` overrides its default text).
    pub fn ask_path(&self, ask: Ask, initial: Option<String>) {
        let (title, note, text) = ask.prompt();
        let app = self.app();
        let s = app.global::<LibraryState>();
        s.set_path_title(title.into());
        s.set_path_note(note.into());
        s.set_path_text(initial.unwrap_or(text).into());
        s.set_path_open(true);
        self.state.borrow_mut().ask = Some(ask);
    }

    pub fn confirm(&self, what: Confirm, title: &str, text: &str, yes: &str, no: &str) {
        let app = self.app();
        let s = app.global::<LibraryState>();
        s.set_confirm_title(title.into());
        s.set_confirm_text(text.into());
        s.set_confirm_yes(yes.into());
        s.set_confirm_no(no.into());
        s.set_confirm_open(true);
        self.state.borrow_mut().confirm = Some(what);
    }

    /// Runs `work` on a thread, then `done` with its result on the UI thread.
    pub fn spawn<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&Rc<Ctx>, T) + Send + 'static,
    ) {
        std::thread::spawn(move || {
            let result = work();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(c) = CTX.with(|c| c.borrow().clone()) {
                    done(&c, result);
                }
            });
        });
    }

    /// Analyses `tracks` ([`analyze::request`]: tracks with an Analysis Lock are skipped there). Imports only when Preferences >
    /// Analysis > Auto Analysis is on; `force` (the Analyze Track action) always.
    pub fn analyze(&self, tracks: Vec<Track>, force: bool) {
        if force || prefs::with(|p| p.analysis.auto_analysis) {
            analyze::request(&tracks);
        }
    }
}

/// The path-entry dialog closed: OK runs what it asked for.
fn path_done(c: &Rc<Ctx>, ok: bool) {
    let app = c.app();
    let s = app.global::<LibraryState>();
    s.set_path_open(false);
    let (ask, text) = (
        c.state.borrow_mut().ask.take(),
        s.get_path_text().trim().to_owned(),
    );
    let (Some(ask), true) = (ask, ok) else { return };
    if text.is_empty() {
        return c.say("a path is needed");
    }
    let path = PathBuf::from(&text);
    match ask {
        Ask::ImportFiles => import::files(
            c,
            text.split('|').map(|p| PathBuf::from(p.trim())).collect(),
        ),
        Ask::ImportFolder => import::files(c, vec![path]),
        Ask::ImportPlaylist => import::playlist(c, path),
        Ask::ImportCollection(i) => import::collection(c, path, i),
        Ask::BackupTo { music } => backup::run(c, path, music),
        Ask::Restore => backup::confirm_restore(c, path),
        Ask::ExportList(f) => playlist::write(c, path, f),
        Ask::Relocate(tracks) => missing::relocate_to(c, tracks, path),
        Ask::Connect { .. } => super::connect::add(&path),
    }
}

/// The confirmation closed: 1 = yes, 2 = no, 0 = cancel.
fn confirm_done(c: &Rc<Ctx>, answer: i32) {
    c.app().global::<LibraryState>().set_confirm_open(false);
    let Some(what) = c.state.borrow_mut().confirm.take() else {
        return;
    };
    match (what, answer) {
        (Confirm::WriteBack, n) => super::connect::writeback_answer(n == 1),
        (_, 0) => {}
        (Confirm::BackupMusic, n) => c.ask_path(Ask::BackupTo { music: n == 1 }, None),
        (Confirm::Restore(p), 1) => backup::restore(c, p),
        (Confirm::Remove(t), 1) => track::remove(c, t),
        (Confirm::RemoveMissing(t), 1) => missing::remove(c, t),
        _ => {}
    }
}

/// Opens the ADD dialog of Preferences > Libraries (a folder, or a library file).
pub(in crate::app) fn ask_connect(folder: bool) {
    if let Some(c) = CTX.with(|c| c.borrow().clone()) {
        c.ask_path(Ask::Connect { folder }, None);
    }
}

/// Asks whether to write the planned changes back to a connected library file ([`super::connect`]).
pub(in crate::app) fn ask_writeback(text: &str) {
    if let Some(c) = CTX.with(|c| c.borrow().clone()) {
        c.confirm(Confirm::WriteBack, "WRITE BACK", text, "WRITE", "CANCEL");
    }
}

/// A recording just finished (Preferences > Advanced > Recordings): `import` adds the file to the Collection when it is not
/// there yet (then Auto Analysis may run), `select` shows it in the browser's track panel.
pub(in crate::app) fn import_recorded(app: &App, file: &Path, import: bool, select: bool) {
    let Some(c) = CTX.with(|c| c.borrow().clone()) else {
        return;
    };
    if import && !c.known().contains(file) {
        match openfader::library::import::files(
            &c.store,
            &HashSet::new(),
            &[file.to_path_buf()],
            &|_, _| {},
        ) {
            Ok(r) => {
                c.reload(None);
                c.analyze(
                    vec![Track {
                        file: Some(file.to_path_buf()),
                        ..Track::default()
                    }],
                    false,
                );
                c.say(&format!("recording added to the Collection ({})", r.text()));
            }
            Err(e) => c.say(&format!("recording not imported: {e}")),
        }
    }
    if select {
        app.global::<BrowserState>()
            .invoke_select_file(file.to_string_lossy().as_ref().into());
    }
}

/// Adds `files` (music files and folders) to the Collection, as File > Import does (what a plugin's `import` command runs).
pub(in crate::app) fn import_files(files: Vec<PathBuf>) {
    if let Some(c) = CTX.with(|c| c.borrow().clone()) {
        import::files(&c, files);
    }
}

/// The index after the `:` of an action id like `import-collection:2` (the library plugin's place in its menu).
fn action_index(id: &str) -> Option<usize> {
    id.split_once(':')?.1.parse().ok()
}

/// A menu / context-menu entry (see the ids in ui/app.slint and ui/browser-table.slint).
fn act(c: &Rc<Ctx>, id: &str) {
    match id {
        "source-import" => import::source(c),
        "source-connect" => super::connect::connect_context(),
        "import-track" => c.ask_path(Ask::ImportFiles, None),
        "import-folder" => c.ask_path(Ask::ImportFolder, None),
        "import-playlist" => c.ask_path(Ask::ImportPlaylist, None),
        id if id.starts_with("import-collection:") => {
            if let Some(i) = action_index(id) {
                c.ask_path(Ask::ImportCollection(i), None);
            }
        }
        "missing" => missing::open(c),
        "dupes" => dupes::open(c),
        "backup" => c.confirm(
            Confirm::BackupMusic,
            "BACKUP LIBRARY",
            "Do you want to back up music files as well?",
            "YES",
            "NO",
        ),
        "restore" => c.ask_path(Ask::Restore, None),
        id if id.starts_with("export-collection:") => {
            if let Some(i) = action_index(id) {
                export::collection(c, i);
            }
        }
        "undo" => undo::run(c, false),
        "redo" => undo::run(c, true),
        "exit" => {
            let _ = slint::quit_event_loop();
        }
        id if id.starts_with("track-") => track::act(c, id),
        id if id.starts_with("playlist-") => playlist::act(c, id),
        _ => c.say(&format!("unknown menu action {id}")),
    }
}

/// Connects `LibraryState`, the File / Track / Playlist actions and the library settings (once, UI thread). `lib` is the
/// Collection over the opened source.
pub fn wire(app: &App, lib: Rc<dyn Library>, decks: Decks) {
    settings::wire();
    let c = Rc::new(Ctx {
        app: app.as_weak(),
        lib,
        store: store::global(),
        decks,
        state: RefCell::default(),
    });
    CTX.with(|x| *x.borrow_mut() = Some(c.clone()));
    let s = app.global::<LibraryState>();
    s.set_path_browse(chooser().is_some());
    let k = c.clone();
    s.on_action(move |id| act(&k, &id));
    let k = c.clone();
    s.on_path_done(move |ok| path_done(&k, ok));
    let k = c.clone();
    s.on_confirm_done(move |a| confirm_done(&k, a));
    let k = c.clone();
    s.on_path_browse_clicked(move || {
        let (pick, start) = {
            let st = k.state.borrow();
            (
                st.ask.as_ref().map_or(Pick::File, Ask::pick),
                k.app().global::<LibraryState>().get_path_text().to_string(),
            )
        };
        k.spawn(
            move || browse(pick, &start),
            |c, found| {
                if let Some(p) = found {
                    c.app().global::<LibraryState>().set_path_text(p.into());
                }
            },
        );
    });
    undo::wire(&c);
    missing::wire(&c);
    let k = c.clone();
    s.on_dupes_toggle(move |i| dupes::toggle(&k, i as usize));
    let k = c.clone();
    s.on_dupes_keep(move |i| dupes::keep(&k, i as usize));
    let k = c.clone();
    s.on_dupes_merge(move || dupes::merge(&k));
    let k = c.clone();
    s.on_dupes_delete(move || dupes::delete(&k));
    let k = c.clone();
    s.on_dupes_rescan(move || dupes::scan(&k));
    export::wire(app, &c);
}
