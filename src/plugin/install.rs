//! Installing and removing plugins. A plugin folder, a `.zip` of one (the folder's files at the archive root, or one top
//! folder holding them) or the `plugin.json` of a folder is copied into the user's plugin folder
//! ([`user_dir`]: `<config dir>/extension/plugin/<name>/`), replacing an older copy of the same plugin. The copy is
//! checked as [`super::discover`] would read it before it replaces anything. Only plugins in that folder can be removed;
//! bundled ones can be switched off ([`super::config`]).

use super::{Plugin, read_manifest};
use crate::zip::Zip;
use std::path::{Component, Path, PathBuf};

/// Folders and files an install leaves out (build and editor leftovers).
const SKIP: [&str; 4] = [".git", "target", "__pycache__", ".DS_Store"];

/// `<config dir>/extension/plugin`, where plugins are installed.
pub fn user_dir() -> Option<PathBuf> {
    crate::paths::config_dir().map(|d| d.join("extension").join("plugin"))
}

/// Whether `path` looks like a plugin to install (a folder with or around a `plugin.json`, that file, or a `.zip`
/// holding one).
pub fn is_plugin(path: &Path) -> bool {
    if path.is_dir() {
        return source_dir(path).is_ok();
    }
    if path.file_name().is_some_and(|n| n == "plugin.json") {
        return true;
    }
    is_zip(path) && Zip::open(path).is_ok_and(|z| zip_prefix(&z).is_ok())
}

fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Installs the plugin at `from` into [`user_dir`]; returns its name.
pub fn install(from: &Path) -> Result<String, String> {
    install_into(from, &user_dir().ok_or("no config folder (set HOME)")?)
}

/// [`install`] into `root`.
pub fn install_into(from: &Path, root: &Path) -> Result<String, String> {
    crate::paths::at(std::fs::create_dir_all(root), root)?;
    let staging = root.join(format!(".installing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = stage(from, &staging).and_then(|()| {
        let m = read_manifest(&staging)?;
        let dest = root.join(folder_name(&m.name));
        if dest.exists() {
            crate::paths::at(std::fs::remove_dir_all(&dest), &dest)?;
        }
        crate::paths::at(std::fs::rename(&staging, &dest), &dest)?;
        Ok(m.name)
    });
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// A plugin name as a folder name: path separators and other troublesome characters become `_`.
fn folder_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || "-_. ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Copies the plugin's files into `to`.
fn stage(from: &Path, to: &Path) -> Result<(), String> {
    if from.is_dir() {
        return copy_tree(&source_dir(from)?, to);
    }
    if from.file_name().is_some_and(|n| n == "plugin.json") {
        return copy_tree(from.parent().ok_or("plugin.json has no folder")?, to);
    }
    if is_zip(from) {
        return unzip(from, to);
    }
    Err(format!(
        "{}: not a plugin (a folder with a plugin.json, that file, or a .zip of the folder)",
        from.display()
    ))
}

/// The folder holding `plugin.json`: `dir` itself or its only sub-folder that has one.
fn source_dir(dir: &Path) -> Result<PathBuf, String> {
    if dir.join("plugin.json").is_file() {
        return Ok(dir.to_owned());
    }
    let inner: Vec<PathBuf> = crate::paths::at(std::fs::read_dir(dir), dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("plugin.json").is_file())
        .collect();
    match &inner[..] {
        [one] => Ok(one.clone()),
        [] => Err(format!("{}: no plugin.json", dir.display())),
        _ => Err(format!(
            "{}: holds several plugins; drop them one by one",
            dir.display()
        )),
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    crate::paths::at(std::fs::create_dir_all(to), to)?;
    for e in crate::paths::at(std::fs::read_dir(from), from)?.flatten() {
        let name = e.file_name();
        if SKIP.iter().any(|s| name == *s) {
            continue;
        }
        let (src, dst) = (e.path(), to.join(&name));
        let kind = crate::paths::at(e.file_type(), &src)?;
        if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else if kind.is_file() {
            crate::paths::at(std::fs::copy(&src, &dst), &src)?;
        }
    }
    Ok(())
}

/// The entry prefix the plugin's files sit under in `z`: `""` (plugin.json at the root) or `"top/"` (one top folder).
fn zip_prefix(z: &Zip) -> Result<String, String> {
    if z.has("plugin.json") {
        return Ok(String::new());
    }
    let tops: Vec<&str> = z
        .names()
        .filter_map(|n| {
            n.strip_suffix("plugin.json")
                .filter(|p| p.ends_with('/') && p.matches('/').count() == 1)
        })
        .collect();
    match &tops[..] {
        [one] => Ok(one.to_string()),
        [] => Err("the zip holds no plugin.json".into()),
        _ => Err("the zip holds several plugins".into()),
    }
}

fn unzip(from: &Path, to: &Path) -> Result<(), String> {
    let mut z = Zip::open(from).map_err(|e| format!("{}: {e:?}", from.display()))?;
    let prefix = zip_prefix(&z)?;
    let names: Vec<String> = z
        .names()
        .filter(|n| n.starts_with(&prefix))
        .map(str::to_owned)
        .collect();
    for name in names {
        let rel = Path::new(&name[prefix.len()..]);
        // No absolute paths or `..`: every entry lands inside the plugin folder.
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(format!(
                "{}: entry {name:?} leaves the plugin folder",
                from.display()
            ));
        }
        if name.ends_with('/')
            || rel.as_os_str().is_empty()
            || rel.iter().any(|c| SKIP.iter().any(|s| c == *s))
        {
            continue;
        }
        let dst = to.join(rel);
        if let Some(dir) = dst.parent() {
            crate::paths::at(std::fs::create_dir_all(dir), dir)?;
        }
        let bytes = z
            .read(&name)
            .map_err(|e| format!("{}: {e:?}", from.display()))?;
        crate::paths::at(std::fs::write(&dst, bytes), &dst)?;
    }
    Ok(())
}

/// Whether `p` was installed by the user (it lives in [`user_dir`]) and so can be removed.
pub fn removable(p: &Plugin) -> bool {
    user_dir().is_some_and(|u| p.dir.starts_with(u))
}

/// Deletes an installed plugin's folder.
pub fn uninstall(p: &Plugin) -> Result<(), String> {
    if !removable(p) {
        return Err(format!(
            "{} is bundled with the app: switch it off instead",
            p.manifest.name
        ));
    }
    crate::paths::at(std::fs::remove_dir_all(&p.dir), &p.dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rk-install-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const JSON: &str = r#"{"name": "hello", "version": "1", "script": "main.lua"}"#;

    #[test]
    fn a_folder_a_zip_or_its_plugin_json_installs_and_replaces_an_older_copy() {
        let t = temp("ok");
        let src = t.join("download/hello-plugin");
        fs::create_dir_all(src.join("lib")).unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join("plugin.json"), JSON).unwrap();
        fs::write(src.join("main.lua"), "function handle() end").unwrap();
        fs::write(src.join("lib/util.lua"), "return {}").unwrap();
        let root = t.join("plugins");
        // The folder around the plugin folder works too.
        assert_eq!(install_into(&t.join("download"), &root).unwrap(), "hello");
        assert!(root.join("hello/lib/util.lua").is_file() && !root.join("hello/.git").exists());
        fs::write(src.join("plugin.json"), JSON.replace("\"1\"", "\"2\"")).unwrap();
        fs::remove_file(src.join("lib/util.lua")).unwrap();
        assert_eq!(
            install_into(&src.join("plugin.json"), &root).unwrap(),
            "hello"
        );
        assert!(
            fs::read_to_string(root.join("hello/plugin.json"))
                .unwrap()
                .contains("\"2\"")
        );
        assert!(
            !root.join("hello/lib/util.lua").exists(),
            "the old copy is replaced, not merged"
        );
        let zip = t.join("hello.zip");
        fs::write(
            &zip,
            crate::zip::write(&[
                ("hello/plugin.json", JSON.into()),
                ("hello/main.lua", b"x".to_vec()),
            ]),
        )
        .unwrap();
        assert!(is_plugin(&zip) && is_plugin(&src) && !is_plugin(&t));
        assert_eq!(install_into(&zip, &root).unwrap(), "hello");
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            1,
            "no staging folder is left"
        );
        fs::remove_dir_all(t).ok();
    }

    #[test]
    fn a_broken_plugin_or_an_escaping_zip_changes_nothing() {
        let t = temp("bad");
        let root = t.join("plugins");
        let zip = t.join("evil.zip");
        fs::write(
            &zip,
            crate::zip::write(&[("plugin.json", JSON.into()), ("../../x.lua", b"x".to_vec())]),
        )
        .unwrap();
        assert!(
            install_into(&zip, &root)
                .unwrap_err()
                .contains("leaves the plugin folder")
        );
        let src = t.join("p");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("plugin.json"), JSON).unwrap(); // main.lua is missing
        assert!(install_into(&src, &root).unwrap_err().contains("main.lua"));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(t).ok();
    }
}
