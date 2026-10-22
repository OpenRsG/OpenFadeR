//! Plugins of a repository: [`scan`] lists every folder with a `plugin.json` (at most [`super::DEPTH`] folders below the
//! repository's top folder), [`install`] puts one through [`crate::plugin::install`].
//!
//! A plugin that loads a native library (`effects` naming one, `library.lib`) needs the file for this OS. When the repository
//! zip does not hold it, it is the root of the release asset `<plugin>-<os>-<arch>.zip` of the repository's latest release
//! ([`Repo::asset_url`]); with no such asset nothing is installed.

use super::{below_top, Get, Repo};
use crate::plugin::{effect::Decls, install, Manifest};
use crate::zip::Zip;
use std::path::Path;

/// A plugin found in a repository.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The entry prefix of its folder in the repository zip (`top/my-fx/`).
    pub dir: String,
    pub manifest: Manifest,
    /// `(language, description)` from the plugin's `locales/` ([`crate::locale::pick`]).
    pub descriptions: Vec<(String, String)>,
}

/// The biggest `plugin.json` read.
const MAX_MANIFEST: u64 = 1 << 20;

/// The plugins in `z`, by name (the first of a name wins). A `plugin.json` that does not parse, or has no usable name, is left out.
pub fn scan(z: &mut Zip) -> Vec<Entry> {
    let dirs: Vec<String> = z
        .names()
        .filter_map(|n| {
            let (top, rest) = below_top(n)?;
            let dir = rest.strip_suffix("plugin.json")?;
            let shallow =
                dir.is_empty() || dir.ends_with('/') && dir.matches('/').count() <= super::DEPTH;
            shallow.then(|| format!("{top}/{dir}"))
        })
        .collect();
    let mut out: Vec<Entry> = Vec::new();
    for dir in dirs {
        let file = format!("{dir}plugin.json");
        if z.size(&file).is_none_or(|s| s > MAX_MANIFEST) {
            continue;
        }
        let Some(manifest) = z
            .read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
            .filter(|m| !m.name.is_empty() && !m.name.contains(':'))
        else {
            continue;
        };
        if !out.iter().any(|e| e.manifest.name == manifest.name) {
            let descriptions = descriptions(z, &dir, &manifest.description);
            out.push(Entry {
                dir,
                manifest,
                descriptions,
            });
        }
    }
    out.sort_by_key(|e| e.manifest.name.to_lowercase());
    out
}

/// The translations of `description` in the `locales/<lang>.json` files of the plugin at `dir` in `z`.
fn descriptions(z: &mut Zip, dir: &str, description: &str) -> Vec<(String, String)> {
    let prefix = format!("{dir}locales/");
    let files: Vec<String> = z
        .names()
        .filter(|n| {
            n.strip_prefix(&prefix)
                .is_some_and(|f| f.ends_with(".json") && !f.contains('/'))
        })
        .map(str::to_owned)
        .collect();
    let mut out = Vec::new();
    for f in files {
        if z.size(&f).is_none_or(|s| s > MAX_MANIFEST) {
            continue;
        }
        let Some(map) = z.read(&f).ok().and_then(|b| {
            serde_json::from_slice::<std::collections::HashMap<String, String>>(&b).ok()
        }) else {
            continue;
        };
        if let (Some(lang), Some(t)) = (
            f[prefix.len()..].strip_suffix(".json"),
            map.get(description),
        ) {
            out.push((lang.to_owned(), t.clone()));
        }
    }
    out
}

/// The native library files the plugin loads on this OS, as `plugin.json` names them (relative to the plugin folder).
pub fn natives(m: &Manifest) -> Vec<String> {
    let effects = match &m.effects {
        Some(Decls::Library(p)) => Some(p.to_string_lossy().into_owned()),
        _ => None,
    };
    let library = m
        .library
        .as_ref()
        .and_then(|l| l.lib.file())
        .map(str::to_owned);
    effects.into_iter().chain(library).collect()
}

/// Lays the plugin out in `to`: its folder of the repository zip, and the native libraries the zip lacks from the release asset.
fn stage(repo: &Repo, zip: &Path, e: &Entry, work: &Path, to: &Path) -> Result<(), String> {
    let name = &e.manifest.name;
    let mut z = Zip::open(zip).map_err(|e| format!("{}: {e:?}", zip.display()))?;
    z.unpack(&e.dir, to, &[".git"])
        .map_err(|e| format!("{name}: {e}"))?;
    let missing: Vec<String> = natives(&e.manifest)
        .into_iter()
        .filter(|f| !to.join(f).is_file())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let asset = work.join("asset.zip");
    crate::paths::at(std::fs::create_dir_all(work), work)?;
    super::get(&repo.asset_url(name), &asset).map_err(|g| match g {
        Get::NotFound => format!(
            "no prebuilt {name} for {}-{} yet",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        Get::Failed(e) => format!("{name}: downloading the prebuilt library failed ({e})"),
    })?;
    let mut a =
        Zip::open(&asset).map_err(|e| format!("{name}: the prebuilt zip is damaged ({e:?})"))?;
    a.unpack("", to, &[])
        .map_err(|e| format!("{name}: the prebuilt zip: {e}"))?;
    match missing.iter().find(|f| !to.join(f).is_file()) {
        Some(f) => Err(format!("{name}: the prebuilt zip lacks {f}")),
        None => Ok(()),
    }
}

/// Installs plugin `e` of `repo` (its zip is `zip`, see [`super::Found`]) through [`install::install`], which checks it as a
/// manual drop is and replaces an older copy; returns its name. Blocks on the network: run it on a worker thread, one install at
/// a time.
pub fn install(repo: &Repo, zip: &Path, e: &Entry) -> Result<String, String> {
    let work = std::env::temp_dir().join(format!("openfader-get-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let plugin = work.join("plugin");
    let done = stage(repo, zip, e, &work, &plugin).and_then(|()| install::install(&plugin));
    let _ = std::fs::remove_dir_all(&work);
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rk-catalog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn zip_of(dir: &Path, entries: &[(&str, &str)]) -> Zip {
        let list: Vec<(&str, Vec<u8>)> = entries
            .iter()
            .map(|(n, c)| (*n, c.as_bytes().to_vec()))
            .collect();
        let path = dir.join("repo.zip");
        std::fs::write(&path, crate::zip::write(&list)).unwrap();
        Zip::open(&path).unwrap()
    }

    #[test]
    fn a_repository_zip_lists_plugins_and_themes_up_to_two_folders_deep() {
        let t = temp("scan");
        let mut z = zip_of(
            &t,
            &[
                ("repo-main/", ""),
                ("repo-main/README.md", "x"),
                (
                    "repo-main/one/plugin.json",
                    r#"{"name": "One", "version": "2"}"#,
                ),
                ("repo-main/group/two/plugin.json", r#"{"name": "two"}"#),
                ("repo-main/a/b/c/plugin.json", r#"{"name": "too-deep"}"#),
                ("repo-main/bad/plugin.json", "not json"),
                ("repo-main/nameless/plugin.json", "{}"),
                ("repo-main/dup/plugin.json", r#"{"name": "One"}"#),
                ("repo-main/one/theme.json", r#"{"name": "inside a plugin"}"#),
                ("repo-main/dracula.json", r#"{"name": "Dracula"}"#),
                ("repo-main/themes/nord.json", r#"{"name": "Nord"}"#),
                ("repo-main/a/b/c/deep.json", "{}"),
                ("repo-main/notes.txt", "{}"),
                (
                    "repo-main/Pad-Box.xml",
                    "<?xml?><OpenFadeRMapping><info/></OpenFadeRMapping>",
                ),
                ("repo-main/maps/x.xml", "<OpenFadeRMapping/>"),
                ("repo-main/maps/other.xml", "<project/>"),
                ("repo-main/a/b/c/deep.xml", "<OpenFadeRMapping/>"),
            ],
        );
        let found = super::scan(&mut z);
        let names: Vec<&str> = found.iter().map(|e| e.manifest.name.as_str()).collect();
        assert_eq!(
            names,
            ["One", "two"],
            "sorted, depth <= 2, valid, first of a name wins"
        );
        assert_eq!(found[0].dir, "repo-main/one/");
        assert_eq!(found[0].manifest.version, "2");
        let themes: Vec<String> = super::super::themes::scan(&mut z, &found)
            .into_iter()
            .map(|f| f.stem)
            .collect();
        assert_eq!(themes, ["dracula", "nord"]);
        let maps: Vec<String> = super::super::mappings::scan(&mut z)
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(
            maps,
            ["Pad-Box.xml", "x.xml"],
            "only <OpenFadeRMapping> files, two folders deep"
        );
        std::fs::remove_dir_all(t).ok();
    }

    #[test]
    fn natives_are_the_libraries_this_os_loads() {
        let m: Manifest = serde_json::from_str(
            r#"{"name": "fx", "effects": "libfx.so", "library": {"lib": {"linux": "liba.so", "windows": "a.dll", "macos": "liba.dylib"}}}"#,
        )
        .unwrap();
        let file = m.library.as_ref().and_then(|l| l.lib.file()).unwrap();
        assert_eq!(natives(&m), ["libfx.so".to_owned(), file.to_owned()]);
        let json: Manifest = serde_json::from_str(r#"{"name": "j", "effects": [{"name": "A", "slot": "mic", "chain": [{"block": "gain"}]}]}"#).unwrap();
        assert!(natives(&json).is_empty());
    }

    #[test]
    fn a_zip_that_leaves_its_folder_or_holds_a_symlink_installs_nothing() {
        let t = temp("unsafe");
        let manifest = r#"{"name": "evil"}"#;
        let mut z = zip_of(
            &t,
            &[
                ("r-main/evil/plugin.json", manifest),
                ("r-main/evil/../../x.lua", "x"),
            ],
        );
        let to = t.join("out");
        let e = z.unpack("r-main/evil/", &to, &[]).unwrap_err();
        assert!(e.contains("leaves the folder"), "{e}");
        assert!(!t.join("x.lua").exists());
        let mut abs = zip_of(
            &t,
            &[
                ("r-main/evil/plugin.json", manifest),
                ("r-main/evil//etc/passwd", "x"),
            ],
        );
        assert!(abs.unpack("r-main/evil/", &t.join("out2"), &[]).is_err());

        // A symlink: the Unix attributes of the second entry say so (made by Unix, mode 0o120777).
        let mut bytes = crate::zip::write(&[
            ("e/plugin.json", manifest.as_bytes().to_vec()),
            ("e/link", b"/etc/passwd".to_vec()),
        ]);
        let at = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == [0x50, 0x4b, 1, 2])
            .nth(1)
            .unwrap()
            .0;
        bytes[at + 5] = 3;
        bytes[at + 38..at + 42].copy_from_slice(&(0o120777u32 << 16).to_le_bytes());
        let path = t.join("link.zip");
        std::fs::write(&path, bytes).unwrap();
        let e = Zip::open(&path)
            .unwrap()
            .unpack("e/", &t.join("out3"), &[])
            .unwrap_err();
        assert!(e.contains("symlink"), "{e}");
        std::fs::remove_dir_all(t).ok();
    }
}
