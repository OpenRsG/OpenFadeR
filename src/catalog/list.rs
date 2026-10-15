//! The user's repositories: `<config>/repositories.json`, a plain list of `owner/repo`. A first run has no file and gets the
//! [`DEFAULTS`]; the file is written by the first add or remove, so removing a default stays removed.

use super::Repo;
use std::path::PathBuf;

/// Present on first run (removable).
pub const DEFAULTS: [&str; 3] = [
    "OpenRsG/OpenFadeR-Plugins",
    "OpenRsG/OpenFadeR-Themes",
    "OpenRsG/OpenFadeR-Map",
];

fn file() -> Result<PathBuf, String> {
    crate::paths::config_dir()
        .map(|d| d.join("repositories.json"))
        .ok_or_else(|| "no config folder (set HOME)".into())
}

fn defaults() -> Vec<Repo> {
    DEFAULTS
        .iter()
        .filter_map(|s| Repo::parse(s).ok())
        .collect()
}

/// The saved repositories (the defaults without a file; entries that are no `owner/repo` are dropped).
pub fn load() -> Vec<Repo> {
    let Some(text) = file().ok().and_then(|f| std::fs::read_to_string(f).ok()) else {
        return defaults();
    };
    match serde_json::from_str::<Vec<String>>(&text) {
        Ok(list) => list.iter().filter_map(|s| Repo::parse(s).ok()).collect(),
        Err(e) => {
            eprintln!("catalog: repositories.json: {e}; using the defaults");
            defaults()
        }
    }
}

fn save(repos: &[Repo]) -> Result<(), String> {
    let f = file()?;
    if let Some(dir) = f.parent() {
        crate::paths::at(std::fs::create_dir_all(dir), dir)?;
    }
    let names: Vec<String> = repos.iter().map(Repo::to_string).collect();
    crate::paths::at(
        std::fs::write(&f, serde_json::to_string_pretty(&names).unwrap_or_default()),
        &f,
    )
}

/// Adds the repository `text` names (see [`Repo::parse`]) unless the list has it already (GitHub names ignore case);
/// returns the new list.
pub fn add(text: &str) -> Result<Vec<Repo>, String> {
    let repo = Repo::parse(text)?;
    let mut all = load();
    if all
        .iter()
        .any(|r| r.to_string().eq_ignore_ascii_case(&repo.to_string()))
    {
        return Err(format!("{repo} is in the list already"));
    }
    all.push(repo);
    save(&all)?;
    Ok(all)
}

/// Takes `repo` out of the list; returns the new list.
pub fn remove(repo: &Repo) -> Result<Vec<Repo>, String> {
    let mut all = load();
    all.retain(|r| r != repo);
    save(&all)?;
    Ok(all)
}
