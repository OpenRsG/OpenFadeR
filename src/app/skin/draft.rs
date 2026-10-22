//! The theme editor's working copy: every token's value, edited one at a time and shown live. While a [`Draft`] is open
//! ([`open`] … [`close`]) `skin::apply` styles every window with the draft's values instead of the chosen skin's, so the whole app
//! is the preview; closing it (Discard, a theme chosen, Save as) restores the chosen skin. The values are kept for every token (not
//! only the changed ones), so a saved theme is complete and does not depend on what the built-in values become later.

use super::{Base, Skin, TOKEN_INFO, TOKENS, Ty, Value, file};
use slint::Color;
use std::cell::RefCell;

/// A theme being edited.
pub struct Draft {
    pub base: Base,
    /// Every token's value, in [`TOKENS`] order.
    pub values: Vec<Value>,
    /// The values the draft was opened with: what "reset" goes back to and what counts as "changed".
    pub start: Vec<Value>,
    /// The built-in values of `base`: the ones a base switch moves the untouched tokens to.
    pub defaults: Vec<Value>,
    pub author: String,
    pub description: String,
    pub layout: Vec<(String, String)>,
    /// The colour token open in the picker and its hue (degrees), saturation, value and alpha, kept apart because a grey has no hue.
    pub picked: Option<usize>,
    pub hsva: [f32; 4],
}

/// One token as the editor's list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Position in [`TOKENS`].
    pub index: usize,
    pub name: &'static str,
    pub hint: &'static str,
    /// 0 colour, 1 number (length, int, float or duration), 2 text, 3 the font family.
    pub kind: i32,
    /// The value as the field shows it (`#rrggbb`, `12`, `80ms`).
    pub text: String,
    pub color: Color,
    pub changed: bool,
}

thread_local! {
    static DRAFT: RefCell<Option<Draft>> = const { RefCell::new(None) };
}

/// The open draft's base and values (what `skin::apply` shows instead of the chosen skin).
pub fn current() -> Option<(Base, Vec<Value>)> {
    DRAFT.with_borrow(|d| d.as_ref().map(|d| (d.base, d.values.clone())))
}

/// Whether a draft is open.
pub fn is_open() -> bool {
    DRAFT.with_borrow(Option::is_some)
}

/// Runs `f` on the open draft (`None`: no draft is open).
pub fn with<R>(f: impl FnOnce(&mut Draft) -> R) -> Option<R> {
    DRAFT.with_borrow_mut(|d| d.as_mut().map(f))
}

/// Opens a draft that starts as the skin `name` among `skins` (`false`: no such skin, or no window has been styled yet).
pub fn open(skins: &[Skin], name: &str) -> bool {
    let Some((base, values)) = super::values_of(skins, name) else {
        return false;
    };
    let file = skins.iter().find(|s| s.name.eq_ignore_ascii_case(name));
    DRAFT.with_borrow_mut(|d| {
        *d = Some(Draft {
            base,
            start: values.clone(),
            values,
            defaults: super::defaults(base),
            author: file.map_or_else(String::new, |s| s.author.clone()),
            description: file.map_or_else(String::new, |s| s.description.clone()),
            layout: file.map_or_else(Vec::new, |s| s.layout.clone()),
            picked: None,
            hsva: [0.0; 4],
        });
    });
    true
}

pub fn close() {
    DRAFT.with_borrow_mut(|d| *d = None);
}

/// The editor's groups in theme.slint order (`// @group`).
pub fn groups() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (g, _) in TOKEN_INFO {
        if !out.contains(g) {
            out.push(g);
        }
    }
    out
}

impl Draft {
    /// The tokens of group `group` (an index into [`groups`]), in file order.
    pub fn rows(&self, group: usize) -> Vec<Row> {
        let Some(&name) = groups().get(group) else {
            return Vec::new();
        };
        (0..TOKENS.len())
            .filter(|&i| TOKEN_INFO[i].0 == name)
            .map(|i| self.row(i))
            .collect()
    }

    pub fn row(&self, i: usize) -> Row {
        let (name, ty) = TOKENS[i];
        let v = &self.values[i];
        Row {
            index: i,
            name,
            hint: TOKEN_INFO[i].1,
            kind: match (ty, name) {
                (Ty::Color, _) => 0,
                (Ty::Str, "font") => 3,
                (Ty::Str, _) => 2,
                _ => 1,
            },
            text: file::show(v),
            color: match v {
                Value::Color(c) => *c,
                _ => Color::from_argb_u8(0, 0, 0, 0),
            },
            changed: *v != self.start[i],
        }
    }

    /// Sets token `i` (`false`: not a value of that token's type).
    pub fn set(&mut self, i: usize, v: Value) -> bool {
        let ok = i < TOKENS.len()
            && std::mem::discriminant(&v) == std::mem::discriminant(&self.values[i]);
        if ok {
            self.values[i] = v;
        }
        ok
    }

    /// Sets token `i` from what was typed in its field.
    pub fn set_text(&mut self, i: usize, text: &str) -> Result<(), String> {
        let ty = TOKENS.get(i).ok_or(tr!("no such token"))?.1;
        let v = file::from_text(ty, text)?;
        self.set(i, v);
        Ok(())
    }

    /// Moves a number token one step (`dir` ±1): a length 1 px, a whole number 1 (a font weight 100), a float 0.05, a duration 10 ms.
    pub fn step(&mut self, i: usize, dir: i32) {
        let d = dir.signum();
        let name = TOKENS.get(i).map_or("", |t| t.0);
        let v = match &self.values[i] {
            Value::Length(n) => Value::Length((n + d as f32).max(0.0)),
            Value::Int(n) => {
                Value::Int((n + d * if name.starts_with("weight") { 100 } else { 1 }).max(0))
            }
            Value::Float(n) => {
                Value::Float(((n + 0.05 * d as f32).clamp(0.0, 1.0) * 100.0).round() / 100.0)
            }
            Value::Duration(ms) => Value::Duration((ms + 10 * i64::from(d)).max(0)),
            _ => return,
        };
        self.values[i] = v;
    }

    /// Puts token `i` back to the value it had when the draft was opened.
    pub fn reset(&mut self, i: usize) {
        if let Some(s) = self.start.get(i) {
            self.values[i] = s.clone();
        }
    }

    pub fn reset_all(&mut self) {
        self.values.clone_from(&self.start);
    }

    /// Switches the base: every token still at the old base's value takes the new base's, the edited ones stay.
    pub fn set_base(&mut self, base: Base) {
        if base == self.base {
            return;
        }
        let new = super::defaults(base);
        for ((v, old), new) in self.values.iter_mut().zip(&self.defaults).zip(&new) {
            if v == old {
                v.clone_from(new);
            }
        }
        (self.base, self.defaults) = (base, new);
    }

    /// Opens colour token `i` in the picker (anything else closes it).
    pub fn pick(&mut self, i: Option<usize>) {
        self.picked = i.filter(|&i| matches!(self.values.get(i), Some(Value::Color(_))));
        if let Some(Value::Color(c)) = self.picked.map(|i| &self.values[i]) {
            let h = c.to_hsva();
            self.hsva = [h.hue, h.saturation, h.value, h.alpha];
        }
    }

    /// The picked colour from the picker's sliders: hue in degrees, the rest 0..1.
    pub fn set_hsva(&mut self, hsva: [f32; 4]) {
        let Some(i) = self.picked else { return };
        self.hsva = [
            hsva[0].rem_euclid(360.0),
            hsva[1].clamp(0.0, 1.0),
            hsva[2].clamp(0.0, 1.0),
            hsva[3].clamp(0.0, 1.0),
        ];
        let [h, s, v, a] = self.hsva;
        self.values[i] = Value::Color(Color::from_hsva(h, s, v, a));
    }

    /// Sets the picked colour to `c` (a swatch, the hex field) and moves the sliders to it.
    pub fn set_picked(&mut self, c: Color) {
        if let Some(i) = self.picked {
            self.values[i] = Value::Color(c);
            self.pick(Some(i));
        }
    }

    /// The draft as a skin called `name`, with every token.
    pub fn to_skin(&self, name: &str) -> Skin {
        Skin {
            name: name.trim().to_owned(),
            base: self.base,
            tokens: super::complete(self.values.clone()),
            author: self.author.clone(),
            description: self.description.clone(),
            layout: self.layout.clone(),
            path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, headless, skin};

    fn at(name: &str) -> usize {
        TOKENS.iter().position(|t| t.0 == name).unwrap()
    }

    /// A draft of the built-in dark skin (the draft is thread-local and every test has its own thread).
    fn dark() -> App {
        headless::init();
        let app = App::new().unwrap();
        skin::restyle(&app, &[], skin::BUILTIN[0]);
        assert!(open(&[], skin::BUILTIN[0]));
        app
    }

    #[test]
    fn groups_cover_every_token_once() {
        let _app = dark();
        let g = groups();
        assert_eq!(g[0], "Surfaces");
        assert!(g.contains(&"Waveform") && g.contains(&"Typography") && g.contains(&"Motion"));
        with(|d| {
            let total: usize = (0..g.len()).map(|n| d.rows(n).len()).sum();
            assert_eq!(total, TOKENS.len());
            assert!(d.rows(g.len()).is_empty());
        })
        .unwrap();
    }

    #[test]
    fn rows_know_their_kind_hint_and_whether_they_changed() {
        let _app = dark();
        with(|d| {
            let accent = d.row(at("accent"));
            assert_eq!(
                (accent.kind, accent.text.as_str(), accent.changed),
                (0, "#59719e", false)
            );
            assert_eq!(d.row(at("font")).kind, 3);
            assert_eq!(d.row(at("font-size")).kind, 1);
            assert_eq!(d.row(at("font-size")).text, "11");
            assert_eq!(d.row(at("t-fast")).text, "80ms");
            assert!(d.row(at("bg")).hint.contains("Window"));
            d.set(at("accent"), Value::Color(Color::from_rgb_u8(1, 2, 3)));
            assert!(d.row(at("accent")).changed);
            d.reset(at("accent"));
            assert!(!d.row(at("accent")).changed);
            assert!(!d.set(at("accent"), Value::Length(1.0)), "wrong type");
        })
        .unwrap();
    }

    #[test]
    fn typed_values_steps_and_resets() {
        let _app = dark();
        with(|d| {
            d.set_text(at("accent"), "ff8800").unwrap();
            assert_eq!(d.row(at("accent")).text, "#ff8800");
            d.set_text(at("scrim"), "#11223344").unwrap();
            assert_eq!(d.row(at("scrim")).text, "#11223344");
            assert!(d.set_text(at("accent"), "red").is_err());
            d.set_text(at("font-size"), "13px").unwrap();
            d.step(at("font-size"), 1);
            assert_eq!(d.row(at("font-size")).text, "14");
            d.step(at("weight"), 1);
            assert_eq!(d.row(at("weight")).text, "500");
            d.step(at("t-fast"), -1);
            assert_eq!(d.row(at("t-fast")).text, "70ms");
            d.step(at("key-sat"), 1);
            d.step(at("key-sat"), 1);
            assert_eq!(d.row(at("key-sat")).text, "0.55");
            d.step(at("h-bar"), -100);
            d.set_text(at("font"), "Inter").unwrap();
            assert_eq!(d.row(at("font")).text, "Inter");
            d.reset_all();
            assert!((0..TOKENS.len()).all(|i| !d.row(i).changed));
        })
        .unwrap();
    }

    #[test]
    fn the_picker_keeps_the_hue_of_a_grey_and_follows_the_sliders() {
        let _app = dark();
        with(|d| {
            d.pick(Some(at("font-size")));
            assert_eq!(d.picked, None, "only colours open the picker");
            d.pick(Some(at("accent")));
            let [h, s, v, a] = d.hsva;
            assert!(
                (200.0..230.0).contains(&h) && s > 0.4 && v > 0.6 && a == 1.0,
                "{:?}",
                d.hsva
            );
            d.set_hsva([120.0, 0.0, 0.502, 1.0]);
            assert_eq!(d.hsva[0], 120.0, "the hue survives a zero saturation");
            assert_eq!(d.row(at("accent")).text, "#808080");
            d.set_hsva([0.0, 1.0, 1.0, 0.502]);
            assert_eq!(d.row(at("accent")).text, "#ff000080");
            d.set_picked(Color::from_rgb_u8(0, 0, 255));
            assert_eq!(d.hsva[0].round(), 240.0);
        })
        .unwrap();
    }

    #[test]
    fn switching_the_base_moves_only_the_untouched_tokens() {
        let app = dark();
        // the light built-ins, read with no draft open (an open draft is what a window shows)
        close();
        let light = skin::restyle(&app, &[], skin::BUILTIN[1]);
        skin::restyle(&app, &[], skin::BUILTIN[0]);
        assert!(open(&[], skin::BUILTIN[0]));
        with(|d| {
            d.set(at("accent"), Value::Color(Color::from_rgb_u8(1, 2, 3)));
            d.set_base(Base::Light);
            assert_eq!(d.values[at("bg")], light[at("bg")]);
            assert_eq!(d.row(at("accent")).text, "#010203");
            assert!(d.row(at("bg")).changed, "it moved with the base");
            d.set_base(Base::Dark);
            assert_eq!(d.row(at("bg")).text, "#11111b");
            assert!(!d.row(at("bg")).changed, "and is back");
        })
        .unwrap();
    }

    #[test]
    fn a_saved_draft_reads_back_the_same_and_the_open_draft_wins_over_the_chosen_skin() {
        let app = dark();
        let skin_out = with(|d| {
            d.set_text(at("accent"), "#abcdef").unwrap();
            d.step(at("key-sat"), 1);
            d.layout = vec![("deck_layout".into(), "vertical".into())];
            d.to_skin(" Mine ")
        })
        .unwrap();
        assert_eq!(skin_out.name, "Mine");
        assert_eq!(skin_out.tokens.len(), TOKENS.len(), "a complete theme");
        let (back, notes) = file::parse(&file::serialize(&skin_out), "x").unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(back.tokens, skin_out.tokens);
        assert_eq!(back.layout, skin_out.layout);
        // the draft is what every window shows, whatever skin is chosen
        skin::restyle(&app, &[], skin::BUILTIN[1]);
        let shown = skin::read_all(&app);
        assert_eq!(
            shown[at("accent")],
            Value::Color(Color::from_rgb_u8(0xab, 0xcd, 0xef))
        );
        close();
        skin::restyle(&app, &[], skin::BUILTIN[1]);
        assert_ne!(skin::read_all(&app)[at("accent")], shown[at("accent")]);
        assert!(!is_open());
    }
}
