//! Skin files: `*.json` in the skin folders (`$OPENFADER_SKINS`, `<config dir>/extension/skin`, the bundled `extension/skin`; see [`openfader::paths::search_path`]).
//!
//! ```json
//! { "name": "Ocean", "author": "me", "description": "Cool blues", "base": "dark",
//!   "layout": { "deck_layout": "vertical" }, "tokens": { "accent": "#00b4d8", "font-size": 12, "t-fast": "80ms" } }
//! ```
//! `name` (default: the file name) is what VIEW > Color > Skin lists and stores; `base` (`dark`, the default, or `light`) picks the
//! built-in values every token not listed keeps; `author` and `description` are shown in the theme gallery; `layout` holds VIEW
//! settings the theme sets when it is chosen ([`LAYOUT_KEYS`]). A token that is unknown, of the wrong type or has a bad value is
//! reported and left at the base's value; the rest of the skin still applies. Token values by type: colour `"#rrggbb"` /
//! `"#rrggbbaa"`; length a number or `"12px"`; string text; int a whole number; float a number; duration a number of ms,
//! `"80ms"` or `"0.2s"`. [`serialize`] writes the same format back (the theme editor and installer use it).

use super::{Base, Skin, Ty, Value, TOKENS};
use serde_json::Value as Json;
use slint::Color;
use std::path::{Path, PathBuf};

/// The VIEW settings a skin's `layout` may set, by their names in the settings file (the values are those of the PREFERENCES rows,
/// e.g. `deck_layout`: `horizontal` / `vertical`, `decks`: `2` / `4`, `waveform_colors`: `threeband` / `rgb` / `blue`).
pub const LAYOUT_KEYS: [&str; 11] = [
    "deck_layout",
    "decks",
    "waveform_colors",
    "hot_cue_colors",
    "pad_fx_color",
    "sampler_color",
    "line_space",
    "browse_font",
    "browse_bold",
    "overview",
    "show_mixer",
];

/// The skins under `dirs` (earlier folders win a name clash), with one note per problem found.
pub fn discover(dirs: &[PathBuf]) -> (Vec<Skin>, Vec<String>) {
    let (mut skins, mut notes) = (Vec::new(), Vec::new());
    for dir in dirs {
        notes.extend(load_dir(dir, &mut skins));
    }
    (skins, notes)
}

/// Adds the skins of the `*.json` files in `dir` (sorted by file name; a missing folder holds none) to `out`.
fn load_dir(dir: &Path, out: &mut Vec<Skin>) -> Vec<String> {
    let mut notes = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return notes;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    for path in files {
        let at = path.display();
        let stem = path
            .file_stem()
            .map_or(String::new(), |s| s.to_string_lossy().into_owned());
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| parse(&text, &stem));
        match parsed {
            Err(e) => notes.push(format!("skin {at}: {e}")),
            Ok((mut skin, file_notes)) => {
                skin.path = Some(path.clone());
                notes.extend(file_notes.into_iter().map(|n| format!("skin {at}: {n}")));
                if super::BUILTIN
                    .into_iter()
                    .chain(out.iter().map(|s| s.name.as_str()))
                    .any(|n| n.eq_ignore_ascii_case(&skin.name))
                {
                    notes.push(format!(
                        "skin {at}: the name {:?} is taken, skipped",
                        skin.name
                    ));
                } else {
                    out.push(skin);
                }
            }
        }
    }
    notes
}

/// The skin in `text` (`stem`: the file name without `.json`) and the notes on what was left out. An error means the file holds no skin.
pub fn parse(text: &str, stem: &str) -> Result<(Skin, Vec<String>), String> {
    let json: Json = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let obj = json
        .as_object()
        .ok_or("the file must hold one JSON object")?;
    let mut notes = Vec::new();
    let name = match obj.get("name") {
        None => stem.to_owned(),
        Some(Json::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
        Some(_) => return Err("\"name\" must be a non-empty string".into()),
    };
    let base = match obj.get("base").map(Json::as_str) {
        None => Base::Dark,
        Some(Some(b)) if b.eq_ignore_ascii_case("dark") => Base::Dark,
        Some(Some(b)) if b.eq_ignore_ascii_case("light") => Base::Light,
        Some(_) => {
            notes.push("\"base\" must be \"dark\" or \"light\", using \"dark\"".into());
            Base::Dark
        }
    };
    notes.extend(
        obj.keys()
            .filter(|k| {
                !["name", "base", "tokens", "author", "description", "layout"].contains(&k.as_str())
            })
            .map(|k| format!("unknown key {k:?} ignored")),
    );
    let mut string = |key: &str| match obj.get(key) {
        None => String::new(),
        Some(Json::String(s)) => s.trim().to_owned(),
        Some(_) => {
            notes.push(format!("{key:?} must be a string, ignored"));
            String::new()
        }
    };
    let (author, description) = (string("author"), string("description"));
    let mut layout = Vec::new();
    match obj.get("layout") {
        None => {}
        Some(Json::Object(map)) => {
            for (key, v) in map {
                let v = match v {
                    Json::String(s) => s.trim().to_owned(),
                    Json::Bool(_) | Json::Number(_) => v.to_string(),
                    _ => {
                        notes.push(format!(
                            "layout {key:?}: expected a text, number or true / false, got {v}"
                        ));
                        continue;
                    }
                };
                match LAYOUT_KEYS.iter().find(|k| **k == key) {
                    Some(k) => layout.push((k.to_string(), v)),
                    None => notes.push(format!(
                        "unknown layout setting {key:?} (one of {})",
                        LAYOUT_KEYS.join(", ")
                    )),
                }
            }
        }
        Some(_) => notes.push("\"layout\" must be an object, none applied".into()),
    }
    let mut tokens = Vec::new();
    match obj.get("tokens") {
        None => {}
        Some(Json::Object(map)) => {
            for (key, v) in map {
                let Some(&(token, ty)) = TOKENS.iter().find(|(n, _)| n == key) else {
                    notes.push(format!("unknown token {key:?} (the names are the in-out properties of ui/theme.slint, docs/skins.md)"));
                    continue;
                };
                match value(ty, v) {
                    Ok(v) => tokens.push((token, v)),
                    Err(e) => notes.push(format!("token {key:?}: {e}")),
                }
            }
        }
        Some(_) => notes.push("\"tokens\" must be an object, no token applied".into()),
    }
    // the file order is the JSON map's (sorted by name); the theme.slint order is the one the editor and the files we write use
    tokens.sort_by_key(|(n, _)| TOKENS.iter().position(|t| t.0 == *n));
    let skin = Skin {
        name,
        base,
        tokens,
        author,
        description,
        layout,
        path: None,
    };
    Ok((skin, notes))
}

/// `j` as a value of type `ty`.
fn value(ty: Ty, j: &Json) -> Result<Value, String> {
    let bad = |want: &str| format!("expected {want}, got {j}");
    Ok(match ty {
        Ty::Color => Value::Color(color(
            j.as_str().ok_or_else(|| bad("a colour like \"#1473eb\""))?,
        )?),
        Ty::Length => Value::Length(
            number(j, "px").ok_or_else(|| bad("a length like 12 or \"12px\""))? as f32,
        ),
        Ty::Str => Value::Str(j.as_str().ok_or_else(|| bad("a string"))?.to_owned()),
        Ty::Int => Value::Int(
            j.as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| bad("a whole number"))?,
        ),
        Ty::Float => Value::Float(j.as_f64().ok_or_else(|| bad("a number"))? as f32),
        Ty::Duration => {
            // a bare number is ms; "80ms" and "0.2s" as in ui/theme.slint
            let ms = match j.as_str() {
                Some(s) => s
                    .strip_suffix("ms")
                    .and_then(|n| n.trim().parse::<f64>().ok())
                    .or_else(|| {
                        s.strip_suffix('s')
                            .and_then(|n| n.trim().parse::<f64>().ok())
                            .map(|s| s * 1000.0)
                    }),
                None => j.as_f64(),
            };
            let ms = ms
                .filter(|ms| ms.is_finite() && *ms >= 0.0)
                .ok_or_else(|| bad("a duration like 80, \"80ms\" or \"0.2s\""))?;
            Value::Duration(ms.round() as i64)
        }
    })
}

/// A JSON number, or a string holding one with an optional `unit` suffix.
fn number(j: &Json, unit: &str) -> Option<f64> {
    match j {
        Json::String(s) => s.strip_suffix(unit).unwrap_or(s).trim().parse().ok(),
        j => j.as_f64(),
    }
    .filter(|n| n.is_finite())
}

/// `#rrggbb` or `#rrggbbaa`.
fn color(s: &str) -> Result<Color, String> {
    let hex = s
        .strip_prefix('#')
        .filter(|h| matches!(h.len(), 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| format!("bad colour {s:?}, use #rrggbb or #rrggbbaa"))?;
    let v = u32::from_str_radix(hex, 16).expect("checked hex digits");
    let (rgb, a) = if hex.len() == 6 {
        (v, 255)
    } else {
        (v >> 8, v & 255)
    };
    Ok(Color::from_argb_u8(
        a as u8,
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ))
}

/// A value of type `ty` typed by hand (the editor's fields): the JSON forms, a colour without its `#` too, a number without a quote.
pub fn from_text(ty: Ty, text: &str) -> Result<Value, String> {
    let t = text.trim();
    let json = if ty == Ty::Str {
        Json::String(text.to_owned())
    } else if ty == Ty::Color && !t.starts_with('#') && matches!(t.len(), 6 | 8) {
        Json::String(format!("#{t}"))
    } else if let Ok(n) = t.parse::<i64>() {
        Json::from(n)
    } else if let Some(n) = t.parse::<f64>().ok().filter(|n| n.is_finite()) {
        Json::from(n)
    } else {
        Json::String(t.to_owned())
    };
    value(ty, &json)
}

/// `c` as `#rrggbb`, or `#rrggbbaa` when it is not opaque.
pub fn hex(c: Color) -> String {
    match c.alpha() {
        255 => format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue()),
        a => format!("#{:02x}{:02x}{:02x}{a:02x}", c.red(), c.green(), c.blue()),
    }
}

/// A number as JSON without float noise (`12`, `0.5`; two decimals at most).
fn num(n: f64) -> Json {
    let r = (n * 100.0).round() / 100.0;
    if r.fract() == 0.0 {
        Json::from(r as i64)
    } else {
        Json::from(r)
    }
}

/// `v` in its JSON form (what [`serialize`] writes).
fn to_json(v: &Value) -> Json {
    match v {
        Value::Color(c) => Json::String(hex(*c)),
        Value::Length(n) | Value::Float(n) => num(f64::from(*n)),
        Value::Str(s) => Json::String(s.clone()),
        Value::Int(n) => Json::from(*n),
        Value::Duration(ms) => Json::from(*ms),
    }
}

/// `v` as the editor's field shows it: `#rrggbb`, a number, `80ms`, a font name.
pub fn show(v: &Value) -> String {
    match v {
        Value::Duration(ms) => format!("{ms}ms"),
        Value::Str(s) => s.clone(),
        v => match to_json(v) {
            Json::String(s) => s,
            j => j.to_string(),
        },
    }
}

/// `skin` as a file (the keys in a readable order, one token per line; what [`parse`] reads back).
pub fn serialize(skin: &Skin) -> String {
    let q = |s: &str| Json::String(s.to_owned()).to_string();
    let mut out = format!("{{\n  \"name\": {},\n", q(&skin.name));
    for (key, v) in [("author", &skin.author), ("description", &skin.description)] {
        if !v.is_empty() {
            out += &format!("  \"{key}\": {},\n", q(v));
        }
    }
    let base = if skin.base == Base::Light {
        "light"
    } else {
        "dark"
    };
    out += &format!("  \"base\": \"{base}\",\n");
    if !skin.layout.is_empty() {
        let lines: Vec<String> = skin
            .layout
            .iter()
            .map(|(k, v)| format!("    {}: {}", q(k), q(v)))
            .collect();
        out += &format!("  \"layout\": {{\n{}\n  }},\n", lines.join(",\n"));
    }
    let lines: Vec<String> = skin
        .tokens
        .iter()
        .map(|(k, v)| format!("    {}: {}", q(k), to_json(v)))
        .collect();
    out + &format!("  \"tokens\": {{\n{}\n  }}\n}}\n", lines.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(skin: &Skin) -> Vec<(&'static str, Value)> {
        skin.tokens.clone()
    }

    #[test]
    fn every_token_type_parses_from_its_json_form() {
        let (skin, notes) = parse(
            r##"{"name":" Ocean ","base":"LIGHT","tokens":{"accent":"#00b4d8","scrim":"#11223344","font-size":12,
                "font-md":"13px","font":"Inter","weight":500,"t-fast":"0.2s","t-fade":150,"tooltip-delay":"600ms"}}"##,
            "ocean",
        )
        .unwrap();
        assert_eq!(
            (skin.name.as_str(), skin.base, notes.len()),
            ("Ocean", Base::Light, 0),
            "{notes:?}"
        );
        let got = |n: &str| tokens(&skin).into_iter().find(|t| t.0 == n).map(|t| t.1);
        assert_eq!(
            got("accent"),
            Some(Value::Color(Color::from_rgb_u8(0, 0xb4, 0xd8)))
        );
        assert_eq!(
            got("scrim"),
            Some(Value::Color(Color::from_argb_u8(0x44, 0x11, 0x22, 0x33)))
        );
        assert_eq!(got("font-size"), Some(Value::Length(12.0)));
        assert_eq!(got("font-md"), Some(Value::Length(13.0)));
        assert_eq!(got("font"), Some(Value::Str("Inter".into())));
        assert_eq!(got("weight"), Some(Value::Int(500)));
        assert_eq!(got("t-fast"), Some(Value::Duration(200)));
        assert_eq!(got("t-fade"), Some(Value::Duration(150)));
        assert_eq!(got("tooltip-delay"), Some(Value::Duration(600)));
    }

    #[test]
    fn a_bad_token_is_reported_and_the_rest_of_the_skin_still_applies() {
        let (skin, notes) = parse(
            r##"{"tokens":{"acent":"#fff","accent":"red","font-size":"big","weight":400.5,"font":3,"t-fast":"-1ms","play":"#2dd728",
                "danger":"#12345"}}"##,
            "mine",
        )
        .unwrap();
        assert_eq!(skin.name, "mine", "no name: the file name");
        assert_eq!(
            tokens(&skin),
            [("play", Value::Color(Color::from_rgb_u8(0x2d, 0xd7, 0x28)))]
        );
        let all = notes.join("\n");
        for want in [
            "unknown token \"acent\"",
            "bad colour \"red\"",
            "token \"font-size\"",
            "token \"weight\"",
            "token \"font\"",
            "token \"t-fast\"",
            "bad colour \"#12345\"",
        ] {
            assert!(all.contains(want), "{want} not in {all}");
        }
        assert_eq!(notes.len(), 7, "{all}");
    }

    #[test]
    fn a_file_that_is_no_skin_is_an_error_and_odd_headers_are_noted() {
        assert!(parse("{ nope", "x").unwrap_err().contains("not valid JSON"));
        assert!(parse("[1]", "x").unwrap_err().contains("one JSON object"));
        assert!(parse(r#"{"name":""}"#, "x").is_err());
        let (skin, notes) = parse(r#"{"base":"grey","extra":1,"tokens":[]}"#, "x").unwrap();
        assert_eq!(
            (skin.base, skin.tokens.len(), notes.len()),
            (Base::Dark, 0, 3),
            "{notes:?}"
        );
    }

    #[test]
    fn discovery_skips_taken_and_broken_files_and_the_first_folder_wins() {
        let root = std::env::temp_dir().join(format!("openfader-skins-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(
            a.join("one.json"),
            r##"{"name":"One","tokens":{"accent":"#010101"}}"##,
        )
        .unwrap();
        std::fs::write(a.join("broken.json"), "{").unwrap();
        std::fs::write(a.join("readme.txt"), "not a skin").unwrap();
        std::fs::write(b.join("one.json"), r#"{"name":"one"}"#).unwrap();
        std::fs::write(b.join("mocha.json"), r#"{"name":"catppuccin mocha"}"#).unwrap();
        std::fs::write(b.join("two.json"), "{}").unwrap();
        let (skins, notes) = discover(&[a, b, root.join("missing")]);
        let names: Vec<_> = skins.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["One", "two"]);
        assert_eq!(skins[0].tokens.len(), 1, "the earlier folder's One");
        assert_eq!(notes.len(), 3, "{notes:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_checked_out_themes_load_without_a_note_and_are_complete() {
        // The OpenFadeR-Themes checkout in `extension/skin/` (start.sh fetches it); nothing to check without it. Its CI runs this.
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/extension/skin"));
        if !dir.is_dir() {
            return;
        }
        let (skins, notes) = discover(&[dir]);
        assert!(notes.is_empty(), "{notes:?}");
        for s in &skins {
            // a theme with metadata lists every token; the small examples without an author list a few
            if !s.author.is_empty() {
                assert_eq!(s.tokens.len(), TOKENS.len(), "{}: every token", s.name);
                assert!(!s.description.is_empty(), "{}: description", s.name);
            }
            let mut p = crate::app::prefs::Prefs::default();
            let bad = crate::app::prefs::view::apply_layout(&mut p, &s.layout);
            assert!(bad.is_empty(), "{}: layout {bad:?}", s.name);
        }
    }

    #[test]
    fn author_description_and_layout_parse_and_bad_hints_are_noted() {
        let (skin, notes) = parse(
            r#"{"author":" me ","description":"d","layout":{"deck_layout":"vertical","decks":2,"browse_bold":true,"nope":"x","line_space":["a"]}}"#,
            "x",
        )
        .unwrap();
        assert_eq!(
            (skin.author.as_str(), skin.description.as_str()),
            ("me", "d")
        );
        assert_eq!(
            skin.layout,
            [
                ("browse_bold".to_owned(), "true".to_owned()),
                ("deck_layout".to_owned(), "vertical".to_owned()),
                ("decks".to_owned(), "2".to_owned()),
            ]
        );
        assert_eq!(notes.len(), 2, "{notes:?}");
        let (skin, notes) = parse(r#"{"author":3,"layout":"vertical"}"#, "x").unwrap();
        assert!(skin.author.is_empty() && skin.layout.is_empty());
        assert_eq!(notes.len(), 2, "{notes:?}");
        let (skin, notes) = parse(
            r##"{"name":"Old","base":"light","tokens":{"accent":"#123456"}}"##,
            "x",
        )
        .unwrap();
        assert!(
            notes.is_empty() && skin.layout.is_empty(),
            "a file from before layout hints loads unchanged"
        );
    }

    #[test]
    fn serialize_writes_what_parse_reads() {
        let skin = Skin {
            author: "A \"quoted\" name".into(),
            description: "line\nbreak".into(),
            layout: vec![("deck_layout".into(), "vertical".into())],
            ..Skin::new(
                "Tokyo \"Night\"",
                Base::Light,
                vec![
                    (
                        "scrim",
                        Value::Color(Color::from_argb_u8(0x44, 0x11, 0x22, 0x33)),
                    ),
                    ("accent", Value::Color(Color::from_rgb_u8(1, 2, 3))),
                    ("key-sat", Value::Float(0.25)),
                    ("font", Value::Str("Inter \\ Sans".into())),
                    ("font-size", Value::Length(10.5)),
                    ("weight", Value::Int(500)),
                    ("gap", Value::Length(3.0)),
                    ("t-fast", Value::Duration(120)),
                ],
            )
        };
        let text = serialize(&skin);
        let (back, notes) = parse(&text, "x").unwrap();
        assert!(notes.is_empty(), "{notes:?}\n{text}");
        assert_eq!(back, skin, "{text}");
        assert!(
            text.contains("\"gap\": 3,\n"),
            "whole numbers have no decimals: {text}"
        );
    }

    #[test]
    fn typed_text_is_read_like_the_file_forms_and_shown_back() {
        let c = |s| from_text(Ty::Color, s);
        assert_eq!(c("#00b4d8"), c("00b4d8"));
        assert!(c("00b4d").is_err() && c("red").is_err());
        assert_eq!(from_text(Ty::Length, " 12px "), Ok(Value::Length(12.0)));
        assert_eq!(from_text(Ty::Length, "12.5"), Ok(Value::Length(12.5)));
        assert!(from_text(Ty::Length, "big").is_err());
        assert_eq!(from_text(Ty::Int, "400"), Ok(Value::Int(400)));
        assert!(from_text(Ty::Int, "400.5").is_err());
        assert_eq!(from_text(Ty::Duration, "0.2s"), Ok(Value::Duration(200)));
        assert_eq!(from_text(Ty::Duration, "80"), Ok(Value::Duration(80)));
        assert_eq!(
            from_text(Ty::Str, " Inter "),
            Ok(Value::Str(" Inter ".into()))
        );
        assert_eq!(show(&Value::Duration(80)), "80ms");
        assert_eq!(show(&Value::Length(11.0)), "11");
        assert_eq!(
            show(&Value::Color(Color::from_argb_u8(0x80, 255, 0, 0))),
            "#ff000080"
        );
    }
}
