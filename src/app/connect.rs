//! Connections in the window ([`openfader::library::connect`]): the sync of each connected library on a worker thread (its
//! steps in the browser's loading line, a toast when done), the live sync (every [`POLL`] the source's stamp is compared with
//! the last sync's, and a changed source syncs again), the write-back of two-way connections (a dry run, then a confirm
//! dialog in [`super::library`], then the write) and what Preferences > Libraries ([`super::prefs::libraries`]) shows and
//! does. UI thread only, except the workers.

use super::{browser, library, prefs, toast, App, BrowserState};
use openfader::{
    library::connect::{self, Connection, Found, Mode, Plan, Report, Wins},
    store::{self, Store},
};
use slint::{ComponentHandle, Timer, TimerMode, Weak};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How often the connected sources are looked at.
const POLL: Duration = Duration::from_secs(5);

/// How often a connected folder (walked whole by [`connect::stamp`]) is looked at.
const FOLDER_POLL: Duration = Duration::from_secs(30);

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
        return "never".into();
    }
    let d = (now() - at).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        _ => format!("{} d ago", d / 86400),
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
                        &format!("Syncing {label}: {done} / {total} tracks"),
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
            toast::show(&app, &format!("{}: sync failed: {e}", c.label));
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
        return say("choose All tracks or a playlist of a library source first");
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
            say(&format!("Connected {}", c.label));
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
            say(&format!("Connected {}", c.label));
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

/// Looks at every connection once: a changed source syncs, new edits of a two-way connection are planned for write-back.
fn tick() {
    let store = store::global();
    for c in store.connections().unwrap_or_default() {
        if with(|s| s.busy.contains(&c.id)) {
            continue;
        }
        // a folder is walked whole: looked at every FOLDER_POLL, not every POLL
        if !c.path.is_file()
            && c.last_sync != 0
            && with(|s| {
                let due = s
                    .checked
                    .get(&c.id)
                    .is_none_or(|t| t.elapsed() >= FOLDER_POLL);
                if due {
                    s.checked.insert(c.id, Instant::now());
                }
                !due
            })
        {
            continue;
        }
        let stamp = connect::stamp(&c);
        let skip = stamp.is_empty() && c.last_sync != 0 // source gone (USB out): wait for it
            || with(|s| s.failed.get(&c.id) == Some(&stamp));
        if !skip && (c.last_sync == 0 || stamp != c.stamp) {
            start_sync(c);
        } else if c.mode == Mode::TwoWay && !skip && with(|s| s.plan.is_none() && !s.planning) {
            let seq = store.journal_seq().unwrap_or(0);
            if with(|s| s.seen.insert(c.id, seq)).is_some_and(|old| old != seq) {
                start_plan(c);
            }
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
                Ok(n) => say(&format!(
                    "{label}: wrote {n} tracks (backup in the backups folder)"
                )),
                Err(e) => {
                    // plan again on the next look: the source or the pending edits may still differ
                    with(|s| s.seen.insert(id, 0));
                    say(&format!("{label}: not written: {e}"))
                }
            }
            prefs::refresh();
        });
    });
}

/// Connects the start-up libraries: `legacy` (the path given at start or the old Database setting) and the own music folder.
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
    rescan();
    if let Some(f) = found()
        .first()
        .filter(|_| store.connections().is_ok_and(|c| c.len() <= 1))
    {
        toast::show(
            app,
            &format!("Found {}: connect it in Preferences > Libraries", f.label),
        );
    }
    TIMER.with(|t| t.start(TimerMode::Repeated, POLL, tick));
    tick();
}
