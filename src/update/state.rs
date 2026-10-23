//! What the updater remembers between runs, in `<config>/update.json`: when the last check ran (one a day at most), the
//! version the user skipped, and the version just installed with whether it has started properly yet (the roll-back entry).

use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Seconds between two automatic checks.
pub const DAY: u64 = 24 * 60 * 60;

/// An update this program installed: `from` is what `<install>.prev` holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub from: String,
    pub to: String,
    /// The new version ran long enough (or quit cleanly): it starts.
    pub confirmed: bool,
    /// How many times the new version has been started.
    pub starts: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Unix seconds of the last automatic check that reached GitHub.
    pub checked: u64,
    /// The release tag's version the user chose to skip.
    pub skipped: String,
    pub applied: Option<Applied>,
}

impl State {
    /// An automatic check is due: none for a day (or the clock moved back).
    pub fn due(&self, now: u64) -> bool {
        self.checked == 0 || now.saturating_sub(self.checked) >= DAY || now < self.checked
    }

    /// The new version was started and has not shown that it works: it crashed or was killed before [`State::confirm`].
    pub fn failed_start(&self, running: &str) -> bool {
        self.applied
            .as_ref()
            .is_some_and(|a| a.to == running && !a.confirmed && a.starts >= 2)
    }

    /// This version starts: counts it when it is the one just installed, forgets the record when another is running
    /// (the user installed by hand, or rolled back).
    pub fn started(&mut self, running: &str) {
        let Some(a) = &mut self.applied else { return };
        if a.to == running {
            a.starts += 1;
        } else if a.from != running {
            self.applied = None;
        }
    }

    /// The running version works.
    pub fn confirm(&mut self, running: &str) {
        if let Some(a) = self.applied.as_mut().filter(|a| a.to == running) {
            a.confirmed = true;
        }
    }
}

pub fn file() -> Option<PathBuf> {
    Some(paths::config_dir()?.join("update.json"))
}

/// The saved state (the default when there is none or it is unreadable).
pub fn read(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write(path: &Path, s: &State) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        paths::at(std::fs::create_dir_all(dir), dir)?;
    }
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    paths::at(std::fs::write(&tmp, text), &tmp)?;
    paths::at(std::fs::rename(&tmp, path), path)
}

/// Reads the state, lets `f` change it, saves it when it changed (a program that never recorded anything writes no file);
/// returns what `f` returned.
pub fn update<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    let path = file()?;
    let mut s = read(&path);
    let before = s.clone();
    let out = f(&mut s);
    if s != before {
        write(&path, &s).ok()?;
    }
    Some(out)
}

/// Unix seconds now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_is_due_once_a_day() {
        let s = State {
            checked: 1_000_000,
            ..Default::default()
        };
        assert!(!s.due(1_000_000 + DAY - 1));
        assert!(s.due(1_000_000 + DAY));
        assert!(
            s.due(999_000),
            "a clock set back never blocks checks for good"
        );
        assert!(State::default().due(5));
    }

    #[test]
    fn a_new_version_that_never_confirms_is_a_failed_start() {
        let mut s = State {
            applied: Some(Applied {
                from: "0.2.5".into(),
                to: "0.3.0".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        s.started("0.3.0");
        assert!(!s.failed_start("0.3.0"), "the first start is still running");
        s.started("0.3.0");
        assert!(s.failed_start("0.3.0"), "started twice, never confirmed");
        assert!(!s.failed_start("0.2.5"));
        s.confirm("0.3.0");
        assert!(!s.failed_start("0.3.0"));
        s.started("0.2.5"); // rolled back by hand: the old version runs, the record is stale
        assert_eq!(s.applied.as_ref().map(|a| a.from.as_str()), Some("0.2.5"));
        s.started("0.1.0");
        assert_eq!(s.applied, None);
    }

    #[test]
    fn the_state_survives_a_restart_and_a_bad_file_is_the_default() {
        let dir =
            std::env::temp_dir().join(format!("openfader-update-state-{}", std::process::id()));
        let f = dir.join("update.json");
        let s = State {
            checked: 7,
            skipped: "0.9.0".into(),
            applied: None,
        };
        write(&f, &s).unwrap();
        assert_eq!(read(&f), s);
        std::fs::write(&f, "{ nope").unwrap();
        assert_eq!(read(&f), State::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
