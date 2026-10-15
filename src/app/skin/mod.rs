//! Skins (themes): JSON files that restyle the whole UI at run time (VIEW > Color > Skin and PREFS > THEMES; format and token list in
//! docs/skins.md).
//!
//! A skin picks a built-in base (dark or light) and overrides any of the skin tokens, which are the `in-out` properties of the
//! `Theme` global in ui/theme.slint (`TOKENS`, generated from that file with the UI, so a new token needs no edit here). [`apply`] sets
//! EVERY token of a window, to the skin's value or else the base's built-in one, so switching skins back and forth is exact. The
//! built-in values are read once from the first window, before any override, by evaluating the `light ? … : …` defaults of
//! theme.slint for both bases. Windows have globals of their own, so the split-screen window is applied separately
//! ([`crate::app::prefs`] `apply_theme` does both). The colours Rust itself draws (waveforms, cue marker) are handed on to
//! [`render::set_palette`].
//!
//! The skin folders are read on demand: [`rescan`] after a file was added, edited or removed (the watcher in [`wire`] does it by itself
//! within a second), [`install_and_choose`] copies a theme file into the user's folder and applies it. A skin may carry `layout` hints, VIEW settings
//! ([`file::LAYOUT_KEYS`]) that choosing it sets ([`layout_of`], applied by `prefs::view::select_skin`).
//!
//! Children: [`file`] (skin files: discovery, parsing, writing), [`store`] (the user's skin folder: install, save, delete),
//! [`draft`] (the theme editor's working copy, applied live to every window instead of the chosen skin), [`page`] (PREFS > THEMES
//! ↔ ui/themes.slint `ThemeState`). The token table and typed access are generated with the UI (`openfader_ui::skin`).

mod draft;
mod file;
mod page;
mod store;

use crate::app::{Theme, prefs, render};
use openfader_ui::skin::{TOKEN_INFO, TOKENS, get, set};
pub use openfader_ui::skin::{Ty, Value};
use render::{Palette, Rgb};
use slint::ComponentHandle;
use std::{cell::OnceCell, cell::RefCell, path::PathBuf, rc::Rc};

pub use page::{install_and_choose, wire};

/// The names of the two built-in skins (the dark and the light base), which skin files cannot take. The first is the default.
pub const BUILTIN: [&str; 2] = ["Catppuccin Mocha", "Catppuccin Latte"];

/// The built-in values a skin starts from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Base {
    Dark,
    Light,
}

/// A skin file's content: the tokens it sets (in [`TOKENS`] names), everything else is the base's.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    pub name: String,
    pub base: Base,
    pub tokens: Vec<(&'static str, Value)>,
    pub author: String,
    pub description: String,
    /// VIEW settings the skin sets when it is chosen: `(setting, value)` as in the settings file ([`file::LAYOUT_KEYS`]).
    pub layout: Vec<(String, String)>,
    /// The file it was read from (`None`: made in memory).
    pub path: Option<PathBuf>,
}

impl Skin {
    pub fn new(name: &str, base: Base, tokens: Vec<(&'static str, Value)>) -> Skin {
        Skin {
            name: name.to_owned(),
            base,
            tokens,
            author: String::new(),
            description: String::new(),
            layout: Vec::new(),
            path: None,
        }
    }
}

thread_local! {
    /// The skins found in the skin folders (read on first use and again by [`rescan`]); UI thread only, like every window.
    static FOUND: RefCell<Option<Rc<Vec<Skin>>>> = const { RefCell::new(None) };
}

fn read_folders() -> Rc<Vec<Skin>> {
    let (skins, notes) = file::discover(&folders());
    notes.iter().for_each(|n| eprintln!("{n}"));
    Rc::new(skins)
}

/// The skin folders, earlier ones winning a name clash.
fn folders() -> Vec<PathBuf> {
    openfader::paths::search_path("OPENFADER_SKINS", "extension/skin")
}

fn found() -> Rc<Vec<Skin>> {
    FOUND.with_borrow_mut(|f| f.get_or_insert_with(read_folders).clone())
}

/// Reads the skin folders again and refreshes every list and picker that shows skins (VIEW > Color > Skin, the theme gallery); the
/// chosen skin stays chosen when it still exists, and is restyled from its file, so an edit shows at once.
pub fn rescan() {
    FOUND.with_borrow_mut(|f| *f = Some(read_folders()));
    prefs::refresh();
    page::refresh();
    prefs::notify();
}

/// The names the skin pickers offer: the two built-in ones, then the skin files'.
pub fn names() -> Vec<String> {
    BUILTIN
        .into_iter()
        .map(str::to_owned)
        .chain(found().iter().map(|s| s.name.clone()))
        .collect()
}

/// Where `name` is in [`names`] (case-insensitive); the first (the default) for a name that is not there.
pub fn position(name: &str) -> usize {
    names()
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .unwrap_or(0)
}

/// The VIEW settings choosing the skin `name` sets (empty for the built-in skins and an unknown name).
pub fn layout_of(name: &str) -> Vec<(String, String)> {
    found()
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .map(|s| s.layout.clone())
        .unwrap_or_default()
}

/// The base and overrides `name` stands for in `skins` (`None`: no such skin).
fn lookup<'a>(skins: &'a [Skin], name: &str) -> Option<(Base, &'a [(&'static str, Value)])> {
    let is = |n: &str| n.eq_ignore_ascii_case(name);
    if is(BUILTIN[0]) {
        Some((Base::Dark, &[]))
    } else if is(BUILTIN[1]) {
        Some((Base::Light, &[]))
    } else {
        skins
            .iter()
            .find(|s| is(&s.name))
            .map(|s| (s.base, &s.tokens[..]))
    }
}

/// The values of every token in each base, in [`TOKENS`] order.
struct Builtin {
    dark: Vec<Value>,
    light: Vec<Value>,
}

impl Builtin {
    /// Reads them from `w` while none of its tokens has been set yet, by flipping `light`.
    fn read<'a, C: ComponentHandle>(w: &'a C) -> Builtin
    where
        Theme<'a>: slint::Global<'a, C>,
    {
        let both = [false, true].map(|light| {
            w.global::<Theme>().set_light(light);
            read_all(w)
        });
        let [dark, light] = both;
        Builtin { dark, light }
    }

    fn of(&self, base: Base) -> &[Value] {
        match base {
            Base::Dark => &self.dark,
            Base::Light => &self.light,
        }
    }
}

/// Every token of `w`, in [`TOKENS`] order.
fn read_all<'a, C: ComponentHandle>(w: &'a C) -> Vec<Value>
where
    Theme<'a>: slint::Global<'a, C>,
{
    TOKENS
        .iter()
        .map(|(n, _)| get(w, n).expect("a token of TOKENS"))
        .collect()
}

/// The value of every token (in [`TOKENS`] order) under a skin: its override, else the base's built-in value.
fn plan(builtin: &Builtin, base: Base, overrides: &[(&str, Value)]) -> Vec<Value> {
    TOKENS
        .iter()
        .zip(builtin.of(base))
        .map(|((name, _), default)| {
            overrides
                .iter()
                .find(|(n, _)| n == name)
                .map_or(default, |(_, v)| v)
                .clone()
        })
        .collect()
}

/// The base and every token value of the skin `name` among `skins` (`None`: no such skin, or no window has been styled yet).
fn values_of(skins: &[Skin], name: &str) -> Option<(Base, Vec<Value>)> {
    let (base, overrides) = lookup(skins, name)?;
    BUILTIN_VALUES.with(|b| Some((base, plan(b.get()?, base, overrides))))
}

/// The built-in value of every token under `base` (empty before the first window is styled).
fn defaults(base: Base) -> Vec<Value> {
    BUILTIN_VALUES.with(|b| b.get().map(|b| b.of(base).to_vec()).unwrap_or_default())
}

/// `values` (in [`TOKENS`] order) as the tokens of a skin.
fn complete(values: Vec<Value>) -> Vec<(&'static str, Value)> {
    TOKENS.iter().map(|t| t.0).zip(values).collect()
}

/// The colours Rust draws with, out of the token values `values` (a missing or non-colour token keeps the built-in colour).
fn palette(values: &[Value]) -> Palette {
    let rgb = |name: &str, default: Rgb| {
        TOKENS
            .iter()
            .position(|(n, _)| *n == name)
            .and_then(|i| match &values[i] {
                Value::Color(c) => Some([c.red(), c.green(), c.blue()]),
                _ => None,
            })
            .unwrap_or(default)
    };
    let d = Palette::DEFAULT;
    Palette {
        bg: rgb("wave-bg", d.bg),
        band: [
            rgb("wave-low", d.band[0]),
            rgb("wave-mid", d.band[1]),
            rgb("wave-high", d.band[2]),
        ],
        cue: rgb("cue", d.cue),
    }
}

thread_local! {
    /// Taken from the first window [`apply`] sees (UI thread only, like every window).
    static BUILTIN_VALUES: OnceCell<Builtin> = const { OnceCell::new() };
    /// The skin name last reported missing, so a settings change does not repeat the message.
    static REPORTED: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Styles `w`'s `Theme` with the skin called `name` (VIEW > Color > Skin; an unknown name is reported once and reads as the default) and
/// hands the colours Rust draws on to the renderers. Sets every token, so the result does not depend on the skin before. While the
/// theme editor has a draft open ([`draft`]) the draft is what every window shows.
pub fn apply<'a, C: ComponentHandle>(w: &'a C, name: &str)
where
    Theme<'a>: slint::Global<'a, C>,
    crate::app::Palette<'a>: slint::Global<'a, C>,
{
    let values = restyle(w, &found(), name);
    render::set_palette(palette(&values));
}

/// Sets every token of `w` for the skin `name` among `skins` (or the open draft) and returns the values set, in [`TOKENS`] order.
fn restyle<'a, C: ComponentHandle>(w: &'a C, skins: &[Skin], name: &str) -> Vec<Value>
where
    Theme<'a>: slint::Global<'a, C>,
    crate::app::Palette<'a>: slint::Global<'a, C>,
{
    BUILTIN_VALUES.with(|b| {
        b.get_or_init(|| Builtin::read(w));
    });
    let (base, values) = draft::current()
        .or_else(|| {
            if lookup(skins, name).is_none()
                && !["dark", "light"]
                    .iter()
                    .any(|o| o.eq_ignore_ascii_case(name))
            {
                REPORTED.with_borrow_mut(|last| {
                    if *last != name {
                        eprintln!(
                            "skin {name:?} not found in the skin folders, using {}",
                            BUILTIN[0]
                        );
                        *last = name.to_owned();
                    }
                });
            }
            values_of(skins, name).or_else(|| values_of(skins, BUILTIN[0]))
        })
        .expect("the built-in values were just read");
    w.global::<Theme>().set_light(base == Base::Light);
    // The std widgets (the menu bar) draw from Slint's own palette, not from `Theme`.
    let scheme = if base == Base::Light {
        slint::language::ColorScheme::Light
    } else {
        slint::language::ColorScheme::Dark
    };
    w.global::<crate::app::Palette>().set_color_scheme(scheme);
    for ((token, _), v) in TOKENS.iter().zip(&values) {
        set(w, token, v);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, headless};
    use slint::Color;

    fn skin(name: &str, base: Base, tokens: &[(&'static str, Value)]) -> Skin {
        Skin::new(name, base, tokens.to_vec())
    }

    fn color(r: u8, g: u8, b: u8) -> Value {
        Value::Color(Color::from_rgb_u8(r, g, b))
    }

    fn window() -> App {
        headless::init();
        App::new().unwrap()
    }

    #[test]
    fn the_table_lists_the_skin_tokens_and_not_the_pref_driven_properties() {
        let ty = |n: &str| TOKENS.iter().find(|t| t.0 == n).map(|t| t.1);
        assert_eq!(ty("accent"), Some(Ty::Color));
        assert_eq!(ty("font-size"), Some(Ty::Length));
        assert_eq!(ty("font"), Some(Ty::Str));
        assert_eq!(ty("weight"), Some(Ty::Int));
        assert_eq!(ty("key-sat"), Some(Ty::Float));
        assert_eq!(ty("t-fast"), Some(Ty::Duration));
        for pref in ["light", "tooltips", "h-row", "list-font", "list-weight"] {
            assert_eq!(ty(pref), None, "{pref} is set by the preferences");
        }
        let app = window();
        assert!(get(&app, "light").is_none());
        assert!(!set(&app, "accent", &Value::Length(1.0)), "wrong type");
        assert!(!set(&app, "nope", &Value::Length(1.0)));
    }

    #[test]
    fn every_token_has_an_editor_group_and_the_groups_run_in_one_piece() {
        assert_eq!(TOKEN_INFO.len(), TOKENS.len());
        let mut seen: Vec<&str> = Vec::new();
        for (group, _) in TOKEN_INFO {
            assert!(!group.is_empty(), "a token before the first `// @group`");
            if seen.last() != Some(group) {
                assert!(
                    !seen.contains(group),
                    "group {group} is split in theme.slint"
                );
                seen.push(group);
            }
        }
        let at = |n: &str| TOKENS.iter().position(|t| t.0 == n).unwrap();
        assert_eq!(TOKEN_INFO[at("accent")].0, "Accents");
        assert!(TOKEN_INFO[at("bg")].1.contains("Window"));
    }

    #[test]
    fn overrides_win_over_the_base_and_unlisted_tokens_keep_the_bases_value() {
        let app = window();
        let s = [skin(
            "A",
            Base::Light,
            &[
                ("accent", color(1, 2, 3)),
                ("font-size", Value::Length(15.0)),
            ],
        )];
        restyle(&app, &[], BUILTIN[0]);
        let dark = read_all(&app);
        restyle(&app, &[], BUILTIN[1]);
        let light = read_all(&app);
        assert_ne!(dark, light);
        restyle(&app, &s, "a");
        let got = read_all(&app);
        for (i, (name, _)) in TOKENS.iter().enumerate() {
            let want = match *name {
                "accent" => &color(1, 2, 3),
                "font-size" => &Value::Length(15.0),
                _ => &light[i],
            };
            assert_eq!(&got[i], want, "{name}");
        }
    }

    #[test]
    fn switching_skins_back_and_forth_restores_the_built_in_values_exactly() {
        let app = window();
        let skins = [
            skin(
                "A",
                Base::Dark,
                &[
                    ("accent", color(1, 2, 3)),
                    ("font", Value::Str("Inter".into())),
                    ("t-fast", Value::Duration(5)),
                    ("pad-style", Value::Int(1)),
                    ("key-sat", Value::Float(0.9)),
                ],
            ),
            skin(
                "B",
                Base::Light,
                &[("bg", color(9, 9, 9)), ("weight", Value::Int(300))],
            ),
        ];
        restyle(&app, &skins, BUILTIN[0]);
        let dark = read_all(&app);
        restyle(&app, &skins, BUILTIN[1]);
        let light = read_all(&app);
        for name in ["A", "B", "a", BUILTIN[1], "B", "A", BUILTIN[0]] {
            restyle(&app, &skins, name);
        }
        assert_eq!(read_all(&app), dark);
        restyle(&app, &skins, "B");
        restyle(&app, &skins, BUILTIN[1]);
        assert_eq!(read_all(&app), light);
        // a second window (the split screen) styled with the same skin has the same values
        let other = window();
        restyle(&other, &skins, "A");
        restyle(&app, &skins, "A");
        assert_eq!(read_all(&other), read_all(&app));
    }

    #[test]
    fn a_missing_skin_reads_as_dark() {
        let app = window();
        restyle(&app, &[], BUILTIN[0]);
        let dark = read_all(&app);
        restyle(&app, &[], BUILTIN[1]);
        restyle(&app, &[], "gone");
        assert_eq!(read_all(&app), dark);
        assert!(lookup(&[], "gone").is_none());
        assert_eq!(position("gone"), 0);
    }

    #[test]
    fn the_renderers_follow_the_skins_waveform_colours() {
        let app = window();
        let skins = [skin(
            "A",
            Base::Dark,
            &[
                ("wave-low", color(1, 2, 3)),
                ("wave-bg", color(4, 5, 6)),
                ("cue", color(7, 8, 9)),
            ],
        )];
        restyle(&app, &skins, BUILTIN[0]);
        let builtin = palette(&read_all(&app));
        assert_eq!(
            builtin,
            Palette::DEFAULT,
            "the built-in skins draw what the renderers always drew"
        );
        restyle(&app, &skins, "A");
        let p = palette(&read_all(&app));
        assert_eq!((p.band[0], p.bg, p.cue), ([1, 2, 3], [4, 5, 6], [7, 8, 9]));
        assert_eq!(p.band[1..], Palette::DEFAULT.band[1..]);
        restyle(&app, &skins, BUILTIN[0]);
    }

    #[test]
    fn the_skin_doc_lists_every_token() {
        let doc = include_str!("../../../docs/skins.md");
        let missing: Vec<_> = TOKENS
            .iter()
            .filter(|(n, _)| !doc.contains(&format!("`{n}`")))
            .map(|t| t.0)
            .collect();
        assert!(missing.is_empty(), "docs/skins.md lacks {missing:?}");
    }
}
