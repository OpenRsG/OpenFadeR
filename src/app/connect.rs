//! Connections in the window ([`openfader::library::connect`]): the sync of each connected library on a worker thread (its
//! steps in the browser's loading line, a toast when done), the live sync (when the OS reports a change in a source, or after
//! a long fallback ([`FILE_FALLBACK`], [`FOLDER_FALLBACK`]), its stamp is compared with the last sync's, and a changed source syncs
//! again; without change notification, [`watch`], every [`POLL`] for a file and every [`FOLDER_POLL`] for a folder), the write-back of two-way connections (a dry run, then a confirm
//! dialog in [`super::library`], then the write) and what Preferences > Libraries ([`super::prefs::libraries`]) shows and
//! does. UI thread only, except the workers.

use super::{browser, library, prefs, toast, App, BrowserState};
use openfader::{
    library::connect::{self, Connection, Found, Mode, Plan, Report, Wins},
    store::{self, Store},
};
use slint::{ComponentHandle, Timer, TimerMode, Weak};
use watch::Watch;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

mod watch;

/// How often the tick runs (it also reads the journal of two-way connections); without change notification it reads each
/// connected library file's stamp too.
const POLL: Duration = Duration::from_secs(5);

/// Without change notification: how often a connected folder (walked whole by [`connect::stamp`]) is looked at.
const FOLDER_POLL: Duration = Duration::from_secs(30);

/// With change notification, the longest a library file goes unread (what the notification may miss: a source on a stick plugged
/// in again, more folders than watches).
const FILE_FALLBACK: Duration = Duration::from_secs(60);

/// The same for a folder.
const FOLDER_FALLBACK: Duration = Duration::from_secs(300);

/// Folders below a plugin's folder that [`connect::stamp`] looks at (its `STAMP_DEPTH`).
const PLUGIN_DEPTH: usize = 3;

#[derive(Default)]
struct State {
    /// A write-back is being planned: no other connection plans meanwhile.
    planning: bool,
    /// When each folder connection's stamp was last read.
    checked: HashMap<i64, Instant>,
    app: Option<Weak<App>>,
    /// Connections being synced or planned right now.
    busy: HashSet<i64>,
    /// The source stamp a failed sync saw: not tried again until the source changes.
    failed: HashMap<i64, String>,
    /// The journal position each two-way connection was last looked at.
    seen: HashMap<i64, i64>,
    /// The write-back waiting for the user's answer.
    plan: Option<Plan>,
    /// Libraries other apps keep in their usual places, not connected.
    found: Vec<Found>,
    /// The change notification of the sources; `None` where there is none.
    watch: Option<Watch>,
    /// A change was reported while its connection was busy: read the stamps again at the next tick.
    rearm: bool,
    /// Connections whose source stamp a worker is reading ([`tick`]).
    stamping: HashSet<i64>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
    static TIMER: Timer = Timer::default();
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with_borrow_mut(f)
}

fn app() -> Option<App> {
    with(|s| s.app.as_ref().and_then(Weak::upgrade))
}

fn say(text: &str) {
    if let Some(app) = app() {
        toast::show(&app, text);
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// What the Libraries page shows of one connection.
pub struct View {
    pub conn: Connection,
    pub tracks: usize,
    pub busy: bool,
}

/// The connections with their track counts.
pub fn views() -> Vec<View> {
    let store = store::global();
    store
        .connections()
        .unwrap_or_default()
        .into_iter()
        .map(|conn| View {
            tracks: store.connection_tracks(conn.id).unwrap_or(0),
            busy: with(|s| s.busy.contains(&conn.id)),
            conn,
        })
        .collect()
}

/// The detected, unconnected libraries ([`rescan`]).
pub fn found() -> Vec<Found> {
    with(|s| s.found.clone())
}

/// Looks for libraries in the plugins' default places again.
pub fn rescan() {
    let found = connect::detect(&store::global()).unwrap_or_default();
    with(|s| s.found = found);
}

/// "3 min ago" style text for a unix time.
pub fn ago(at: i64) -> String {
    if at == 0 {
        return tr!("never").into();
    }
    let d = (now() - at).max(0);
    match d {
        0..=59 => tr!("just now").into(),
        60..=3599 => tr!("{} min ago", d / 60),
        3600..=86399 => tr!("{} h ago", d / 3600),
        _ => tr!("{} d ago", d / 86400),
    }
}

/// Starts the sync of `c` on a worker (no-op while it is syncing).
fn start_sync(c: Connection) {
    if !with(|s| s.busy.insert(c.id)) {
        return;
    }
    let weak = with(|s| s.app.clone());
    let label = c.label.clone();
    std::thread::spawn(move || {
        let store = store::global();
        let stamp = connect::stamp(&c);
        let progress = |done: usize, total: usize| {
            if done % 100 != 0 && done != total {
                return;
            }
            let (weak, label) = (weak.clone(), label.clone());
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.and_then(|w| w.upgrade()) {
                    let frac = if total == 0 {
                        0.0
                    } else {
                        done as f32 / total as f32
                    };
                    browser::loading(
                        &app,
                        &tr!("Syncing {}: {} / {} tracks", label, done, total),
                        Some(frac),
                    );
                }
            });
        };
        let result = connect::sync(&store, &c, &progress);
        let _ = slint::invoke_from_event_loop(move || synced(&c, stamp, result));
    });
}

/// A sync finished.
fn synced(c: &Connection, stamp: String, result: Result<Report, String>) {
    with(|s| s.busy.remove(&c.id));
    let Some(app) = app() else { return };
    browser::loading(&app, "", None);
    match result {
        Ok(r) => {
            with(|s| s.failed.remove(&c.id));
            if r != Report::default() {
                app.global::<BrowserState>().invoke_reload(-1);
                toast::show(&app, &format!("{}: {}", c.label, r.text()));
            }
        }
        Err(e) => {
            with(|s| s.failed.insert(c.id, stamp));
            toast::show(&app, &tr!("{}: sync failed: {}", c.label, e));
        }
    }
    prefs::refresh();
}

/// SYNC NOW.
pub fn sync_now(id: i64) {
    with(|s| s.failed.remove(&id));
    if let Some(c) = store::global()
        .connections()
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.id == id)
    {
        start_sync(c);
    }
}

/// CONNECT on the Media Browser library on screen: the same library file or folder, synced from now on.
pub fn connect_context() {
    let Some((path, kind, _)) = browser::context().source else {
        return say(tr!(
            "choose All tracks or a playlist of a library source first"
        ));
    };
    let (plugin, label) = match kind {
        browser::SourceType::Plugin(p) => (p.clone(), p),
        browser::SourceType::Folder => (
            String::new(),
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
        ),
    };
    match connect::connect(&store::global(), &plugin, &path, &label) {
        Ok(c) => {
            say(&tr!("Connected {}", c.label));
            rescan();
            start_sync(c);
        }
        Err(e) => say(&e),
    }
}

/// ADD…: the path the user chose.
pub fn add(path: &Path) {
    let store = store::global();
    match connect::adopt(&store, path) {
        Ok(c) => {
            say(&tr!("Connected {}", c.label));
            rescan();
            start_sync(c);
        }
        Err(e) => say(&e),
    }
    prefs::refresh();
}

/// CONNECT on a detected library.
pub fn connect_found(i: usize) {
    let Some(f) = with(|s| s.found.get(i).cloned()) else {
        return;
    };
    match connect::connect(&store::global(), &f.plugin, &f.path, &f.label) {
        Ok(c) => {
            rescan();
            start_sync(c);
        }
        Err(e) => say(&e),
    }
}

/// DISCONNECT: the maps go, the tracks stay.
pub fn disconnect(id: i64) {
    if let Err(e) = store::global().connection_delete(id) {
        say(&e);
    }
    rescan();
}

pub fn set_mode(id: i64, mode: Mode) {
    update(id, |_, wins| (mode, wins));
}

pub fn set_wins(id: i64, wins: Wins) {
    update(id, |mode, _| (mode, wins));
}

fn update(id: i64, f: impl FnOnce(Mode, Wins) -> (Mode, Wins)) {
    let store = store::global();
    if let Some(c) = store
        .connections()
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.id == id)
    {
        let (mode, wins) = f(c.mode, c.wins);
        if let Err(e) = store.connection_set(id, mode, wins) {
            say(&e);
        }
    }
}

/// What the watcher is told to watch for `c`: the folder (to the depth its stamp reads), or the parent of a library file.
fn root(c: &Connection) -> watch::Root {
    if c.path.is_file() {
        (c.path.parent().unwrap_or(&c.path).to_owned(), 0)
    } else {
        (c.path.clone(), if c.plugin.is_empty() { usize::MAX } else { PLUGIN_DEPTH })
    }
}

/// Whether a source's stamp is read at this tick: when something in it changed, or when it was last read `since` ago and that is
/// longer than the interval that applies (a folder is walked whole, so it waits longer than a file).
fn due(is_file: bool, watching: bool, changed: bool, since: Option<Duration>) -> bool {
    let every = match (watching, is_file) {
        (false, true) => Duration::ZERO,
        (false, false) => FOLDER_POLL,
        (true, true) => FILE_FALLBACK,
        (true, false) => FOLDER_FALLBACK,
    };
    changed || since.is_none_or(|t| t >= every)
}

/// Looks at every connection once: a changed source syncs, new edits of a two-way connection are planned for write-back. A
/// source's stamp (a whole folder walk for a folder) is read on a worker, so neither the start nor a tick waits for the disk.
fn tick() {
    let connections = store::global().connections().unwrap_or_default();
    let (watching, changed) = with(|s| {
        let watching = s.watch.as_ref().is_some_and(Watch::alive);
        if let Some(w) = s.watch.as_mut().filter(|_| watching) {
            w.set(connections.iter().map(root).collect());
        }
        let changed = std::mem::take(&mut s.rearm) | s.watch.as_ref().is_some_and(Watch::take);
        (watching, changed)
    });
    for c in connections {
        if with(|s| s.busy.contains(&c.id)) {
            if changed {
                with(|s| s.rearm = true);
            }
            continue;
        }
        let read = c.last_sync == 0
            || with(|s| {
                let since = s.checked.get(&c.id).map(Instant::elapsed);
                let due = due(c.path.is_file(), watching, changed, since);
                if due {
                    s.checked.insert(c.id, Instant::now());
                }
                due
            });
        if !read {
            let stamp = c.stamp.clone();
            decide(c, stamp);
        } else if with(|s| s.stamping.insert(c.id)) {
            std::thread::spawn(move || {
                let stamp = connect::stamp(&c);
                let _ = slint::invoke_from_event_loop(move || {
                    with(|s| s.stamping.remove(&c.id));
                    decide(c, stamp);
                });
            });
        }
    }
}

/// What [`tick`] does with connection `c` once its source's `stamp` is known: sync a changed source, else plan a write-back.
fn decide(c: Connection, stamp: String) {
    let skip = stamp.is_empty() && c.last_sync != 0 // source gone (USB out): wait for it
        || with(|s| s.failed.get(&c.id) == Some(&stamp));
    if !skip && (c.last_sync == 0 || stamp != c.stamp) {
        start_sync(c);
    } else if c.mode == Mode::TwoWay && !skip && with(|s| s.plan.is_none() && !s.planning) {
        let seq = store::global().journal_seq().unwrap_or(0);
        if with(|s| s.seen.insert(c.id, seq)).is_some_and(|old| old != seq) {
            start_plan(c);
        }
    }
}

/// The dry run of a write-back on a worker; a change asks the user.
fn start_plan(c: Connection) {
    if !with(|s| s.busy.insert(c.id)) {
        return;
    }
    // The slot is reserved now: no second connection plans (and asks) until this one is answered.
    with(|s| s.planning = true);
    std::thread::spawn(move || {
        let r = connect::plan(&store::global(), &c);
        let _ = slint::invoke_from_event_loop(move || {
            with(|s| {
                s.busy.remove(&c.id);
                s.planning = false;
            });
            match r {
                Ok(Some(plan)) => {
                    library::ask_writeback(&plan.text());
                    with(|s| s.plan = Some(plan));
                }
                Ok(None) => {}
                Err(e) => say(&format!("{}: {e}", c.label)),
            }
        });
    });
}

/// The confirm dialog closed: `yes` writes the planned file.
pub fn writeback_answer(yes: bool) {
    let Some(plan) = with(|s| s.plan.take()) else {
        return;
    };
    if !yes {
        return;
    }
    let label = plan.conn.label.clone();
    with(|s| s.busy.insert(plan.conn.id));
    let id = plan.conn.id;
    std::thread::spawn(move || {
        let r = connect::apply(&store::global(), plan);
        let _ = slint::invoke_from_event_loop(move || {
            with(|s| s.busy.remove(&id));
            match r {
                Ok(n) => say(&tr!(
                    "{}: wrote {} tracks (backup in the backups folder)",
                    label,
                    n
                )),
                Err(e) => {
                    // plan again on the next look: the source or the pending edits may still differ
                    with(|s| s.seen.insert(id, 0));
                    say(&tr!("{}: not written: {}", label, e))
                }
            }
            prefs::refresh();
        });
    });
}

/// Connects the start-up libraries: `legacy` (the path given at start or the old Database setting) and the own music folder,
/// then reads the Collection on a worker while the window is built: the browser finds the list ready.
pub fn adopt_startup(store: &Store, legacy: Option<PathBuf>, music: Option<PathBuf>) {
    if let Some(p) = legacy {
        if let Err(e) = connect::adopt(store, &p) {
            eprintln!("{}: {e}", p.display());
        }
    }
    if let Some(m) = music {
        if let Err(e) = connect::own_folder(store, &m) {
            eprintln!("{}: {e}", m.display());
        }
    }
    std::thread::spawn(|| {
        if let Err(e) = store::global().collection_edited() {
            eprintln!("store: {e}");
        }
    });
}

/// Starts the live sync and offers detected libraries on a first run (UI thread, once).
pub fn wire(app: &App) {
    let store = store::global();
    with(|s| {
        s.app = Some(app.as_weak());
        s.seen = store
            .connections()
            .unwrap_or_default()
            .into_iter()
            // a write-back that was pending when the app last ran is found by the first tick
            .map(|c| (c.id, c.last_seq))
            .collect();
    });
    with(|s| s.watch = Watch::start());
    rescan();
    if let Some(f) = found()
        .first()
        .filter(|_| store.connections().is_ok_and(|c| c.len() <= 1))
    {
        toast::show(
            app,
            &tr!("Found {}: connect it in Preferences > Libraries", f.label),
        );
    }
    TIMER.with(|t| t.start(TimerMode::Repeated, POLL, tick));
    tick();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_notification_files_are_read_every_tick_and_folders_every_30_s() {
        assert!(due(true, false, false, Some(Duration::ZERO)));
        assert!(!due(false, false, false, Some(Duration::from_secs(29))));
        assert!(due(false, false, false, Some(FOLDER_POLL)));
        assert!(due(false, false, false, None), "never read: now");
    }

    #[test]
    fn with_notification_a_source_is_read_on_change_or_after_the_long_fallback() {
        assert!(!due(true, true, false, Some(Duration::from_secs(59))));
        assert!(due(true, true, false, Some(FILE_FALLBACK)));
        assert!(!due(false, true, false, Some(Duration::from_secs(299))));
        assert!(due(false, true, false, Some(FOLDER_FALLBACK)));
        assert!(due(false, true, true, Some(Duration::ZERO)), "a reported change reads at once");
    }
}
