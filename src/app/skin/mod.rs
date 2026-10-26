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
//! ↔ ui/themes.slint `ThemeState`), [`get`] (the themes of the repositories, installed from GET THEMES, `app/catalog`). The token table
//! and typed access are generated with the UI (`openfader_ui::skin`).

mod draft;
mod file;
pub mod get;
mod page;
mod store;

use crate::app::{prefs, render, Theme};
use openfader_ui::skin::{get, set, FOLLOWS, TOKENS, TOKEN_INFO, TOKEN_RULES};
pub use openfader_ui::skin::{Ty, Value};
use render::{Palette, Rgb};
use slint::ComponentHandle;
use std::{cell::OnceCell, cell::RefCell, path::PathBuf, rc::Rc};

pub use page::{install_and_choose, refresh, wire};

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
    /// The file's `"locales"`: language → {English text → translation}, kept as written ([`Skin::shown_description`]).
    pub locales: serde_json::Map<String, serde_json::Value>,
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
            locales: serde_json::Map::new(),
            path: None,
        }
    }

    /// The description in the UI language when `locales` translates it.
    pub fn shown_description(&self) -> String {
        self.locales
            .get(&openfader::locale::code())
            .and_then(|m| m.get(&self.description))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&self.description)
            .to_owned()
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

/// The value of every token (in [`TOKENS`] order) under a skin: its override, else the base's built-in value, except that a token
/// that [`FOLLOWS`] another one takes the value that other one ends up with.
fn plan(builtin: &Builtin, base: Base, overrides: &[(&str, Value)]) -> Vec<Value> {
    let mut values: Vec<Value> = TOKENS
        .iter()
        .zip(builtin.of(base))
        .map(|((name, _), default)| {
            overrides
                .iter()
                .find(|(n, _)| n == name)
                .map_or(default, |(_, v)| v)
                .clone()
        })
        .collect();
    for (token, target) in FOLLOWS {
        if !overrides.iter().any(|(n, _)| n == token) {
            let at = |n: &str| TOKENS.iter().position(|t| t.0 == n);
            if let (Some(i), Some(j)) = (at(token), at(target)) {
                values[i] = values[j].clone();
            }
        }
    }
    values
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
    let at = |name: &str| TOKENS.iter().position(|(n, _)| *n == name);
    let rgb = |name: &str, default: Rgb| {
        at(name)
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
        memory: rgb("memory-cue", d.memory),
        decks: std::array::from_fn(|n| {
            let (lo, hi) = (format!("deck{}", n + 1), format!("deck{}-hi", n + 1));
            [rgb(&lo, d.decks[n][0]), rgb(&hi, d.decks[n][1])]
        }),
        per_deck: at("wave-style").is_some_and(|i| values[i] == Value::Int(1)),
        fade: at("wave-fade").map_or(0.0, |i| match values[i] {
            Value::Float(f) => f.clamp(0.0, 1.0),
            _ => 0.0,
        }),
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
    use crate::app::{headless, App};
    use openfader_ui::skin::Rule;
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
        let set = |n: &str| match n {
            "accent" => Some(color(1, 2, 3)),
            "font-size" => Some(Value::Length(15.0)),
            _ => None,
        };
        for (i, (name, _)) in TOKENS.iter().enumerate() {
            // A token that follows an overridden one (`menu-focus` follows `accent`) takes the override too.
            let follows = FOLLOWS.iter().find(|f| f.0 == *name).and_then(|f| set(f.1));
            let want = set(name).or(follows).unwrap_or_else(|| light[i].clone());
            assert_eq!(got[i], want, "{name}");
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

    fn token(app: &App, name: &str) -> Value {
        get(app, name).unwrap()
    }

    #[test]
    fn a_token_that_follows_another_takes_the_skins_value_unless_the_skin_sets_it() {
        let app = window();
        let skins = [
            skin("Flat", Base::Light, &[("panel", color(1, 2, 3))]),
            skin(
                "Set",
                Base::Dark,
                &[("panel", color(1, 2, 3)), ("panel-top", color(9, 9, 9))],
            ),
        ];
        assert!(FOLLOWS.contains(&("panel-top", "panel")));
        restyle(&app, &skins, BUILTIN[0]);
        let (dark, dark_top) = (token(&app, "panel"), token(&app, "panel-top"));
        assert_eq!(dark, dark_top, "the built-in themes have a flat panel");
        for _ in 0..2 {
            restyle(&app, &skins, "Flat");
            assert_eq!(
                token(&app, "panel-top"),
                color(1, 2, 3),
                "follows the skin's panel"
            );
            restyle(&app, &skins, "Set");
            assert_eq!(
                token(&app, "panel-top"),
                color(9, 9, 9),
                "unless the skin sets it"
            );
            restyle(&app, &skins, BUILTIN[1]);
            assert_eq!(token(&app, "panel-top"), token(&app, "panel"));
            assert_ne!(token(&app, "panel-top"), dark_top);
        }
        restyle(&app, &skins, "Flat");
        restyle(&app, &skins, BUILTIN[0]);
        assert_eq!(
            token(&app, "panel-top"),
            dark_top,
            "switching back restores it"
        );
    }

    #[test]
    fn display_font_is_the_display_family_or_else_the_font() {
        let app = window();
        let skins = [
            skin("A", Base::Dark, &[("font", Value::Str("Inter".into()))]),
            skin(
                "B",
                Base::Dark,
                &[
                    ("font", Value::Str("Inter".into())),
                    ("font-display", Value::Str("Orbitron".into())),
                ],
            ),
        ];
        let shown = |name: &str| {
            restyle(&app, &skins, name);
            app.global::<Theme>().get_display_font().to_string()
        };
        assert_eq!(shown(BUILTIN[0]), "Liberation Sans");
        assert_eq!(shown("A"), "Inter");
        assert_eq!(shown("B"), "Orbitron");
        assert_eq!(shown("A"), "Inter");
    }

    #[test]
    fn the_built_in_values_obey_their_rules() {
        assert_eq!(TOKEN_RULES.len(), TOKENS.len());
        let app = window();
        for base in BUILTIN {
            restyle(&app, &[], base);
            for (i, v) in read_all(&app).iter().enumerate() {
                assert_eq!(file::check(i, v), Ok(()), "{} under {base}", TOKENS[i].0);
            }
        }
        let at = |n: &str| TOKENS.iter().position(|t| t.0 == n).unwrap();
        assert!(matches!(TOKEN_RULES[at("stage-deck")], Rule::Choice(n) if n.len() == 4));
        assert!(matches!(TOKEN_RULES[at("glow")], Rule::Range(lo, hi) if (lo, hi) == (0.0, 1.0)));
        assert_eq!(TOKEN_RULES[at("accent")], Rule::Any);
    }

    #[test]
    fn wave_style_one_gives_each_deck_its_own_bands_and_wave_fade_reaches_the_renderers() {
        let app = window();
        let skins = [
            skin(
                "Neon",
                Base::Dark,
                &[
                    ("wave-style", Value::Int(1)),
                    ("deck1", color(0, 255, 0)),
                    ("deck1-hi", color(1, 2, 3)),
                    ("deck2", color(200, 0, 255)),
                    ("wave-fade", Value::Float(0.6)),
                ],
            ),
            skin(
                "Plain",
                Base::Dark,
                &[("deck1", color(0, 255, 0)), ("deck2", color(200, 0, 255))],
            ),
        ];
        restyle(&app, &skins, "Neon");
        let p = palette(&read_all(&app));
        let (d1, d2) = (p.for_deck(0), p.for_deck(1));
        assert_eq!(d1.band, [[0, 255, 0], [1, 2, 3], p.band[2]]);
        assert_eq!(d2.band[0], [200, 0, 255]);
        assert_eq!(
            d2.band[1],
            Palette::DEFAULT.decks[1][1],
            "deck2-hi keeps its default"
        );
        assert_eq!(d1.band[2], d2.band[2], "the high band is shared");
        assert_eq!(p.fade, 0.6);
        restyle(&app, &skins, "Plain");
        let p = palette(&read_all(&app));
        assert_eq!(
            p.for_deck(0),
            p.for_deck(1),
            "wave-style 0: every deck the same"
        );
        assert_eq!(p.for_deck(0).band, p.band);
        assert_eq!(p.fade, 0.0);
        restyle(&app, &skins, BUILTIN[0]);
    }

    #[test]
    fn the_docs_list_every_token() {
        let docs = include_str!("../../../docs/skins.md");
        for (name, _) in TOKENS {
            assert!(
                docs.contains(&format!("`{name}`")),
                "docs/skins.md does not list the token {name}"
            );
        }
        assert!(docs.contains("`display-font`"));
    }
}
