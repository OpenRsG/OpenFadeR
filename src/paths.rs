//! Where OpenFadeR writes user-visible output files (recordings, XML exports), and the path helpers every section shares.

use std::io;
use std::path::{Path, PathBuf};

/// The user's home folder: `$HOME`, else `%USERPROFILE%` (Windows); empty values count as unset.
pub fn home() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

fn dir_from(env_var: &str, get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let get = |k: &str| get(k).filter(|v| !v.is_empty());
    get(env_var)
        .or_else(|| get("XDG_MUSIC_DIR"))
        .map(PathBuf::from)
        .or_else(|| {
            get("HOME")
                .or_else(|| get("USERPROFILE"))
                .map(|h| Path::new(&h).join("Music").join("OpenFadeR"))
        })
}

/// Output directory: `$env_var`, else `$XDG_MUSIC_DIR`, else `~/Music/OpenFadeR` (empty values count as unset).
pub fn output_dir(env_var: &str) -> Option<PathBuf> {
    dir_from(env_var, |k| std::env::var(k).ok())
}

/// Adds the path to an IO or SQL error: `path: message`.
pub fn at<T, E: std::fmt::Display>(r: Result<T, E>, p: &Path) -> Result<T, String> {
    r.map_err(|e| format!("{}: {e}", p.display()))
}

/// `rel` as a device library stores a path (leading `/` allowed) under the device root `root`.
pub fn on_device(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.trim_start_matches('/'))
}

/// A file-name safe form of a title, at most `max` characters: letters, digits, space, `_`, `(` and `)` are kept, the
/// rest becomes `_` (so no `-`: the name parts are joined with it, see `groove::source::bpm_from_name`); empty → `track`.
pub fn file_stem(title: &str, max: usize) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " _()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s: String = s.trim().chars().take(max).collect();
    if s.is_empty() {
        "track".into()
    } else {
        s
    }
}

/// Whether `path` ends in one of `exts` (ASCII case-insensitive, no dot).
pub fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Capture folder: `setting` if non-blank, else `$env_var`, else `<output_dir(env_var)>/<sub>`.
pub fn capture_dir(setting: &str, env_var: &str, sub: &str) -> Option<PathBuf> {
    let setting = setting.trim();
    if !setting.is_empty() {
        return Some(setting.into());
    }
    env_path(env_var).or_else(|| output_dir(env_var).map(|b| b.join(sub)))
}

/// Which of the per-user folders [`base_from`] means.
#[derive(Clone, Copy, PartialEq)]
pub enum Base {
    /// Settings and user files.
    Config,
    /// Our own persistent data (the store, downloaded plugins).
    Data,
    /// What can be rebuilt (analysis, stems, models).
    Cache,
}

/// `<per-user folder>/openfader` of `base`, by each OS's convention: `$XDG_*_HOME` where set (on any OS: it is also how tests and
/// portable setups point elsewhere), else Linux `~/.config`, `~/.local/share`, `~/.cache`; macOS `~/Library/Application Support`
/// (config and data) and `~/Library/Caches`; Windows `%APPDATA%` (config), `%LOCALAPPDATA%` (data) and
/// `%LOCALAPPDATA%\openfader\cache`. `None` when the variables it needs are unset; `get` reads them (empty counts as unset).
pub fn base_from(base: Base, get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let var = |k: &str| get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let (xdg, linux, windows, mac) = match base {
        Base::Config => (
            "XDG_CONFIG_HOME",
            ".config",
            "APPDATA",
            "Library/Application Support",
        ),
        Base::Data => (
            "XDG_DATA_HOME",
            ".local/share",
            "LOCALAPPDATA",
            "Library/Application Support",
        ),
        Base::Cache => ("XDG_CACHE_HOME", ".cache", "LOCALAPPDATA", "Library/Caches"),
    };
    if let Some(x) = var(xdg) {
        return Some(x.join("openfader"));
    }
    if cfg!(windows) {
        let d = var(windows)?.join("openfader");
        return Some(if base == Base::Cache {
            d.join("cache")
        } else {
            d
        });
    }
    let sub = if cfg!(target_os = "macos") {
        mac
    } else {
        linux
    };
    Some(var("HOME")?.join(sub).join("openfader"))
}

fn base(b: Base) -> Option<PathBuf> {
    base_from(b, |k| std::env::var(k).ok())
}

/// Where our own persistent store lives (`store.db`, see [`crate::store`]); see [`base_from`].
pub fn data_dir() -> Option<PathBuf> {
    base(Base::Data)
}

/// Settings and user files (lighting fixtures); see [`base_from`].
pub fn config_dir() -> Option<PathBuf> {
    base(Base::Config)
}

/// The parent of every cache folder (analysis, stems, models); see [`base_from`].
pub fn cache_dir() -> Option<PathBuf> {
    base(Base::Cache)
}

/// Folders to look for user add-ons in: each entry of `$env_var` (a path list: `:`-separated, `;` on Windows), then
/// `<config_dir>/<sub>`, then the bundled `<sub>` next to the executable, in a `Resources` folder beside it (a macOS app
/// bundle's `Contents/Resources`), or in a folder above it (so `target/release/openfader` finds the repository's).
/// Used by plugins (`extension/plugin`), skins (`extension/skin`) and mappings; folders that do not exist are kept
/// (callers skip them).
pub fn search_path(env_var: &str, sub: &str) -> Vec<PathBuf> {
    let env = std::env::var_os(env_var).unwrap_or_default();
    let exe = std::env::current_exe().ok();
    let bundled = exe
        .iter()
        .flat_map(|e| e.ancestors().skip(1))
        .flat_map(|d| [d.join(sub), d.join("Resources").join(sub)])
        .find(|d| d.is_dir());
    let mut dirs: Vec<PathBuf> = Vec::new();
    let all = std::env::split_paths(&env)
        .filter(|p| !p.as_os_str().is_empty())
        .chain(config_dir().map(|c| c.join(sub)))
        .chain(bundled);
    for d in all {
        // The same folder twice (an env entry naming the bundled one) would report every plugin in it as a duplicate.
        let same = |e: &PathBuf| {
            e == &d || (e.canonicalize().ok()).is_some_and(|c| Some(c) == d.canonicalize().ok())
        };
        if !dirs.iter().any(same) {
            dirs.push(d);
        }
    }
    dirs
}

/// `<dir>/<prefix>-YYYYmmdd-HHMMSS.<ext>` (local time) with `dir` created; if that name exists a `-2`, `-3`…
/// suffix is added, so an existing file is never returned.
// ponytail: exists-then-create race with another process; callers open with `create_new` anyway.
pub fn timestamped(dir: &Path, prefix: &str, ext: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut p = dir.join(format!("{prefix}-{stamp}.{ext}"));
    for n in 2.. {
        if !p.exists() {
            break;
        }
        p = dir.join(format!("{prefix}-{stamp}-{n}.{ext}"));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_precedence() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_string())
        };
        let all = env(&[("X_DIR", "/r"), ("XDG_MUSIC_DIR", "/m"), ("HOME", "/h")]);
        assert_eq!(dir_from("X_DIR", all), Some(PathBuf::from("/r")));
        assert_eq!(
            dir_from("X_DIR", env(&[("XDG_MUSIC_DIR", "/m"), ("HOME", "/h")])),
            Some(PathBuf::from("/m"))
        );
        assert_eq!(
            dir_from(
                "X_DIR",
                env(&[("X_DIR", ""), ("XDG_MUSIC_DIR", ""), ("HOME", "/h")])
            ),
            Some(PathBuf::from("/h/Music/OpenFadeR"))
        );
        assert_eq!(dir_from("X_DIR", env(&[])), None);
    }

    #[test]
    fn per_user_folders_follow_each_os() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_string())
        };
        let all = &[
            ("HOME", "/h"),
            ("APPDATA", "/a"),
            ("LOCALAPPDATA", "/l"),
            ("XDG_CACHE_HOME", "/x"),
        ];
        assert_eq!(
            base_from(Base::Cache, env(all)),
            Some(PathBuf::from("/x/openfader")),
            "XDG wins on every OS"
        );
        let got =
            [Base::Config, Base::Data, Base::Cache].map(|b| base_from(b, env(&all[..3])).unwrap());
        let want: [&str; 3] = if cfg!(windows) {
            ["/a/openfader", "/l/openfader", "/l/openfader/cache"]
        } else if cfg!(target_os = "macos") {
            [
                "/h/Library/Application Support/openfader",
                "/h/Library/Application Support/openfader",
                "/h/Library/Caches/openfader",
            ]
        } else {
            [
                "/h/.config/openfader",
                "/h/.local/share/openfader",
                "/h/.cache/openfader",
            ]
        };
        assert_eq!(got, want.map(PathBuf::from));
        assert_eq!(base_from(Base::Data, env(&[])), None);
    }

    #[test]
    fn timestamped_creates_dir_and_never_returns_an_existing_file() {
        let dir = std::env::temp_dir()
            .join(format!("openfader-paths-{}", std::process::id()))
            .join("new");
        let a = timestamped(&dir, "t", "txt").unwrap();
        assert!(dir.is_dir() && a.extension().unwrap() == "txt");
        std::fs::write(&a, b"x").unwrap();
        let b = timestamped(&dir, "t", "txt").unwrap();
        assert_ne!(a, b);
        std::fs::write(&b, b"y").unwrap();
        let c = timestamped(&dir, "t", "txt").unwrap();
        assert!(c != a && c != b);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn titles_become_safe_file_names() {
        assert_eq!(file_stem("A/B: c*d", 80), "A_B_ c_d");
        assert_eq!(
            file_stem("AC/DC – Back in Black?", 40),
            "AC_DC _ Back in Black_"
        );
        assert_eq!(file_stem("  ", 40), "track");
        assert_eq!(file_stem(&"x".repeat(200), 80).len(), 80);
    }
}
