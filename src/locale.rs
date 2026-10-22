//! The language extensions are shown in. A plugin folder, or a folder of themes, may hold `locales/<lang>.json`: one object,
//! English text → its translation (`{"Download from URL…": "Télécharger depuis une URL…"}`), `<lang>` a code of the app's
//! languages (`fr`, `es`, `de`, `pt`, `it`, `nl`). [`tr`] shows a text the extension gave (its `plugin.json`, a reply, a
//! theme's description) in the language [`set`] by the app; a text the file lacks stays as it is. A key may hold `{}`
//! placeholders, so a text the extension builds at run time is translated too: `"{} tracks exported": "{} pistes exportées"`
//! turns "12 tracks exported" into "12 pistes exportées"; the translation takes the values in order with `{}`, or by
//! position with `{0}`, `{1}`. Every request a plugin gets also carries the code as `"lang"` ([`code`]), for a plugin that
//! translates itself.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

/// A `locales/<lang>.json`: the plain texts, and the keys with placeholders split at them.
#[derive(Default)]
struct Catalog {
    exact: HashMap<String, String>,
    templates: Vec<(Vec<String>, String)>,
}

struct State {
    /// "" for English.
    code: String,
    catalogs: HashMap<PathBuf, Arc<Catalog>>,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| {
    RwLock::new(State {
        code: String::new(),
        catalogs: HashMap::new(),
    })
});

/// Shows extensions in `code` ("en" or "" for English, the texts as written).
pub fn set(code: &str) {
    let code = if code == "en" { "" } else { code };
    let mut s = STATE.write();
    if s.code != code {
        s.code = code.to_string();
        s.catalogs.clear();
    }
}

/// The language set, `"en"` for English: the `"lang"` of every request.
pub fn code() -> String {
    let s = STATE.read();
    if s.code.is_empty() {
        "en".into()
    } else {
        s.code.clone()
    }
}

/// `text`, given by the extension in `dir` (a plugin folder, or the folder of a theme file), in the language set.
pub fn tr(dir: &Path, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let catalog = {
        let s = STATE.read();
        if s.code.is_empty() {
            return text.to_string();
        }
        match s.catalogs.get(dir) {
            Some(c) => c.clone(),
            None => {
                let code = s.code.clone();
                drop(s);
                let c = Arc::new(load(&dir.join("locales").join(format!("{code}.json"))));
                STATE.write().catalogs.insert(dir.to_path_buf(), c.clone());
                c
            }
        }
    };
    catalog.tr(text)
}

impl Catalog {
    /// `text` translated, or as it is.
    fn tr(&self, text: &str) -> String {
        if let Some(t) = self.exact.get(text) {
            return t.clone();
        }
        self.templates
            .iter()
            .find_map(|(parts, t)| captures(parts, text).map(|values| fill(t, &values)))
            .unwrap_or_else(|| text.to_string())
    }
}

/// `english` in the language set, from `(code, translation)` pairs read ahead (a theme's description, a repository plugin's).
pub fn pick(translations: &[(String, String)], english: &str) -> String {
    let s = STATE.read();
    translations
        .iter()
        .find(|(c, _)| *c == s.code)
        .map_or(english, |(_, t)| t)
        .to_string()
}

/// A missing or broken file is an empty catalog (the texts stay English); a broken one is reported on stderr.
fn load(path: &Path) -> Catalog {
    let Ok(json) = std::fs::read_to_string(path) else {
        return Catalog::default();
    };
    let map: HashMap<String, String> = match serde_json::from_str(&json) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return Catalog::default();
        }
    };
    let mut c = Catalog::default();
    for (k, v) in map {
        if v.is_empty() {
            continue;
        }
        if k.contains("{}") {
            c.templates
                .push((k.split("{}").map(str::to_string).collect(), v));
        } else {
            c.exact.insert(k, v);
        }
    }
    // The most specific key first: the one with the most fixed text.
    c.templates
        .sort_by_key(|(parts, _)| std::cmp::Reverse(parts.iter().map(String::len).sum::<usize>()));
    c
}

/// The values standing at the placeholders when `text` is `parts` joined by them, each at least one character.
fn captures<'a>(parts: &[String], text: &'a str) -> Option<Vec<&'a str>> {
    let (first, rest) = parts.split_first()?;
    let (last, middle) = rest.split_last()?;
    let mut s = text
        .strip_prefix(first.as_str())?
        .strip_suffix(last.as_str())?;
    let mut values = Vec::with_capacity(rest.len());
    for part in middle {
        // The value ends at the first occurrence of the next fixed text after at least one character.
        let start = s.char_indices().nth(1).map_or(s.len(), |(i, _)| i);
        let at = if part.is_empty() {
            start
        } else {
            start + s.get(start..)?.find(part.as_str())?
        };
        values.push(&s[..at]);
        s = &s[at + part.len()..];
    }
    if s.is_empty() {
        return None;
    }
    values.push(s);
    values.iter().all(|v| !v.is_empty()).then_some(values)
}

/// `template` with `{}` replaced by `values` in order, `{0}`, `{1}`… by position.
fn fill(template: &str, values: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}');
        let value = close.and_then(|c| match &after[..c] {
            "" => {
                next += 1;
                values.get(next - 1)
            }
            i => i.parse::<usize>().ok().and_then(|i| values.get(i)),
        });
        match (value, close) {
            (Some(v), Some(c)) => {
                out.push_str(v);
                rest = &after[c + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_plain_and_built_texts_and_leaves_the_rest() {
        // The global language stays English: other tests read it in the requests they check.
        let file =
            std::env::temp_dir().join(format!("openfader-locale-{}.json", std::process::id()));
        std::fs::write(
            &file,
            r#"{"Download": "Télécharger", "{} tracks exported": "{} pistes exportées",
                "{} of {} done": "{1} : {0} terminés", "Skipped {}": "", "{} to {}": "{} vers {}"}"#,
        )
        .unwrap();
        let c = load(&file);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(c.tr("Download"), "Télécharger");
        assert_eq!(c.tr("12 tracks exported"), "12 pistes exportées");
        assert_eq!(c.tr("3 of 10 done"), "10 : 3 terminés");
        assert_eq!(c.tr("A to B to C"), "A vers B to C");
        assert_eq!(
            c.tr(" tracks exported"),
            " tracks exported",
            "a placeholder takes at least one character"
        );
        assert_eq!(
            c.tr("Skipped 2"),
            "Skipped 2",
            "an empty translation keeps the English"
        );
        assert_eq!(c.tr("Unknown"), "Unknown");
        assert_eq!(load(&file).tr("Download"), "Download", "no file");
        assert_eq!(code(), "en");
        assert_eq!(
            tr(&file, "Download"),
            "Download",
            "English shows the texts as written"
        );
    }
}
