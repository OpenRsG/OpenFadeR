//! What the last import of a collection did, kept in `<data>/last-import-report.json` so File > Library > Last Import Report…
//! can show it again after a restart. Built from the [`Report`] the import returns, nothing else: the lists are what the import
//! really skipped.

use super::import::{Reason, Report, Skipped};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// Local time the import ended, `YYYY-MM-DD HH:MM`.
    pub when: String,
    /// What was read: the library file's path, or the name of the source.
    pub source: String,
    pub added: usize,
    pub playlists: usize,
    pub cues: usize,
    pub grids: usize,
    /// Tracks imported whose file is not at its path now.
    pub absent: usize,
    pub skipped: Vec<Skipped>,
}

impl Saved {
    pub fn of(source: &str, r: &Report) -> Self {
        Saved {
            when: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            source: source.to_owned(),
            added: r.added,
            playlists: r.playlists,
            cues: r.cues,
            grids: r.grids,
            absent: r.absent,
            skipped: r.skipped.clone(),
        }
    }

    /// How many entries were skipped for `reason`.
    pub fn count(&self, reason: Reason) -> usize {
        self.skipped.iter().filter(|s| s.reason == reason).count()
    }

    /// Keeps this report as the last one at `path` (written whole, then moved into place).
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        let tmp = path.with_extension("json.tmp");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The report kept at `path`; `None` when there is none or it cannot be read.
    pub fn load_from(path: &Path) -> Option<Saved> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

/// Where the last report lives.
pub fn file() -> Option<PathBuf> {
    Some(crate::paths::data_dir()?.join("last-import-report.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_report_survives_a_restart_and_a_missing_or_broken_file_is_no_report() {
        let dir =
            std::env::temp_dir().join(format!("openfader-last-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("last-import-report.json");
        assert_eq!(Saved::load_from(&path), None);
        let skip = |name: &str, reason| Skipped {
            name: name.into(),
            reason,
        };
        let r = Report {
            added: 3,
            playlists: 2,
            cues: 7,
            grids: 3,
            absent: 1,
            skipped: vec![
                skip("/a/gone.mp3", Reason::MissingFile),
                skip("/a/x.ogg", Reason::UnsupportedFormat),
                skip("/a/dup.mp3", Reason::Duplicate),
                skip("/a/dup2.mp3", Reason::Duplicate),
            ],
            ..Report::default()
        };
        let saved = Saved::of("/lib/collection.xml", &r);
        saved.save_to(&path).unwrap();
        assert_eq!(Saved::load_from(&path), Some(saved.clone()));
        assert_eq!(
            [
                Reason::MissingFile,
                Reason::UnsupportedFormat,
                Reason::Duplicate
            ]
            .map(|x| saved.count(x)),
            [1, 1, 2]
        );
        // A newer report replaces it.
        let second = Saved::of("b", &Report::default());
        second.save_to(&path).unwrap();
        assert_eq!(Saved::load_from(&path), Some(second));
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Saved::load_from(&path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
