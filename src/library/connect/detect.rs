//! Libraries of other apps found in their default places: each enabled library plugin's manifest lists glob patterns per OS
//! (`detect`, see [`crate::plugin::library::Detect`]); the ones that match a file or folder and are not connected yet are
//! offered in Preferences > Libraries.

use crate::store::{Store, R};
use std::path::PathBuf;

/// A library that exists in a plugin's default place and is not connected.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub plugin: String,
    pub label: String,
    pub path: PathBuf,
}

/// `name` against `pat`, where `*` stands for any run of characters (within one path part).
fn wild(pat: &[u8], name: &[u8]) -> bool {
    match pat.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| wild(rest, &name[i..])),
        Some((c, rest)) => name.first() == Some(c) && wild(rest, &name[1..]),
    }
}

/// The existing paths `pattern` names: `~`, `$HOME`, `%USERPROFILE%`, `%APPDATA%` and `%LOCALAPPDATA%` expand, `/` and `\`
/// both separate, and a part with `*` matches the entries of its folder.
pub fn expand(pattern: &str) -> Vec<PathBuf> {
    let home = crate::paths::home().map(|h| h.to_string_lossy().into_owned());
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .or_else(|| home.clone().filter(|_| name == "HOME"))
    };
    let mut p = pattern.replace('\\', "/");
    if let Some(rest) = p.strip_prefix("~/") {
        p = format!("$HOME/{rest}");
    }
    for (token, name) in [
        ("$HOME", "HOME"),
        ("%USERPROFILE%", "USERPROFILE"),
        ("%APPDATA%", "APPDATA"),
        ("%LOCALAPPDATA%", "LOCALAPPDATA"),
    ] {
        if p.contains(token) {
            let Some(v) = var(name) else {
                return Vec::new();
            };
            p = p.replace(token, &v.replace('\\', "/"));
        }
    }
    let mut found = vec![PathBuf::new()];
    for (i, part) in p.split('/').enumerate() {
        if i == 0 && (part.is_empty() || part.ends_with(':')) {
            // `/` or a drive (`C:`): its root, never the drive-relative `C:Users`.
            found = vec![PathBuf::from(format!("{part}/"))];
            continue;
        }
        if part.is_empty() {
            continue;
        }
        found = if part.contains('*') {
            found
                .iter()
                .flat_map(|dir| {
                    let base = if dir.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        dir.clone()
                    };
                    let mut names: Vec<String> = std::fs::read_dir(base)
                        .into_iter()
                        .flatten()
                        .filter_map(|e| e.ok()?.file_name().into_string().ok())
                        .filter(|n| wild(part.as_bytes(), n.as_bytes()))
                        .collect();
                    names.sort();
                    names.into_iter().map(|n| dir.join(n)).collect::<Vec<_>>()
                })
                .collect()
        } else {
            found.iter().map(|d| d.join(part)).collect()
        };
    }
    found.retain(|p| p.exists());
    found
}

/// The libraries the enabled plugins' `detect` patterns find, minus the connected ones.
pub fn detect(store: &Store) -> R<Vec<Found>> {
    let connected: Vec<PathBuf> = store.connections()?.into_iter().map(|c| c.path).collect();
    let mut out: Vec<Found> = Vec::new();
    for d in crate::plugin::library::detectors() {
        for path in d.patterns.iter().flat_map(|p| expand(p)) {
            if connected.contains(&path) || out.iter().any(|f| f.path == path) {
                continue;
            }
            let label = match crate::plugin::library::claim(&path) {
                Some((plugin, kind)) if plugin == d.plugin => kind,
                _ => d.plugin.clone(),
            };
            out.push(Found {
                plugin: d.plugin.clone(),
                label,
                path,
            });
        }
    }
    Ok(out)
}
