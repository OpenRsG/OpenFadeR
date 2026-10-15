//! Fixture profiles: a small built-in set of our own, plus user profiles read from JSON files in
//! `$XDG_CONFIG_HOME/openfader/fixtures/` (else `~/.config/openfader/fixtures/`).
//!
//! A user file holds one profile object or an array of them:
//! `{"name": "My par", "kind": "par", "channels": ["dimmer", "red", "green", "blue", 0, "strobe"]}`
//! `kind` is `par`, `moving_head` or `strobe`; a channel is one of `dimmer red green blue white strobe pan tilt`, or a
//! number 0 to 255 for a slot held at that value (macro, speed, mode).

use super::{Chan, FixtureKind, Profile};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn profile(name: &str, kind: FixtureKind, channels: &[Chan]) -> Profile {
    Profile {
        name: name.into(),
        kind,
        channels: channels.to_vec(),
    }
}

pub fn builtin() -> Vec<Profile> {
    use Chan::*;
    use FixtureKind::{MovingHead, Par};
    vec![
        profile("Generic par RGB (3ch)", Par, &[Red, Green, Blue]),
        profile(
            "Generic par RGB (5ch)",
            Par,
            &[Dimmer, Red, Green, Blue, Strobe],
        ),
        profile(
            "Generic par RGBW (6ch)",
            Par,
            &[Dimmer, Red, Green, Blue, White, Strobe],
        ),
        profile(
            "Generic moving head (8ch)",
            MovingHead,
            &[Pan, Tilt, Dimmer, Strobe, Red, Green, Blue, Fixed(0)],
        ),
        profile(
            "Generic strobe (2ch)",
            FixtureKind::Strobe,
            &[Dimmer, Strobe],
        ),
    ]
}

fn channel(v: &Value) -> Result<Chan, String> {
    if let Some(n) = v.as_u64() {
        return u8::try_from(n)
            .map(Chan::Fixed)
            .map_err(|_| format!("fixed value {n} is not 0-255"));
    }
    Ok(match v.as_str().map(str::to_ascii_lowercase).as_deref() {
        Some("dimmer") => Chan::Dimmer,
        Some("red") => Chan::Red,
        Some("green") => Chan::Green,
        Some("blue") => Chan::Blue,
        Some("white") => Chan::White,
        Some("strobe") => Chan::Strobe,
        Some("pan") => Chan::Pan,
        Some("tilt") => Chan::Tilt,
        _ => return Err(format!("unknown channel {v}")),
    })
}

fn one(v: &Value) -> Result<Profile, String> {
    let name = v["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .ok_or("profile has no \"name\"")?;
    let kind = match v["kind"].as_str() {
        Some("par") => FixtureKind::Par,
        Some("moving_head") => FixtureKind::MovingHead,
        Some("strobe") => FixtureKind::Strobe,
        other => return Err(format!("{name}: unknown kind {other:?}")),
    };
    let channels = v["channels"]
        .as_array()
        .ok_or_else(|| format!("{name}: no \"channels\" list"))?;
    let channels = channels
        .iter()
        .map(channel)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{name}: {e}"))?;
    if channels.is_empty() || channels.len() > super::SLOTS {
        return Err(format!("{name}: needs 1-512 channels"));
    }
    Ok(Profile {
        name: name.into(),
        kind,
        channels,
    })
}

/// The profiles in one JSON document (an object or an array).
pub fn parse(json: &str) -> Result<Vec<Profile>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    match v.as_array() {
        Some(list) => list.iter().map(one).collect(),
        None => Ok(vec![one(&v)?]),
    }
}

pub fn dir() -> Option<PathBuf> {
    Some(crate::paths::config_dir()?.join("fixtures"))
}

/// Every `*.json` in `dir`, sorted by file name; a broken file is skipped and named in the returned notes.
pub fn load_dir(dir: &Path) -> (Vec<Profile>, Vec<String>) {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let (mut profiles, mut notes) = (vec![], vec![]);
    for f in files {
        match std::fs::read_to_string(&f)
            .map_err(|e| e.to_string())
            .and_then(|s| parse(&s))
        {
            Ok(p) => profiles.extend(p),
            Err(e) => notes.push(format!(
                "{}: {e}",
                f.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    (profiles, notes)
}

/// Built-ins, then the user's profiles.
pub fn all() -> (Vec<Profile>, Vec<String>) {
    let mut p = builtin();
    let (user, notes) = dir().map(|d| load_dir(&d)).unwrap_or_default();
    p.extend(user);
    (p, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_object_and_an_array() {
        let one = parse(r#"{"name":"P","kind":"par","channels":["dimmer","Red",0,255]}"#).unwrap();
        assert_eq!(
            one[0].channels,
            [Chan::Dimmer, Chan::Red, Chan::Fixed(0), Chan::Fixed(255)]
        );
        let two = parse(r#"[{"name":"A","kind":"strobe","channels":["dimmer","strobe"]},{"name":"B","kind":"moving_head","channels":["pan","tilt"]}]"#).unwrap();
        assert_eq!(
            (two[0].kind, two[1].kind),
            (FixtureKind::Strobe, FixtureKind::MovingHead)
        );
    }

    #[test]
    fn rejects_bad_profiles_with_the_reason() {
        assert!(
            parse(r#"{"name":"P","kind":"laser","channels":["red"]}"#)
                .unwrap_err()
                .contains("kind")
        );
        assert!(
            parse(r#"{"name":"P","kind":"par","channels":["red","uv"]}"#)
                .unwrap_err()
                .contains("uv")
        );
        assert!(
            parse(r#"{"name":"P","kind":"par","channels":[256]}"#)
                .unwrap_err()
                .contains("256")
        );
        assert!(parse(r#"{"kind":"par","channels":["red"]}"#).is_err());
        assert!(parse("nope").is_err());
    }

    #[test]
    fn directory_load_skips_broken_files() {
        let d = std::env::temp_dir().join(format!("openfader-fixtures-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("a.json"),
            r#"{"name":"Mine","kind":"par","channels":["red","green","blue"]}"#,
        )
        .unwrap();
        std::fs::write(d.join("b.json"), "{").unwrap();
        std::fs::write(d.join("c.txt"), "ignored").unwrap();
        let (p, notes) = load_dir(&d);
        std::fs::remove_dir_all(&d).unwrap();
        assert_eq!((p.len(), p[0].name.as_str()), (1, "Mine"));
        assert!(notes.len() == 1 && notes[0].starts_with("b.json"));
    }

    #[test]
    fn builtins_fit_a_universe_and_have_unique_names() {
        let b = builtin();
        assert!(b.iter().all(|p| (1..=512).contains(&p.channels.len())));
        assert!(
            b.iter()
                .enumerate()
                .all(|(i, p)| b[..i].iter().all(|q| q.name != p.name))
        );
    }
}
