//! The UI language (PREFS > VIEW > LANGUAGE): English, the source text, and the translations in ui/translations.
//!
//! The `.slint` files mark their texts `@tr("…")`; slint-build bundles the `.po` files and switches them with
//! [`slint::select_bundled_translation`]. Texts the app writes from Rust go through [`tr!`] (and [`tr_n!`] for a count, `n_!` for
//! a text in a table that is translated where it is shown, by [`tr_dyn`]), which read the same `.po` files. Both use Slint's
//! placeholders: `{}` in order, `{0}`, `{1}` by position, `{n}` the count of a plural; `{{` and `}}` are braces. Numbers are
//! formatted before they are passed (`tr!("{} BPM", format!("{bpm:.2}"))`).
//!
//! `python3 ui/translations/extract.py` collects every marked text into `openfader-ui.pot` and adds the new ones to each
//! language's `.po` file, where a missing translation shows the English text.

use std::collections::HashMap;
use std::fmt::{Display, Write};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Code (the folder under ui/translations, also the language of Slint's bundle) and own name of each language; English first.
pub const LANGS: [(&str, &str); 7] = [
    ("en", "English"),
    ("fr", "Français"),
    ("es", "Español"),
    ("de", "Deutsch"),
    ("pt", "Português"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
];

/// The `.po` files of `LANGS[1..]`.
const PO: [&str; 6] = [
    include_str!("../../ui/translations/fr/LC_MESSAGES/openfader-ui.po"),
    include_str!("../../ui/translations/es/LC_MESSAGES/openfader-ui.po"),
    include_str!("../../ui/translations/de/LC_MESSAGES/openfader-ui.po"),
    include_str!("../../ui/translations/pt/LC_MESSAGES/openfader-ui.po"),
    include_str!("../../ui/translations/it/LC_MESSAGES/openfader-ui.po"),
    include_str!("../../ui/translations/nl/LC_MESSAGES/openfader-ui.po"),
];

/// Index into [`LANGS`] of the language shown.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// msgid → its translations (one, or the plural forms); an untranslated entry is left out.
type Catalog = HashMap<&'static str, Vec<&'static str>>;

static CATALOGS: LazyLock<[Catalog; 6]> = LazyLock::new(|| PO.map(parse));

fn catalogs() -> &'static [Catalog; 6] {
    &CATALOGS
}

/// Shows `lang` (a [`LANGS`] code, or anything else for the system's language when it is one of them, else English).
pub fn apply(lang: &str) {
    let i = index(lang)
        .or_else(|| sys_locale::get_locale().and_then(|l| index(base(&l))))
        .unwrap_or(0);
    CURRENT.store(i, Ordering::Relaxed);
    if let Err(e) = slint::select_bundled_translation(LANGS[i].0) {
        eprintln!("language {}: {e}", LANGS[i].0);
    }
}

fn index(code: &str) -> Option<usize> {
    LANGS.iter().position(|(c, _)| *c == code)
}

/// "pt-BR" / "fr_FR.UTF-8" → "pt" / "fr".
fn base(locale: &str) -> &str {
    locale.split(['-', '_', '.', '@']).next().unwrap_or(locale)
}

/// The shown translation of `s`, or `s`.
pub fn tr_dyn(s: &str) -> &str {
    lookup(s).and_then(|v| v.first().copied()).unwrap_or(s)
}

fn lookup(s: &str) -> Option<&'static Vec<&'static str>> {
    match CURRENT.load(Ordering::Relaxed) {
        0 => None,
        i => catalogs()[i - 1].get(s),
    }
}

/// [`tr_dyn`] for a literal ([`tr!`]).
pub fn tr(s: &'static str) -> &'static str {
    lookup(s).and_then(|v| v.first().copied()).unwrap_or(s)
}

/// The form of `one` / `other` for the count `n` in the shown language ([`tr_n!`]).
pub fn tr_n(one: &'static str, other: &'static str, n: i64) -> &'static str {
    let i = CURRENT.load(Ordering::Relaxed);
    let form = plural(LANGS[i].0, n);
    match lookup(one) {
        Some(forms) => forms.get(form).or(forms.last()).copied().unwrap_or(one),
        None if form == 0 => one,
        None => other,
    }
}

/// gettext's plural form index (the `Plural-Forms` of each `.po` header): French and Portuguese count 0 as one.
fn plural(lang: &str, n: i64) -> usize {
    match lang {
        "fr" | "pt" => (n > 1) as usize,
        _ => (n != 1) as usize,
    }
}

/// Fills Slint-style placeholders: `{}` takes the next argument, `{0}`… a given one, `{n}` is `n`; `{{` / `}}` are braces.
pub fn fmt(template: &str, n: Option<i64>, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let (mut next, mut rest) = (0, template);
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        let name = tail
            .find('}')
            .filter(|_| tail.starts_with('{'))
            .map(|end| (end, &tail[1..end]));
        let Some((end, name)) = name
            .filter(|(_, n)| n.is_empty() || *n == "n" || n.bytes().all(|b| b.is_ascii_digit()))
        else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
            continue;
        };
        let arg = match name {
            "" => {
                next += 1;
                args.get(next - 1)
            }
            "n" => {
                if let Some(n) = n {
                    let _ = write!(out, "{n}");
                }
                None
            }
            i => i.parse::<usize>().ok().and_then(|i| args.get(i)),
        };
        if let Some(a) = arg {
            let _ = write!(out, "{a}");
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A `.po` file's entries (gettext's quoting: `\"`, `\\`, `\n`, `\t`; strings split over lines are joined).
fn parse(po: &'static str) -> Catalog {
    let mut cat = Catalog::new();
    let (mut id, mut strs, mut field): (String, Vec<String>, u8) = (String::new(), Vec::new(), 0);
    let mut flush = |id: &mut String, strs: &mut Vec<String>| {
        if !id.is_empty() && strs.iter().all(|s| !s.is_empty()) && !strs.is_empty() {
            let v = strs.drain(..).map(|s| &*s.leak()).collect();
            cat.insert(&*std::mem::take(id).leak(), v);
        }
        id.clear();
        strs.clear();
    };
    for line in po.lines().map(str::trim) {
        let quoted = |l: &str| l.find('"').map(|q| unquote(&l[q..]));
        if line.starts_with("msgid ") {
            flush(&mut id, &mut strs);
            id = quoted(line).unwrap_or_default();
            field = 1;
        } else if line.starts_with("msgid_plural") {
            field = 0;
        } else if line.starts_with("msgstr") {
            strs.push(quoted(line).unwrap_or_default());
            field = 2;
        } else if line.starts_with('"') {
            match field {
                1 => id.push_str(&unquote(line)),
                2 => strs.last_mut().unwrap().push_str(&unquote(line)),
                _ => {}
            }
        }
    }
    flush(&mut id, &mut strs);
    cat
}

fn unquote(q: &str) -> String {
    let q = q.trim();
    let inner = q
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(q);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

/// A translated text: `tr!("Saved")` → `&'static str`; with arguments `tr!("{} tracks", n)` → `String` ([`fmt`]).
macro_rules! tr {
    ($s:literal) => {
        $crate::app::i18n::tr($s)
    };
    ($s:literal, $($a:expr),+ $(,)?) => {
        $crate::app::i18n::fmt($crate::app::i18n::tr($s), None, &[$(&$a as &dyn ::std::fmt::Display),+])
    };
}

/// A text with a count: `tr_n!("{n} track", "{n} tracks", n)` (more arguments fill `{}` / `{0}`…) → `String`.
macro_rules! tr_n {
    ($one:literal, $other:literal, $n:expr $(, $a:expr)* $(,)?) => {{
        let n = $n as i64;
        $crate::app::i18n::fmt($crate::app::i18n::tr_n($one, $other, n), Some(n), &[$(&$a as &dyn ::std::fmt::Display),*])
    }};
}

/// Marks a text in a table for translation where it is shown ([`tr_dyn`]); the text itself is unchanged.
macro_rules! n_ {
    ($s:literal) => {
        $s
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_fill_in_order_by_position_and_count() {
        assert_eq!(fmt("{} of {}", None, &[&1, &"two"]), "1 of two");
        assert_eq!(fmt("{1} before {0}", None, &[&"a", &"b"]), "b before a");
        assert_eq!(fmt("{n} tracks in {}", Some(3), &[&"x"]), "3 tracks in x");
        assert_eq!(fmt("{{literal}} {}", None, &[&5]), "{literal} 5");
        assert_eq!(
            fmt("unclosed { and } stay", None, &[]),
            "unclosed { and } stay"
        );
    }

    #[test]
    fn po_entries_join_lines_unescape_and_skip_untranslated() {
        let po = "msgid \"\"\nmsgstr \"Content-Type: text/plain\\n\"\n\nmsgid \"Say \\\"hi\\\"\"\nmsgstr \"Dis \"\n\"\\\"salut\\\"\"\n\nmsgid \"untranslated\"\nmsgstr \"\"\n\nmsgid \"{n} track\"\nmsgid_plural \"{n} tracks\"\nmsgstr[0] \"{n} piste\"\nmsgstr[1] \"{n} pistes\"\n";
        let c = parse(po);
        assert_eq!(c["Say \"hi\""], vec!["Dis \"salut\""]);
        assert!(!c.contains_key("untranslated"));
        assert!(!c.contains_key(""), "the header is not a text");
        assert_eq!(c["{n} track"], vec!["{n} piste", "{n} pistes"]);
    }

    /// Every language translates every text of the template, with the same placeholders.
    #[test]
    fn every_language_translates_every_text_with_its_placeholders() {
        let pot = parse_ids(include_str!("../../ui/translations/openfader-ui.pot"));
        let marks = |s: &str| {
            let mut m: Vec<String> = Vec::new();
            let mut rest = s.replace("{{", "").replace("}}", "");
            while let Some(a) = rest.find('{') {
                let Some(b) = rest[a..].find('}') else { break };
                m.push(rest[a..a + b + 1].to_string());
                rest = rest[a + b + 1..].to_string();
            }
            m.sort();
            m
        };
        for (i, cat) in catalogs().iter().enumerate() {
            let lang = LANGS[i + 1].0;
            let missing: Vec<&String> = pot
                .iter()
                .filter(|id| !cat.contains_key(id.as_str()))
                .collect();
            assert!(
                missing.is_empty(),
                "{lang}: {} untranslated, first {:?}",
                missing.len(),
                &missing[..missing.len().min(5)]
            );
            for (id, forms) in cat {
                for f in forms {
                    // a singular form may spell out the count ("une piste") instead of `{n}`
                    let (want, got) = (marks(id), marks(f));
                    let without_n = |m: &[String]| {
                        m.iter()
                            .filter(|x| *x != "{n}")
                            .cloned()
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(
                        without_n(&want),
                        without_n(&got),
                        "{lang}: placeholders of {id:?} in {f:?}"
                    );
                }
            }
        }
    }

    fn parse_ids(pot: &'static str) -> Vec<String> {
        let mut ids = Vec::new();
        let mut cur: Option<String> = None;
        for line in pot.lines().map(str::trim) {
            if let Some(q) = line.strip_prefix("msgid ") {
                ids.extend(cur.take().filter(|s| !s.is_empty()));
                cur = Some(unquote(q));
            } else if line.starts_with('"') {
                if let Some(c) = cur.as_mut() {
                    c.push_str(&unquote(line));
                }
            } else if !line.is_empty() && !line.starts_with('#') {
                ids.extend(cur.take().filter(|s| !s.is_empty()));
            }
        }
        ids.extend(cur.filter(|s| !s.is_empty()));
        ids
    }

    #[test]
    fn plural_forms_follow_each_language() {
        assert_eq!(
            (plural("fr", 0), plural("fr", 1), plural("fr", 2)),
            (0, 0, 1)
        );
        assert_eq!(
            (plural("de", 0), plural("de", 1), plural("de", 2)),
            (1, 0, 1)
        );
    }
}
