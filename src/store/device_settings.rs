//! Device settings (child of [`super`]): the global defaults of Preferences > DJ SYSTEM and the per-device overrides, as
//! `(scope, key, value)` rows of `device_setting`. `scope` is [`GLOBAL`] (`""`) or a device id (`device::Device::id`).
//! The keys and values belong to the device plugin (`SettingsSchema`): the host stores text and never interprets it.
//! Only what differs from what the scope would have anyway is stored (see `app::prefs::dj_system::Ctx::put`), so a default
//! that changes in a later plugin version reaches every untouched setting.
//!
//! A scope's effective value is the schema's default, then the [`GLOBAL`] row, then the device's own row. What the device
//! operations receive as `config` is [`config`]: the stored rows only, the plugin applies its own defaults to the rest.
//! Three kinds of row are not settings: `written` ([`record_fingerprint`]), `behaviour.*` ([`Behaviour`]) and a device's
//! `behaviour.formats` ([`formats`]).
//!
//! A file setting (a picture) is a copy under `<data dir>/device-files/<scope>/<key>.<ext>` ([`import_file`]); its row holds
//! that path.

use super::{R, Store, query_all};
use crate::paths;
use rusqlite::params;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The scope of the defaults shared by every device.
pub const GLOBAL: &str = "";

/// Key of the fingerprint of what the device held after our last write.
const WRITTEN: &str = "written";
/// Prefix of the [`Behaviour`] keys.
const BEHAVIOUR: &str = "behaviour.";
/// Key of the library formats an export writes to a device (plugin names, comma-separated).
const FORMATS: &str = "behaviour.formats";

impl Store {
    /// The rows of `scope`.
    pub fn device_settings(&self, scope: &str) -> R<BTreeMap<String, String>> {
        query_all(
            &self.db(),
            "select key, value from device_setting where scope = ?",
            [scope],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// Stores `key = value` for `scope`.
    pub fn set_device_setting(&self, scope: &str, key: &str, value: &str) -> R<()> {
        self.run(
            "insert or replace into device_setting values (?, ?, ?)",
            params![scope, key, value],
        )
    }

    /// Forgets the override `key` of `scope` (the default applies again).
    pub fn clear_device_setting(&self, scope: &str, key: &str) -> R<()> {
        self.run(
            "delete from device_setting where scope = ? and key = ?",
            params![scope, key],
        )
    }

    /// Forgets every override of `scope` whose key starts with `prefix` (the "Reset to defaults" button; `""` = all).
    pub fn reset_device_settings(&self, scope: &str, prefix: &str) -> R<()> {
        self.run(
            "delete from device_setting where scope = ? and substr(key, 1, ?) = ?",
            params![scope, prefix.len() as i64, prefix],
        )
    }
}

/// What the device operations get as `config`: the stored global rows overlaid by the rows of `scope`.
pub fn config(store: &Store, scope: &str) -> BTreeMap<String, String> {
    let mut c = BTreeMap::new();
    for s in [GLOBAL, scope] {
        match store.device_settings(s) {
            Ok(rows) => c.extend(
                rows.into_iter()
                    .filter(|(k, _)| k != WRITTEN && !k.starts_with(BEHAVIOUR)),
            ),
            Err(e) => eprintln!("device settings: {e}"),
        }
    }
    c
}

/// Remembers the fingerprint the plugin reported for what the device of `scope` holds after we wrote its settings (an export,
/// `settings_write`), so that [`player_changed`] can tell what the player changed afterwards.
pub fn record_fingerprint(store: &Store, scope: &str, fingerprint: &str) -> R<()> {
    store.set_device_setting(scope, WRITTEN, fingerprint)
}

/// Whether the device of `scope` holds something else than what we last wrote (or we never wrote anything): the player, or
/// another computer, changed it.
pub fn player_changed(store: &Store, scope: &str, fingerprint: &str) -> bool {
    store
        .device_settings(scope)
        .ok()
        .and_then(|r| r.get(WRITTEN).map(|w| w != fingerprint))
        .unwrap_or(true)
}

/// The library formats (device plugin names) the user picked for the device `id`; `None` when never picked.
pub fn formats(store: &Store, id: &str) -> Option<Vec<String>> {
    let rows = store.device_settings(id).ok()?;
    let list = rows.get(FORMATS)?;
    Some(
        list.split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Remembers the formats picked for the device `id`.
pub fn set_formats(store: &Store, id: &str, names: &[String]) -> R<()> {
    store.set_device_setting(id, FORMATS, &names.join(","))
}

/// `<data dir>/device-files`.
pub fn files_dir() -> Option<PathBuf> {
    paths::data_dir().map(|d| d.join("device-files"))
}

/// `s` as a file name part: letters, digits, `-` and `_`; anything else becomes `_`.
fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Copies `src` to `<root>/<scope>/<key>.<ext>` (replacing the file of that key, whatever its extension) and returns the new
/// path. `extensions` limits what may be imported (case-insensitive, no dot); empty = any.
pub fn import_file(
    root: &Path,
    scope: &str,
    key: &str,
    src: &Path,
    extensions: &[String],
) -> Result<PathBuf, String> {
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !extensions.is_empty() && !extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
        return Err(format!(
            "{}: not a {} file",
            src.display(),
            extensions.join("/")
        ));
    }
    let bytes = paths::at(std::fs::read(src), src)?;
    let dir = root.join(if scope == GLOBAL {
        "global".to_owned()
    } else {
        safe(scope)
    });
    paths::at(std::fs::create_dir_all(&dir), &dir)?;
    let stem = safe(key);
    for old in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if old.path().file_stem().is_some_and(|s| *s == *stem) {
            let _ = std::fs::remove_file(old.path());
        }
    }
    let to = dir.join(if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{}", safe(&ext))
    });
    paths::at(std::fs::write(&to, bytes), &to)?;
    Ok(to)
}

/// The host-owned checkboxes of Preferences > DJ SYSTEM > DEVICE: how the host treats a device, whatever its plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Behaviour {
    /// When a playlist is deleted from a device, the tracks in it that are in no other playlist are deleted too. Off.
    pub delete_tracks: bool,
    /// Import the play histories automatically when a device is connected. On.
    pub import_history: bool,
    /// Delete the histories from the device after importing them. Off.
    pub delete_history_after_import: bool,
    /// Back up the device library automatically before it is removed. Off.
    pub auto_backup: bool,
    /// Keep the cues, beat grid and phrases of a track that is in the collection and on a device the same on both copies
    /// (`device::link`). On.
    pub sync_cues: bool,
    /// Name of this computer, shown in the device backup list. Empty = the host name.
    pub computer_name: String,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            delete_tracks: false,
            import_history: true,
            delete_history_after_import: false,
            auto_backup: false,
            sync_cues: true,
            computer_name: String::new(),
        }
    }
}

impl Behaviour {
    const KEYS: [&'static str; 6] = [
        "delete_tracks",
        "import_history",
        "delete_history_after_import",
        "auto_backup",
        "sync_cues",
        "computer_name",
    ];

    pub fn load(store: &Store) -> Self {
        let rows = store.device_settings(GLOBAL).unwrap_or_default();
        let flag = |name: &str, default: bool| {
            rows.get(&format!("{BEHAVIOUR}{name}"))
                .map_or(default, |v| v == "1")
        };
        let d = Self::default();
        Self {
            delete_tracks: flag("delete_tracks", d.delete_tracks),
            import_history: flag("import_history", d.import_history),
            delete_history_after_import: flag(
                "delete_history_after_import",
                d.delete_history_after_import,
            ),
            auto_backup: flag("auto_backup", d.auto_backup),
            sync_cues: flag("sync_cues", d.sync_cues),
            computer_name: rows
                .get(&format!("{BEHAVIOUR}computer_name"))
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Sets one field by its name (`delete_tracks`, ..., `computer_name`; booleans `"0"` / `"1"`).
    pub fn set(store: &Store, name: &str, value: &str) -> R<()> {
        if !Self::KEYS.contains(&name) {
            return Err(format!("unknown behaviour setting {name}"));
        }
        store.set_device_setting(GLOBAL, &format!("{BEHAVIOUR}{name}"), value)
    }

    /// Back to the defaults.
    pub fn reset(store: &Store) -> R<()> {
        store.reset_device_settings(GLOBAL, BEHAVIOUR)
    }

    /// The name this computer goes by: [`Self::computer_name`], else the host name.
    pub fn computer(&self) -> String {
        if !self.computer_name.is_empty() {
            return self.computer_name.clone();
        }
        // `/proc` is Linux's; Windows and macOS both ship a `hostname` program.
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .or_else(|| {
                let out = std::process::Command::new("hostname").output().ok()?;
                out.status
                    .success()
                    .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
            })
            .map(|h| h.trim().to_owned())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_the_global_rows_under_the_devices_without_bookkeeping() {
        let s = Store::in_memory().unwrap();
        assert!(config(&s, "dev1").is_empty());
        s.set_device_setting(GLOBAL, "a", "1").unwrap();
        s.set_device_setting(GLOBAL, "b", "2").unwrap();
        s.set_device_setting("dev1", "b", "3").unwrap();
        s.set_device_setting("dev2", "c", "4").unwrap();
        Behaviour::set(&s, "auto_backup", "1").unwrap();
        record_fingerprint(&s, "dev1", "f1").unwrap();
        let c = |scope| config(&s, scope).into_iter().collect::<Vec<_>>();
        let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        assert_eq!(c("dev1"), [pair("a", "1"), pair("b", "3")]);
        assert_eq!(c("dev3"), [pair("a", "1"), pair("b", "2")]);
        assert_eq!(c(GLOBAL), [pair("a", "1"), pair("b", "2")]);
    }

    #[test]
    fn the_player_changed_it_unless_the_fingerprint_is_the_one_we_wrote() {
        let s = Store::in_memory().unwrap();
        assert!(player_changed(&s, "d1", "f1"), "never written");
        record_fingerprint(&s, "d1", "f1").unwrap();
        assert!(!player_changed(&s, "d1", "f1"));
        assert!(player_changed(&s, "d1", "f2"));
        assert!(player_changed(&s, "d2", "f1"), "another device");
    }

    #[test]
    fn behaviour_round_trips() {
        let s = Store::in_memory().unwrap();
        assert_eq!(Behaviour::load(&s), Behaviour::default());
        Behaviour::set(&s, "delete_tracks", "1").unwrap();
        Behaviour::set(&s, "import_history", "0").unwrap();
        Behaviour::set(&s, "computer_name", "STUDIO").unwrap();
        let b = Behaviour::load(&s);
        assert!(b.delete_tracks && !b.import_history && !b.auto_backup);
        assert_eq!(b.computer(), "STUDIO");
        assert!(Behaviour::set(&s, "nope", "1").is_err());
        Behaviour::reset(&s).unwrap();
        assert_eq!(Behaviour::load(&s), Behaviour::default());
    }

    #[test]
    fn a_file_is_copied_per_scope_and_key_and_replaced_whatever_its_extension() {
        let dir = std::env::temp_dir().join(format!("openfader-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (jpg, png) = (dir.join("a.JPG"), dir.join("b.png"));
        std::fs::write(&jpg, [1, 2, 3]).unwrap();
        std::fs::write(&png, [4]).unwrap();
        let root = dir.join("files");
        let ext = |e: &[&str]| e.iter().map(|e| e.to_string()).collect::<Vec<_>>();
        assert!(import_file(&root, GLOBAL, "pic", &png, &ext(&["jpg", "jpeg"])).is_err());
        let g = import_file(&root, GLOBAL, "pic", &jpg, &ext(&["jpg"])).unwrap();
        assert_eq!(g, root.join("global/pic.jpg"));
        let d = import_file(&root, "usb/1", "pic", &jpg, &[]).unwrap();
        assert_eq!(d, root.join("usb_1/pic.jpg"));
        let p = import_file(&root, GLOBAL, "pic", &png, &[]).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), [4]);
        assert!(!g.exists(), "the old copy of the key is gone");
        assert!(d.exists(), "another scope keeps its own");
        assert!(import_file(&root, GLOBAL, "pic", &dir.join("none.png"), &[]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
