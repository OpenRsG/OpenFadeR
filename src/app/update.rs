//! The update check and the one-click update ↔ ui/update.slint (`UpdateState`).
//!
//! At start (PREFS > ADVANCED > Check for updates, on by default, at most once a day) a worker thread asks GitHub for the
//! latest release ([`openfader::update`]); a newer one is announced by a toast and by Help > Update to …, which opens the
//! window with its notes, version and download size: UPDATE NOW, SKIP THIS VERSION, LATER. Help > Check for Updates asks
//! again whenever it is chosen. UPDATE NOW downloads the archive for this system with a progress bar, checks its SHA-256
//! against GitHub's digest, swaps it in next to the old version (kept in `<install>.prev`) and restarts into it; an install
//! the app must not touch opens the release page. The worker also reads the plugin repositories ([`openfader::catalog`], the
//! GET PLUGINS code) and counts the installed plugins that have a newer version there.
//!
//! The version just installed must show that it starts: 20 s of running or a clean exit. A version that was started twice
//! without that gets ROLL BACK highlighted, and `openfader --rollback` does the same without a window.

use super::{library, prefs, toast, App, UpdateState};
use openfader::catalog::{self, plugins::Entry};
use openfader::update::{self, install, state, Release};
use slint::{ComponentHandle, Timer, TimerMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{cell::Cell, cell::RefCell};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How long a freshly installed version runs before it counts as started.
const CONFIRM_AFTER: Duration = Duration::from_secs(20);
/// The most release notes shown (characters).
const NOTES: usize = 8000;

/// How long the restart after a roll-back that put the library back waits, so that the text can be read.
const LIBRARY_NOTICE: Duration = Duration::from_secs(6);

thread_local! {
    /// The newer release the window offers (UI thread).
    static FOUND: RefCell<Option<Release>> = const { RefCell::new(None) };
    /// A check or an update runs.
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static CONFIRM: Timer = Timer::default();
}
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Of `entries`, the plugins installed in another version than the repository's (GET PLUGINS shows UPDATE for them).
fn outdated(entries: &[Entry], installed: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    entries
        .iter()
        .filter(|e| installed(&e.manifest.name).is_some_and(|v| v != e.manifest.version))
        .map(|e| e.manifest.name.clone())
        .collect()
}

/// The installed plugins that have another version in one of the repositories (downloads each again: worker thread).
fn plugin_updates() -> Vec<String> {
    let now = openfader::plugin::current();
    let installed = |name: &str| {
        now.plugins
            .iter()
            .chain(&now.disabled)
            .find(|p| p.manifest.name == name)
            .map(|p| p.manifest.version.clone())
    };
    let mut names: Vec<String> = catalog::list::load()
        .iter()
        .filter_map(|repo| catalog::read(repo, true).ok())
        .flat_map(|found| outdated(&found.plugins, &installed))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The release notes as plain text, cut to [`NOTES`].
fn notes(r: &Release) -> String {
    let text: String = r
        .notes
        .as_deref()
        .unwrap_or_default()
        .replace('\r', "")
        .chars()
        .take(NOTES)
        .collect();
    if text.trim().is_empty() {
        tr!("No release notes.").into()
    } else {
        text
    }
}

/// Asks GitHub on a worker thread; `auto` (the start-up check) records the day and says nothing unless there is news.
fn check(app: &App, auto: bool) {
    if BUSY.replace(true) {
        return;
    }
    let s = app.global::<UpdateState>();
    s.set_busy(true);
    if !auto {
        s.set_status(tr!("Checking for updates…").into());
    }
    let channel = prefs::with(|p| p.advanced.update_channel.channel());
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let release = update::latest(&update::api_base(), channel);
        let reached = release.is_ok();
        if reached && auto {
            state::update(|s| s.checked = state::now());
        }
        let again = weak.clone();
        let _ = weak.upgrade_in_event_loop(move |app| found(&app, release, auto));
        if reached {
            let names = plugin_updates();
            let _ = again.upgrade_in_event_loop(move |app| addons(&app, &names, auto));
        }
    });
}

/// The answer of [`check`].
fn found(app: &App, answer: Result<Release, String>, auto: bool) {
    BUSY.set(false);
    let s = app.global::<UpdateState>();
    s.set_busy(false);
    let rel = match answer {
        Ok(rel) => rel,
        Err(e) => {
            if !auto {
                s.set_status(tr!("Could not check for updates: {}", e).into());
            }
            return;
        }
    };
    let version = rel.version().filter(|_| rel.newer_than(VERSION));
    let Some(version) = version.map(|v| v.to_string()) else {
        FOUND.set(None);
        s.set_version("".into());
        s.set_status(tr!("OpenFadeR {} is the latest version.", VERSION).into());
        return;
    };
    if auto && state::file().is_some_and(|f| state::read(&f).skipped == version) {
        return;
    }
    let (asset, why) = (rel.asset(), install::locate().err());
    s.set_notes(notes(&rel).into());
    s.set_size(
        asset
            .map(|a| format!("{:.0} MB", a.size as f64 / 1e6))
            .unwrap_or_default()
            .into(),
    );
    s.set_can_update(asset.is_some() && why.is_none());
    let status = match (asset, why) {
        (None, _) => {
            tr!("This release has no archive for this system: UPDATE opens its page.").to_owned()
        }
        (_, Some(w)) => tr!("Not updated here ({}): UPDATE opens the release page.", w),
        _ => String::new(),
    };
    s.set_status(status.into());
    s.set_version(version.as_str().into());
    FOUND.set(Some(rel));
    if auto {
        toast::show(
            app,
            &tr!("OpenFadeR {} is available: Help > Update", version),
        );
    } else {
        s.set_visible(true);
    }
}

/// Plugin updates the repositories offer.
fn addons(app: &App, names: &[String], auto: bool) {
    if names.is_empty() {
        return;
    }
    let text = tr_n!(
        "{n} plugin has an update (GET PLUGINS): {}",
        "{n} plugins have updates (GET PLUGINS): {}",
        names.len(),
        names.join(", ")
    );
    app.global::<UpdateState>().set_addons(text.as_str().into());
    if auto {
        toast::show(app, &text);
    }
}

/// UPDATE NOW's first step: the automatic backup of the library (and the settings file) in the data folder; the last 3 stay.
fn back_up_library(
    store: &openfader::store::Store,
    settings: Option<&std::path::Path>,
    dir: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    let dir = dir.ok_or("no data directory (set XDG_DATA_HOME or HOME)")?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    store.backup_auto(dir, settings, VERSION, &stamp, 3)
}

/// UPDATE NOW: download, verify, swap, restart; or the release page when this install is not ours to replace.
fn update_now(app: &App) {
    let Some(rel) = FOUND.with_borrow(Clone::clone) else {
        return;
    };
    let s = app.global::<UpdateState>();
    let (Some(asset), Ok(install)) = (rel.asset().cloned(), install::locate()) else {
        let _ = library::open_external(&rel.page);
        s.set_status(tr!("The release page is open in your browser.").into());
        return;
    };
    if BUSY.replace(true) {
        return;
    }
    CANCEL.store(false, Ordering::Relaxed);
    s.set_busy(true);
    s.set_downloading(true);
    s.set_progress(0.0);
    let to = rel.version().map(|v| v.to_string()).unwrap_or_default();
    let backup = prefs::with(|p| p.advanced.backup_before_update).then(|| {
        (
            openfader::store::global(),
            prefs::path(),
            openfader::store::backup::auto_dir(),
        )
    });
    s.set_status(
        if backup.is_some() {
            tr!("Backing up the library…")
        } else {
            tr!("Downloading…")
        }
        .into(),
    );
    let weak = app.as_weak();
    std::thread::spawn(move || {
        // The library first: a roll-back to this version puts it back when the new one migrated the store.
        let mut saved = String::new();
        if let Some((store, settings, dir)) = backup {
            match back_up_library(&store, settings.as_deref(), dir.as_deref()) {
                Ok(zip) => saved = zip.display().to_string(),
                Err(e) => {
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        installed(&app, Err(format!("the library could not be backed up first ({e}); PREFS > ADVANCED can turn that off")), &install, to, String::new())
                    });
                    return;
                }
            }
        }
        let dest = openfader::paths::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("update")
            .join(&asset.name);
        let (mut last, total, w) = (None::<Instant>, asset.size, weak.clone());
        let got = install::download(&asset, &dest, &CANCEL, &mut |n| {
            if last.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
                last = Some(Instant::now());
                let _ = w.upgrade_in_event_loop(move |app| progress(&app, n, total));
            }
        });
        if got.is_ok() {
            let _ = weak.upgrade_in_event_loop(|app| {
                let s = app.global::<UpdateState>();
                s.set_downloading(false);
                s.set_status(tr!("Installing…").into());
            });
        }
        let done = got.and_then(|_| install::apply(&install, &dest));
        let _ = std::fs::remove_file(&dest);
        let _ = weak.upgrade_in_event_loop(move |app| installed(&app, done, &install, to, saved));
    });
}

fn progress(app: &App, got: u64, total: u64) {
    let s = app.global::<UpdateState>();
    s.set_progress(if total == 0 {
        0.0
    } else {
        got as f32 / total as f32
    });
    s.set_status(
        tr!(
            "Downloading… {} of {} MB",
            got / 1_000_000,
            total / 1_000_000
        )
        .into(),
    );
}

/// The update ended: restart into it, or say why not.
fn installed(
    app: &App,
    done: Result<(), String>,
    install: &install::Install,
    to: String,
    backup: String,
) {
    BUSY.set(false);
    let s = app.global::<UpdateState>();
    s.set_busy(false);
    s.set_downloading(false);
    match done {
        Ok(()) => {
            state::update(|s| {
                s.applied = Some(state::Applied {
                    from: VERSION.into(),
                    to,
                    schema: openfader::store::backup::schema(),
                    backup,
                    ..Default::default()
                })
            });
            s.set_status(tr!("Installed. Restarting…").into());
            super::restart_as(install.exe_path());
        }
        Err(_) if CANCEL.load(Ordering::Relaxed) => s.set_status(tr!("Cancelled.").into()),
        Err(e) => s.set_status(tr!("The update failed, nothing was changed: {}", e).into()),
    }
}

/// ROLL BACK: the version in `<install>.prev` comes back and the program restarts into it. When the library from before the
/// update was put back, the restart waits a few seconds so that the text can be read.
fn roll_back(app: &App) {
    let s = app.global::<UpdateState>();
    match update::roll_back() {
        Ok(done) => {
            s.set_rollback("".into());
            let exe = done.install.exe_path();
            let Some(library) = done.library else {
                return super::restart_as(exe);
            };
            s.set_status(
                tr!(
                    "Rolled back. The library from before the update was put back; the newer one is kept as {}. Restarting…",
                    library.kept.display()
                )
                .into(),
            );
            Timer::single_shot(LIBRARY_NOTICE, move || super::restart_as(exe));
        }
        Err(e) => s.set_status(tr!("Could not roll back: {}", e).into()),
    }
}

/// Help > Check for Updates: the window, with a fresh check unless a newer release is already known.
fn open(app: &App) {
    app.global::<UpdateState>().set_visible(true);
    if FOUND.with_borrow(Option::is_none) {
        check(app, false);
    }
}

/// SKIP THIS VERSION: not announced again at start (Help > Check for Updates still shows it).
fn skip(app: &App) {
    let s = app.global::<UpdateState>();
    let version = s.get_version().to_string();
    state::update(|st| st.skipped = version);
    s.set_version("".into());
    s.set_visible(false);
    FOUND.set(None);
}

/// A clean exit shows that the version just installed starts.
pub fn quit() {
    state::update(|s| s.confirm(VERSION));
}

pub fn wire(app: &App) {
    let s = app.global::<UpdateState>();
    s.set_current(VERSION.into());
    let w = app.as_weak();
    s.on_open(move || {
        if let Some(app) = w.upgrade() {
            open(&app);
        }
    });
    let w = app.as_weak();
    s.on_update_now(move || {
        if let Some(app) = w.upgrade() {
            update_now(&app);
        }
    });
    let w = app.as_weak();
    s.on_skip(move || {
        if let Some(app) = w.upgrade() {
            skip(&app);
        }
    });
    s.on_cancel(|| CANCEL.store(true, Ordering::Relaxed));
    let w = app.as_weak();
    s.on_roll_back(move || {
        if let Some(app) = w.upgrade() {
            roll_back(&app);
        }
    });

    // The version just installed: counted as started; ROLL BACK is offered while its predecessor is kept.
    let (failed, from) = state::update(|s| {
        s.started(VERSION);
        (
            s.failed_start(VERSION),
            s.applied.as_ref().map(|a| a.from.clone()),
        )
    })
    .unwrap_or_default();
    if let Some(from) = from.filter(|_| install::locate().is_ok_and(|i| i.can_roll_back())) {
        s.set_rollback(from.into());
        s.set_failed_start(failed);
        if failed {
            toast::show(
                app,
                tr!("The last start did not finish: Help > Roll Back goes to the version before"),
            );
        }
    }
    let w = app.as_weak();
    CONFIRM.with(|t| {
        t.start(TimerMode::SingleShot, CONFIRM_AFTER, move || {
            state::update(|s| s.confirm(VERSION));
            if let Some(app) = w.upgrade() {
                app.global::<UpdateState>().set_failed_start(false);
            }
        })
    });

    let due = state::file().is_some_and(|f| state::read(&f).due(state::now()));
    if due && prefs::with(|p| p.advanced.check_updates) {
        check(app, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, version: &str) -> Entry {
        Entry {
            dir: format!("repo/{name}/"),
            manifest: serde_json::from_str(&format!(
                r#"{{"name":"{name}","version":"{version}"}}"#
            ))
            .unwrap(),
            descriptions: Vec::new(),
        }
    }

    #[test]
    fn only_installed_plugins_in_another_version_count_as_updates() {
        let repo = [entry("old", "2"), entry("same", "1"), entry("new", "1")];
        let installed = |n: &str| match n {
            "old" => Some("1".to_owned()),
            "same" => Some("1".to_owned()),
            _ => None,
        };
        assert_eq!(
            outdated(&repo, &installed),
            ["old"],
            "a plugin that is not installed is no update"
        );
    }
}
