//! What the user decided about the plugins, kept in `<config dir>/plugins.json`: the plugins switched off and every
//! plugin's [`SettingDecl`] values. Read once ([`get`]), written on every change ([`update`]).
//!
//! ```json
//! {"disabled": ["now-playing"], "settings": {"yt-dlp": {"results": 50}}}
//! ```

use super::{Manifest, SettingDecl};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{path::PathBuf, sync::LazyLock};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Plugin name → setting id → value.
    #[serde(default)]
    pub settings: Map<String, Value>,
}

impl Config {
    pub fn enabled(&self, plugin: &str) -> bool {
        !self.disabled.iter().any(|d| d == plugin)
    }

    pub fn set_enabled(&mut self, plugin: &str, on: bool) {
        self.disabled.retain(|d| d != plugin);
        if !on {
            self.disabled.push(plugin.to_owned());
            self.disabled.sort();
        }
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
    let read = file().and_then(|f| std::fs::read_to_string(f).ok());
    RwLock::new(
        read.and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default(),
    )
});

/// The current configuration (a copy).
pub fn get() -> Config {
    CONFIG.read().clone()
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
        c.set_enabled("p", false);
        assert!(!c.enabled("p"));
        c.set_enabled("p", true);
        assert!(c.enabled("p") && c.disabled.is_empty());
    }
}
