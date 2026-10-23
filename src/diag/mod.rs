//! Diagnostics: the log file, crash reports and the problem report (docs/diagnostics.md).
//!
//! [`init`] (first thing in `main`) opens `logs/openfader.log` in the data folder and redirects stdout and stderr through it
//! ([`log`]: everything the app prints also lands there, timestamped, rotated by size), and installs the panic hook ([`crash`]: a
//! `crash-<timestamp>.md` with the [`report`] of the dying run). The app tells what only it knows with [`set`] (renderer, audio
//! device, controllers), [`set_plugins`] and [`set_settings`]; [`report`] builds the report of the running session and
//! [`pending_crash`] finds the one an earlier run left behind. [`finish`] ends it all at a normal exit.

mod crash;
mod log;
mod redact;
mod report;
mod sys;

pub use crash::{mark_seen, Pending};
pub use redact::{redact, settings as settings_diff};
pub use report::{block, issue_url, ISSUE_URL_MAX};

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The log file is rotated at this size; this many older ones are kept.
const LOG_CAP: u64 = 1 << 20;
const LOG_KEEP: usize = 3;
/// Waiting for a lock the dying thread may hold itself.
const LOCK_WAIT: Duration = Duration::from_millis(200);

static LOG: Mutex<Option<log::Log>> = Mutex::new(None);
static TEES: Mutex<Vec<log::Tee>> = Mutex::new(Vec::new());
static FACTS: Mutex<Facts> = Mutex::new(Facts::new());
static SYS: OnceLock<sys::Sys> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

struct Facts {
    env: Vec<(String, String)>,
    plugins: String,
    settings: String,
}

impl Facts {
    const fn new() -> Facts {
        Facts {
            env: Vec::new(),
            plugins: String::new(),
            settings: String::new(),
        }
    }
}

/// `logs` in the data folder: the log file and the crash reports.
pub fn dir() -> Option<PathBuf> {
    Some(crate::paths::data_dir()?.join("logs"))
}

/// Starts the log and the panic hook. Call once, before the app starts threads of its own that print.
pub fn init() {
    if START.set(Instant::now()).is_err() {
        return;
    }
    std::thread::spawn(|| {
        let _ = SYS.set(sys::read());
    });
    let Some(dir) = dir() else { return };
    crash::prune(&dir);
    match log::Log::open(&dir, LOG_CAP, LOG_KEEP) {
        Ok(mut l) => {
            l.line(
                "diag",
                &format!(
                    "OpenFadeR {} ({}) starts",
                    env!("CARGO_PKG_VERSION"),
                    option_env!("OPENFADER_COMMIT").unwrap_or("unknown")
                ),
            );
            *LOG.lock() = Some(l);
            let mut tees = TEES.lock();
            for err in [false, true] {
                match log::tee(err, &LOG) {
                    Ok(t) => tees.push(t),
                    Err(e) => eprintln!("log file: cannot redirect the output: {e}"),
                }
            }
        }
        Err(e) => eprintln!("log file {}: {e}", dir.display()),
    }
    crash::install();
}

/// A normal end: panics of this run that did not stop it are marked seen, the output goes back to where it came from and the
/// log is complete. Call before the process ends or replaces itself.
pub fn finish() {
    crash::survived();
    for t in TEES.lock().drain(..) {
        t.stop();
    }
    if let Some(l) = LOG.lock().as_mut() {
        l.flush();
    }
}

/// Tells the report one thing about the session (`key` is a bare word: `renderer`, `audio_device`…). The order of the first
/// calls is the order in the report.
pub fn set(key: &str, value: impl ToString) {
    let value = value.to_string();
    let mut f = FACTS.lock();
    match f.env.iter_mut().find(|(k, _)| k == key) {
        Some((_, v)) => *v = value,
        None => f.env.push((key.to_owned(), value)),
    }
}

/// The plugins, one `name: version (on|off)` per line.
pub fn set_plugins(text: String) {
    FACTS.lock().plugins = text;
}

/// The settings that differ from the defaults ([`settings_diff`]).
pub fn set_settings(text: String) {
    FACTS.lock().settings = text;
}

/// The report of the running session (`panic` None) or of a dying one.
fn snapshot(panic: Option<&report::Panic>) -> String {
    let facts = FACTS.try_lock_for(LOCK_WAIT);
    let log = LOG.try_lock_for(LOCK_WAIT);
    let empty = Facts::new();
    let f = facts.as_deref().unwrap_or(&empty);
    let tail = log
        .as_deref()
        .and_then(Option::as_ref)
        .map(log::Log::tail)
        .unwrap_or_default();
    let sys = SYS.get().cloned().unwrap_or_default();
    report::build(&report::Input {
        panic,
        sys: &sys,
        facts: &f.env,
        plugins: &f.plugins,
        settings: &f.settings,
        log: &tail,
        uptime_secs: START.get().map_or(0, |s| s.elapsed().as_secs()),
    })
}

/// The problem report of this session: environment, plugins, settings and the last log lines.
pub fn report() -> String {
    if let Some(l) = LOG.lock().as_mut() {
        l.flush();
    }
    snapshot(None)
}

/// The crash report an earlier run left, if nobody has looked at it.
pub fn pending_crash() -> Option<Pending> {
    crash::pending(&dir()?)
}
