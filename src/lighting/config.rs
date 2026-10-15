//! Saved lighting settings: output protocol and address, palette and the patch, as JSON in
//! `$XDG_CONFIG_HOME/openfader/lighting.json`. Fixtures name their profile; one whose profile no longer exists is dropped.

use super::output::Protocol;
use super::scene::Palette;
use super::{Profile, Rig};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    pub protocol: Protocol,
    pub ip: String,
    pub palette: Palette,
    pub rig: Rig,
}

impl Config {
    /// First run: output off, a small stage of 4 pars, 2 moving heads and a strobe.
    pub fn starter(profiles: &[Profile]) -> Self {
        let mut rig = Rig::default();
        for (name, n) in [
            ("Generic par RGBW (6ch)", 4),
            ("Generic moving head (8ch)", 2),
            ("Generic strobe (2ch)", 1),
        ] {
            if let Some(p) = profiles.iter().find(|p| p.name == name) {
                (0..n).for_each(|_| _ = rig.add(p));
            }
        }
        Self {
            protocol: Protocol::Off,
            ip: "127.0.0.1".into(),
            palette: Palette::Vivid,
            rig,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    Some(crate::paths::config_dir()?.join("lighting.json"))
}

fn json_of(c: &Config) -> Value {
    let patch: Vec<_> = c.rig.fixtures.iter().map(|f| json!({"name": f.name, "profile": f.profile.name, "universe": f.universe, "address": f.address})).collect();
    json!({"protocol": c.protocol.label(), "ip": c.ip, "palette": c.palette.label(), "patch": patch})
}

fn from_json(v: &Value, profiles: &[Profile]) -> Config {
    let mut c = Config::starter(&[]);
    c.protocol = Protocol::ALL
        .into_iter()
        .find(|p| Some(p.label()) == v["protocol"].as_str())
        .unwrap_or(c.protocol);
    c.palette = Palette::ALL
        .into_iter()
        .find(|p| Some(p.label()) == v["palette"].as_str())
        .unwrap_or(Palette::Vivid);
    if let Some(ip) = v["ip"].as_str() {
        c.ip = ip.into();
    }
    for f in v["patch"].as_array().into_iter().flatten() {
        let Some(profile) = profiles
            .iter()
            .find(|p| Some(p.name.as_str()) == f["profile"].as_str())
        else {
            continue;
        };
        let (Some(u), Some(a)) = (
            f["universe"].as_u64().and_then(|u| u16::try_from(u).ok()),
            f["address"].as_u64().and_then(|a| u16::try_from(a).ok()),
        ) else {
            continue;
        };
        if let Some(i) = c.rig.add(profile) {
            if c.rig.set_address(i, u, a).is_err() {
                c.rig.fixtures.pop();
                continue;
            }
            if let Some(n) = f["name"].as_str() {
                c.rig.fixtures[i].name = n.into();
            }
        }
    }
    c
}

/// The saved config; a missing or unreadable file gives [`Config::starter`].
pub fn load(path: &Path, profiles: &[Profile]) -> Config {
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        Some(v) => from_json(&v, profiles),
        None => Config::starter(profiles),
    }
}

/// Atomic write (temp file + rename).
pub fn save(path: &Path, c: &Config) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&json_of(c))?)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::fixtures::builtin;

    #[test]
    fn round_trips_and_drops_unknown_profiles() {
        let b = builtin();
        let mut c = Config::starter(&b);
        c.protocol = Protocol::Both;
        c.ip = "192.0.2.7".into();
        c.palette = Palette::Club2;
        c.rig.set_address(0, 2, 100).unwrap();
        c.rig.fixtures[1].name = "Left".into();
        let file = std::env::temp_dir()
            .join(format!("openfader-lighting-{}", std::process::id()))
            .join("lighting.json");
        save(&file, &c).unwrap();
        assert_eq!(load(&file, &b), c);
        let fewer = load(&file, &b[..1]);
        assert!(
            fewer.rig.fixtures.is_empty(),
            "profiles outside the list are dropped"
        );
        std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
        assert_eq!(
            load(&file, &b),
            Config::starter(&b),
            "missing file = starter stage"
        );
        assert_eq!(c.rig.fixtures.len(), 7);
    }
}
