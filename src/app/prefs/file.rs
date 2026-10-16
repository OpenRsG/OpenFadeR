//! The settings file: `settings.conf` in the config folder ([`openfader::paths::config_dir`]);
//! atomic writes (temp file + rename); a missing file is all defaults, bad lines are reported on stderr.

use super::Prefs;
use openfader::paths::{base_from, Base};
use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) fn path_from(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    Some(base_from(Base::Config, get)?.join("settings.conf"))
}

/// `settings.conf` in the config folder.
pub fn path() -> Option<PathBuf> {
    path_from(|k| std::env::var(k).ok())
}

pub(super) fn load_from(path: &Path) -> (Prefs, Vec<String>) {
    match std::fs::read_to_string(path) {
        Ok(t) => Prefs::parse(&t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Default::default(),
        Err(e) => (Prefs::default(), vec![format!("cannot read: {e}")]),
    }
}

pub(super) fn save_to(path: &Path, p: &Prefs) -> io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, p.to_text())?;
    std::fs::rename(tmp, path)
}

/// Reads the settings file; a missing file or bad values give defaults (bad values are reported on stderr).
pub fn load() -> Prefs {
    let Some(path) = path() else {
        return Prefs::default();
    };
    let (p, notes) = load_from(&path);
    for n in notes {
        eprintln!("settings {}: {n}", path.display());
    }
    p
}

/// Writes the settings file; a failure is reported on stderr (the settings stay in memory).
pub fn save(p: &Prefs) {
    if let Some(path) = path() {
        if let Err(e) = save_to(&path, p) {
            eprintln!("settings {}: cannot save: {e}", path.display());
        }
    }
}
