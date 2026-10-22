//! The user's skin folder (`<config dir>/extension/skin`): where themes made in the editor, duplicated or installed from a file live.
//! Everything that writes goes through [`write_into`], which refuses the built-in names and the names of bundled or `$OPENFADER_SKINS`
//! themes (those folders are not ours to change) and replaces the user's own theme of the same name. The `*_in` functions take the
//! folder and the skins found so far, so tests never touch the real config folder; the public ones use [`user_dir`] and [`found`].

use super::{BUILTIN, Skin, file, found, rescan};
use std::path::{Path, PathBuf};

/// `<config dir>/extension/skin` (`None` when neither `$XDG_CONFIG_HOME` nor `$HOME` is set).
pub fn user_dir() -> Option<PathBuf> {
    openfader::paths::config_dir().map(|c| c.join("extension/skin"))
}

/// The folder, made if it does not exist yet (for the editor's save and the "open folder" button).
pub fn ensure_user_dir() -> Result<PathBuf, String> {
    let dir = user_dir().ok_or(tr!(
        "no config folder: neither XDG_CONFIG_HOME nor HOME is set"
    ))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// What the skin folders hold, for the watcher in `page`: every `*.json` with its size and time of change, in a stable order. Two
/// signatures differ when a theme file was added, removed or edited.
pub fn signature(dirs: &[PathBuf]) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
    let mut out: Vec<_> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| {
            let m = e.metadata().ok();
            (
                e.path(),
                m.as_ref().map_or(0, |m| m.len()),
                m.and_then(|m| m.modified().ok()),
            )
        })
        .collect();
    out.sort();
    out
}
/// Whether `s` is a file of the user's folder `dir` (the ones that can be changed and deleted).
pub fn is_user_in(dir: &Path, s: &Skin) -> bool {
    s.path.as_deref().and_then(Path::parent) == Some(dir)
}

pub fn is_user(s: &Skin) -> bool {
    user_dir().is_some_and(|d| is_user_in(&d, s))
}

/// A file name for a theme name: lower-case letters and digits, anything else one `-` (`Tokyo Night!` → `tokyo-night`).
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "theme".into()
    } else {
        out.into()
    }
}

/// Writes `skin` into `dir` as `<slug>.json` (an existing theme of the user's with that name is overwritten) and returns the path.
pub fn write_into(dir: &Path, skins: &[Skin], skin: &Skin) -> Result<PathBuf, String> {
    let name = skin.name.trim();
    if name.is_empty() {
        return Err(tr!("give the theme a name").into());
    }
    if BUILTIN.iter().any(|b| b.eq_ignore_ascii_case(name)) {
        return Err(tr!(
            "{} is a built-in theme: pick another name",
            format!("{name:?}")
        ));
    }
    let same = skins.iter().find(|s| s.name.eq_ignore_ascii_case(name));
    if same.is_some_and(|s| !is_user_in(dir, s)) {
        return Err(tr!(
            "{} is a bundled theme: pick another name",
            format!("{name:?}")
        ));
    }
    let path = match same.and_then(|s| s.path.clone()) {
        Some(p) => p,
        None => {
            // another file may already use the slug under a different name: number the file, not the theme
            let mut p = dir.join(format!("{}.json", slug(name)));
            for n in 2.. {
                if !p.exists() {
                    break;
                }
                p = dir.join(format!("{}-{n}.json", slug(name)));
            }
            p
        }
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let skin = Skin {
        name: name.to_owned(),
        ..skin.clone()
    };
    std::fs::write(&path, file::serialize(&skin))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Copies the theme file `path` into `dir` after reading it as a skin (its own name decides the file; a file with no valid token is
/// no theme). Returns the theme's name.
pub fn install_in(dir: &Path, skins: &[Skin], path: &Path) -> Result<String, String> {
    let at = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| format!("{at}: {e}"))?;
    let stem = path
        .file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let (skin, notes) = file::parse(&text, &stem).map_err(|e| format!("{at}: {e}"))?;
    if skin.tokens.is_empty() {
        let why = notes.first().map_or(String::new(), |n| format!(" ({n})"));
        return Err(tr!("{}: no theme tokens in the file{}", at, why));
    }
    write_into(dir, skins, &skin)?;
    Ok(skin.name)
}

/// Copies the theme file `path` into the user's skin folder, makes it a theme (see [`install_in`]), re-reads the skin folders and
/// returns the theme's name. The theme gallery, VIEW > Color > Skin and the settings pick it up at once; choosing it is up to the
/// caller (`prefs::view::select_skin`).
pub fn install(path: &Path) -> Result<String, String> {
    let name = install_in(&ensure_user_dir()?, &found(), path)?;
    rescan();
    Ok(name)
}

/// Deletes the user's theme `name` from `dir` (a built-in or a bundled theme is not ours to delete).
pub fn remove_in(dir: &Path, skins: &[Skin], name: &str) -> Result<(), String> {
    let skin = skins
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| tr!("no theme {}", format!("{name:?}")))?;
    if !is_user_in(dir, skin) {
        return Err(tr!(
            "{} is not one of your themes: only the files in {} can be deleted",
            format!("{name:?}"),
            dir.display()
        ));
    }
    let path = skin.path.as_ref().expect("a user skin has a file");
    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `base` with " copy" (then " copy 2", …) added: the first name that none of `taken` has.
pub fn copy_name(base: &str, taken: &[String]) -> String {
    let base = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ');
    let base = base.strip_suffix("copy").unwrap_or(base).trim_end();
    (1..)
        .map(|n| match n {
            1 => format!("{base} copy"),
            n => format!("{base} copy {n}"),
        })
        .find(|c| !taken.iter().any(|t| t.eq_ignore_ascii_case(c)))
        .expect("an unbounded range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::skin::{Base, Value};
    use slint::Color;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("openfader-store-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        d
    }

    fn themed(name: &str) -> Skin {
        Skin::new(
            name,
            Base::Dark,
            vec![("accent", Value::Color(Color::from_rgb_u8(1, 2, 3)))],
        )
    }

    /// The skins `dir` holds, as the app would find them.
    fn read(dir: &Path) -> Vec<Skin> {
        file::discover(&[dir.to_path_buf()]).0
    }

    #[test]
    fn slugs_are_plain_file_names() {
        assert_eq!(slug("Tokyo Night!"), "tokyo-night");
        assert_eq!(slug("  Frappé "), "frappé");
        assert_eq!(slug("???"), "theme");
    }

    #[test]
    fn a_saved_theme_is_found_again_and_saving_the_same_name_replaces_it() {
        let dir = temp("save");
        let first = write_into(&dir, &[], &themed("My Theme")).unwrap();
        assert_eq!(first, dir.join("my-theme.json"));
        let mut again = themed("my theme");
        again.tokens[0].1 = Value::Color(Color::from_rgb_u8(9, 9, 9));
        let skins = read(&dir);
        assert_eq!(
            write_into(&dir, &skins, &again).unwrap(),
            first,
            "same name, same file"
        );
        let skins = read(&dir);
        assert_eq!(skins.len(), 1);
        assert_eq!(
            skins[0].tokens[0].1,
            Value::Color(Color::from_rgb_u8(9, 9, 9))
        );
        // another name whose slug is taken gets a numbered file
        let other = write_into(&dir, &skins, &themed("My-Theme!")).unwrap();
        assert_eq!(other, dir.join("my-theme-2.json"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn built_in_and_bundled_names_are_refused() {
        let (dir, bundled) = (temp("user"), temp("bundled"));
        write_into(&bundled, &[], &themed("Bundled")).unwrap();
        let skins = read(&bundled);
        assert!(write_into(&dir, &skins, &themed("Catppuccin Mocha")).is_err());
        assert!(write_into(&dir, &skins, &themed("CATPPUCCIN LATTE")).is_err());
        assert!(
            write_into(&dir, &skins, &themed("bundled"))
                .unwrap_err()
                .contains("bundled")
        );
        assert!(write_into(&dir, &skins, &themed("  ")).is_err());
        assert!(!dir.exists(), "nothing was written");
        std::fs::remove_dir_all(bundled).ok();
    }

    #[test]
    fn installing_validates_copies_and_reports() {
        let (src, dir) = (temp("src"), temp("dest"));
        std::fs::create_dir_all(&src).unwrap();
        let good = src.join("whatever.json");
        std::fs::write(
            &good,
            r##"{"name":"Fresh","author":"me","layout":{"deck_layout":"vertical"},"tokens":{"accent":"#112233","nope":1}}"##,
        )
        .unwrap();
        assert_eq!(install_in(&dir, &[], &good).unwrap(), "Fresh");
        let skins = read(&dir);
        assert_eq!(skins.len(), 1);
        assert_eq!(skins[0].author, "me");
        assert_eq!(
            skins[0].layout,
            [("deck_layout".to_owned(), "vertical".to_owned())]
        );
        assert_eq!(skins[0].tokens.len(), 1, "the unknown token is dropped");
        assert!(is_user_in(&dir, &skins[0]));
        // not a theme at all
        let junk = src.join("package.json");
        std::fs::write(&junk, r#"{"name":"pkg","version":"1"}"#).unwrap();
        assert!(
            install_in(&dir, &skins, &junk)
                .unwrap_err()
                .contains("no theme tokens")
        );
        let broken = src.join("broken.json");
        std::fs::write(&broken, "{").unwrap();
        assert!(
            install_in(&dir, &skins, &broken)
                .unwrap_err()
                .contains("not valid JSON")
        );
        assert!(install_in(&dir, &skins, &src.join("missing.json")).is_err());
        assert_eq!(read(&dir).len(), 1);
        std::fs::remove_dir_all(src).ok();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn only_the_users_own_themes_can_be_deleted() {
        let (user, bundled) = (temp("del-user"), temp("del-bundled"));
        write_into(&user, &[], &themed("Mine")).unwrap();
        write_into(&bundled, &[], &themed("Theirs")).unwrap();
        let skins: Vec<Skin> = read(&user).into_iter().chain(read(&bundled)).collect();
        assert!(remove_in(&user, &skins, "theirs").is_err());
        assert!(remove_in(&user, &skins, "catppuccin mocha").is_err());
        remove_in(&user, &skins, "MINE").unwrap();
        assert!(read(&user).is_empty());
        assert!(!read(&bundled).is_empty());
        std::fs::remove_dir_all(user).ok();
        std::fs::remove_dir_all(bundled).ok();
    }

    #[test]
    fn copies_get_the_first_free_name() {
        let taken = vec![
            "Nord".to_owned(),
            "Nord copy".to_owned(),
            "nord copy 2".to_owned(),
        ];
        assert_eq!(copy_name("Nord", &taken), "Nord copy 3");
        assert_eq!(copy_name("Nord copy 2", &taken), "Nord copy 3");
        assert_eq!(copy_name("Dracula", &taken), "Dracula copy");
    }
}
