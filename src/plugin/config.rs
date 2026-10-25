//! What the user decided about the plugins, kept in `<config dir>/plugins.json`: the plugins switched on (a plugin is off
//! until the user switches it on or installs it; before the user ever decided, the [`DEFAULT_ON`] ones are on, see
//! [`seed`]), the plugins uninstalled (a bundled copy stays hidden until the plugin is
//! installed again) and every plugin's [`SettingDecl`] values. Read once ([`get`]), written on every change ([`update`]).
//!
//! ```json
//! {"enabled": ["now-playing"], "removed": ["yt-dlp"], "settings": {"yt-dlp": {"results": 50}}}
//! ```

use super::{Manifest, SettingDecl};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{path::PathBuf, sync::LazyLock};

/// The plugins that are on until the user decides: the readers of the devices and libraries other DJ software writes and
/// the effect packs. Only bundled names count (a plugin cannot ask to be on by itself), and only while there is no
/// `plugins.json` ([`seed`]): a file that exists, even one listing none of them, is the user's choice. The rekordbox
/// readers are not here: they are built into the app ([`super::builtin`]) and always on.
pub const DEFAULT_ON: &[&str] = &[
    "engine-dj",
    "serato",
    "traktor",
    "virtualdj",
    "beat-fx",
    "send-fx",
    "lever-fx",
    "mic-fx",
    "soft-mix",
    "cross-pass",
];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub enabled: Vec<String>,
    /// There was no `plugins.json` when this was read: the user has decided nothing yet ([`Config::seed`]).
    #[serde(skip)]
    fresh: bool,
    /// The older file listed the plugins switched off and had every other one on; [`Config::adopt`] turns it into
    /// `enabled` once.
    #[serde(default, skip_serializing)]
    disabled: Option<Vec<String>>,
    /// Uninstalled plugins: no copy of them is read ([`super::install::uninstall`]; an install takes the name out).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    /// Plugin name → setting id → value.
    #[serde(default)]
    pub settings: Map<String, Value>,
}

impl Config {
    pub fn enabled(&self, plugin: &str) -> bool {
        self.enabled.iter().any(|d| d == plugin)
    }

    pub fn set_enabled(&mut self, plugin: &str, on: bool) {
        self.enabled.retain(|d| d != plugin);
        if on {
            self.enabled.push(plugin.to_owned());
            self.enabled.sort();
        }
    }

    /// Before any decision (no file was read): switches on the [`DEFAULT_ON`] plugins among `found`, once. The next write
    /// to the file records them like the user's own switches. Whether it did anything.
    fn seed(&mut self, found: &[&str]) -> bool {
        if !std::mem::take(&mut self.fresh) {
            return false;
        }
        for p in found.iter().filter(|p| DEFAULT_ON.contains(p)) {
            self.set_enabled(p, true);
        }
        true
    }

    /// An older file kept every plugin on but the ones it listed: `found` minus those is switched on, so an upgrade
    /// keeps what the user had. Whether there was such a file.
    pub fn adopt(&mut self, found: &[&str]) -> bool {
        let Some(off) = self.disabled.take() else {
            return false;
        };
        for p in found.iter().filter(|p| !off.iter().any(|o| o == *p)) {
            self.set_enabled(p, true);
        }
        true
    }

    /// Every setting of `m` with the user's value where it fits the setting's type, else the default.
    pub fn values(&self, m: &Manifest) -> Value {
        let own = self.settings.get(&m.name).and_then(Value::as_object);
        Value::Object(
            m.settings
                .iter()
                .map(|s| {
                    let v = own.and_then(|o| o.get(&s.id)).filter(|v| fits(s, v));
                    (
                        s.id.clone(),
                        v.cloned().unwrap_or_else(|| s.default_value()),
                    )
                })
                .collect(),
        )
    }

    /// Stores `value` for setting `id` of plugin `plugin`.
    pub fn set(&mut self, plugin: &str, id: &str, value: Value) {
        let own = self
            .settings
            .entry(plugin.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(o) = own.as_object_mut() {
            o.insert(id.to_owned(), value);
        }
    }
}

/// Whether `v` is a value of `s`'s type (and one of its choices).
fn fits(s: &SettingDecl, v: &Value) -> bool {
    use super::SettingKind::*;
    match s.kind {
        Bool => v.is_boolean(),
        Number => v.is_number(),
        Text | Secret => v.is_string(),
        Choice => v.as_str().is_some_and(|c| s.choices.iter().any(|x| x == c)),
    }
}

fn file() -> Option<PathBuf> {
    crate::paths::config_dir().map(|d| d.join("plugins.json"))
}

static CONFIG: LazyLock<RwLock<Config>> = LazyLock::new(|| {
    let file = file();
    let read = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok());
    let mut c: Config = read
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // No folder to write to (no HOME) or an unreadable file is not "never decided": nothing is switched on behind it.
    c.fresh = file.is_some_and(|f| !f.exists());
    RwLock::new(c)
});

/// The current configuration (a copy).
pub fn get() -> Config {
    CONFIG.read().clone()
}

/// Switches on the [`DEFAULT_ON`] plugins among `found` when the user has decided nothing yet (no `plugins.json`), in
/// memory only: nothing is written until the user changes something (the write then records them like the user's own
/// switches). Later calls do nothing.
pub fn seed(found: &[&str]) {
    CONFIG.write().seed(found);
}

/// Changes the configuration and writes it.
pub fn update(f: impl FnOnce(&mut Config)) -> Result<(), String> {
    let mut c = CONFIG.write();
    f(&mut c);
    let Some(path) = file() else {
        return Err("no config folder (set HOME)".into());
    };
    if let Some(dir) = path.parent() {
        crate::paths::at(std::fs::create_dir_all(dir), dir)?;
    }
    let text = serde_json::to_string_pretty(&*c).map_err(|e| e.to_string())?;
    crate::paths::at(std::fs::write(&path, text), &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_apply_only_before_the_user_decided() {
        let found = ["beat-fx", "engine-dj", "now-playing", "mic-fx"];
        // No plugins.json yet: the defaults among the found plugins are on, once; others stay off.
        let mut c = Config { fresh: true, ..Config::default() };
        assert!(c.seed(&found));
        assert_eq!(c.enabled, ["beat-fx", "engine-dj", "mic-fx"]);
        assert!(!c.seed(&found), "once");
        // A switch the user makes afterwards wins, and is what the file then says.
        c.set_enabled("mic-fx", false);
        assert!(!c.seed(&found) && !c.enabled("mic-fx"));
        // An existing file, whatever it lists, is the user's choice: even one that lists none of them.
        for text in [r#"{"enabled": ["now-playing"]}"#, r#"{"settings": {}}"#, "{}"] {
            let mut c: Config = serde_json::from_str(text).unwrap();
            let before = c.enabled.clone();
            assert!(!c.seed(&found) && c.enabled == before, "{text}");
        }
        // A default that is not installed is not invented.
        let mut c = Config { fresh: true, ..Config::default() };
        c.seed(&["now-playing"]);
        assert!(c.enabled.is_empty());
    }

    #[test]
    fn values_fall_back_to_the_default_when_missing_or_of_the_wrong_type() {
        let m: Manifest = serde_json::from_value(json!({
            "name": "p",
            "settings": [
                {"id": "on", "label": "On", "type": "bool", "default": true},
                {"id": "n", "label": "N", "type": "number"},
                {"id": "mode", "label": "Mode", "type": "choice", "choices": ["a", "b"]},
                {"id": "t", "label": "T", "default": 5},
                {"id": "k", "label": "K", "type": "secret"}
            ]
        }))
        .unwrap();
        let mut c = Config::default();
        assert_eq!(
            c.values(&m),
            json!({"on": true, "n": 0, "mode": "a", "t": "", "k": ""})
        );
        c.set("p", "n", json!(3.5));
        c.set("p", "mode", json!("z")); // not a choice
        c.set("p", "on", json!("yes")); // not a bool
        c.set("p", "k", json!("s3"));
        assert_eq!(
            c.values(&m),
            json!({"on": true, "n": 3.5, "mode": "a", "t": "", "k": "s3"})
        );
        assert!(!c.enabled("p"), "off until switched on");
        c.set_enabled("p", true);
        assert!(c.enabled("p"));
        c.set_enabled("p", false);
        assert!(!c.enabled("p") && c.enabled.is_empty());
        // The older file: everything on but what it lists; read once.
        let mut old: Config = serde_json::from_value(json!({"disabled": ["b"]})).unwrap();
        assert!(old.adopt(&["a", "b"]) && old.enabled == ["a"]);
        assert!(!old.adopt(&["a", "b", "c"]) && old.enabled == ["a"]);
        assert!(!serde_json::to_string(&old).unwrap().contains("disabled"));
    }
}
