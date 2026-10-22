//! The user keymap: a [`Preset`] (the defaults) plus per-action overrides, stored as the `keymap` setting.
//!
//! Map text (ours): `preset=left-handed;deck.cue.A=q;deck.pad.B=shift+a-h;app.help=`, `;`-separated
//! `id=binding` pairs. `preset` is a reserved id (absent = DEFAULT). A binding is `ctrl+shift+key` (modifiers
//! optional, lower case) where `key` is a character, `space`, `enter`, `left`, `right`, `plus`, `semicolon` or an
//! inclusive range `a-b` (the 8 hot-cue pads); an empty binding clears the action. Ids are
//! `deck.<action>.<A-D>` / `app.<action>` (see `Act::slug`). Unknown ids are kept as written (a newer build's
//! actions survive); a bad binding or pair is reported by [`Keymap::parse`] and dropped.
//!
//! Presets: DEFAULT is the table this program shipped with (our choice).
//! LEFT-HANDED
//! (ours) swaps the letters left ↔ right across the keyboard rows (q↔p w↔o e↔i r↔u t↔y, a↔l s↔k d↔j f↔h, z↔m
//! x↔n v↔b; c and g stay) so the mouse hand can be the left one; Ctrl chords, digits, brackets, Space,
//! Enter and the arrows stay where they are.

use super::{Act, Bind, Def, Row, TABLE, normalize, span};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    #[default]
    Standard,
    LeftHanded,
}

/// Letter pairs swapped by LEFT-HANDED.
const MIRROR: [(char, char); 12] = [
    ('q', 'p'),
    ('w', 'o'),
    ('e', 'i'),
    ('r', 'u'),
    ('t', 'y'),
    ('a', 'l'),
    ('s', 'k'),
    ('d', 'j'),
    ('f', 'h'),
    ('z', 'm'),
    ('x', 'n'),
    ('v', 'b'),
];

impl Preset {
    pub const ALL: [Preset; 2] = [Preset::Standard, Preset::LeftHanded];

    /// The name stored in the file.
    pub fn name(self) -> &'static str {
        match self {
            Preset::Standard => "default",
            Preset::LeftHanded => "left-handed",
        }
    }

    /// The name shown on the preset chip.
    pub fn title(self) -> &'static str {
        match self {
            Preset::Standard => "DEFAULT",
            Preset::LeftHanded => "LEFT-HANDED",
        }
    }

    fn bind(self, d: &Def) -> Bind {
        let mut b = Bind::new(d.keys, d.shift, d.ctrl);
        if self == Preset::LeftHanded && !b.ctrl {
            let mut cs = b.keys.chars();
            if let (Some(c), None) = (cs.next(), cs.next()) {
                let to = MIRROR.iter().find_map(|&(a, z)| {
                    if c == a {
                        Some(z)
                    } else if c == z {
                        Some(a)
                    } else {
                        None
                    }
                });
                b.keys = to.unwrap_or(c).to_string();
            }
        }
        b
    }
}

/// A preset and the user's overrides of it (`None` = the action's key is cleared), in file order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keymap {
    pub preset: Preset,
    over: Vec<(String, Option<Bind>)>,
}

impl Keymap {
    /// Reads map text; the notes describe every pair that was ignored.
    pub fn parse(text: &str) -> (Keymap, Vec<String>) {
        let (mut km, mut notes) = (Keymap::default(), Vec::new());
        for item in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let Some((id, v)) = item.split_once('=') else {
                notes.push(format!("`{item}` is not id=binding"));
                continue;
            };
            let (id, v) = (id.trim(), v.trim());
            if id == "preset" {
                match Preset::ALL
                    .iter()
                    .find(|p| p.name().eq_ignore_ascii_case(v))
                {
                    Some(&p) => km.preset = p,
                    None => notes.push(format!("unknown preset `{v}`")),
                }
            } else {
                match parse_bind(v) {
                    Ok(b) => km.put(id, b),
                    Err(e) => notes.push(format!("{id}: {e}")),
                }
            }
        }
        (km, notes)
    }

    /// The map text; empty for the plain DEFAULT preset. `parse(to_text())` gives the same keymap back.
    pub fn to_text(&self) -> String {
        let preset =
            (self.preset != Preset::Standard).then(|| format!("preset={}", self.preset.name()));
        let over = self
            .over
            .iter()
            .map(|(id, b)| format!("{id}={}", b.as_ref().map_or(String::new(), format_bind)));
        preset.into_iter().chain(over).collect::<Vec<_>>().join(";")
    }

    /// The live table: every action with its binding (the override, else the preset's default).
    pub fn rows(&self) -> Vec<Row> {
        TABLE
            .iter()
            .map(|d| {
                let id = d.id();
                let bind = match self.over.iter().find(|(i, _)| *i == id) {
                    Some((_, b)) => b.clone(),
                    None => Some(self.preset.bind(d)),
                };
                Row {
                    id,
                    bind,
                    deck: d.deck,
                    act: d.act,
                    what: d.what,
                }
            })
            .collect()
    }

    /// The preset's own binding of action `id` (`None` for an unknown id).
    fn default_of(&self, id: &str) -> Option<Bind> {
        TABLE
            .iter()
            .find(|d| d.id() == id)
            .map(|d| self.preset.bind(d))
    }

    /// Whether the row's binding differs from the preset's.
    pub fn is_custom(&self, r: &Row) -> bool {
        self.default_of(&r.id) != r.bind
    }

    fn put(&mut self, id: &str, b: Option<Bind>) {
        match self.over.iter_mut().find(|(i, _)| i == id) {
            Some(o) => o.1 = b,
            None => self.over.push((id.to_owned(), b)),
        }
    }

    /// Binds (or, with `None`, clears) action `id`; a binding equal to the preset's is no override at all.
    pub fn set(&mut self, id: &str, b: Option<Bind>) {
        match self.default_of(id) {
            Some(d) if Some(&d) == b.as_ref() => self.over.retain(|(i, _)| i != id),
            _ => self.put(id, b),
        }
    }

    /// Switches to `preset` and drops every override of a known action (unknown ids stay).
    pub fn use_preset(&mut self, preset: Preset) {
        self.preset = preset;
        self.over
            .retain(|(id, _)| !TABLE.iter().any(|d| d.id() == *id));
    }
}

/// `ctrl+shift+key` → binding; empty → `None` (cleared); anything the table could never match is an error.
fn parse_bind(s: &str) -> Result<Option<Bind>, String> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return Ok(None);
    }
    let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    let (mut shift, mut ctrl) = (false, false);
    for m in parts {
        match m {
            "shift" => shift = true,
            "ctrl" => ctrl = true,
            _ => return Err(format!("unknown modifier `{m}`")),
        }
    }
    let key = match key {
        "plus" => "+",
        "semicolon" => ";",
        k => k,
    };
    let ok = match key.chars().collect::<Vec<_>>()[..] {
        [a, '-', b] => {
            a <= b
                && canonical(&a.to_string(), shift, ctrl)
                && canonical(&b.to_string(), shift, ctrl)
        }
        _ => canonical(key, shift, ctrl),
    };
    if ok {
        Ok(Some(Bind::new(key, shift, ctrl)))
    } else {
        Err(format!("`{s}` is not a key the table can match"))
    }
}

fn format_bind(b: &Bind) -> String {
    let key = match b.keys.as_str() {
        "+" => "plus",
        ";" => "semicolon",
        k => k,
    };
    format!(
        "{}{}{key}",
        if b.ctrl { "ctrl+" } else { "" },
        if b.shift { "shift+" } else { "" }
    )
}

/// A character a key press can produce: not a control, whitespace or private-use code (arrows, function keys…).
fn valid_char(c: char) -> bool {
    !c.is_control() && !c.is_whitespace() && !('\u{e000}'..='\u{f8ff}').contains(&c)
}

/// Whether `key` (a name or one character) with these modifiers is exactly what [`normalize`] makes of a key
/// event, i.e. whether a press can ever match it (`shift+_` or `!` cannot: the press arrives as `_` / `shift+1`).
fn canonical(key: &str, shift: bool, ctrl: bool) -> bool {
    let text = match key {
        "space" => " ",
        "enter" => "\n",
        "left" => "\u{f702}",
        "right" => "\u{f703}",
        k => k,
    };
    let named = text != key;
    (named || key.chars().next().is_some_and(valid_char))
        && normalize(text, shift, ctrl) == Some((key.to_owned(), shift))
}

/// What the editor does with a key press.
#[derive(Debug, PartialEq)]
pub enum Capture {
    /// A modifier on its own, up / down, function keys…
    Ignore,
    /// Escape.
    Cancel,
    /// Backspace / Delete.
    Clear,
    Assign(Bind),
}

pub fn capture(text: &str, shift: bool, ctrl: bool) -> Capture {
    match text {
        "\u{1b}" => Capture::Cancel,
        "\u{8}" | "\u{7f}" => Capture::Clear,
        _ => match normalize(text, shift, ctrl) {
            Some((k, s)) if canonical(&k, s, ctrl) => Capture::Assign(Bind {
                keys: k,
                shift: s,
                ctrl,
            }),
            _ => Capture::Ignore,
        },
    }
}

/// `b` as the binding of `act`: an action with a key range (the 8 pads) takes the pressed key as the start of
/// the range (`1` → `1-8`, `a` → `a-h`); the others take `b` as is.
pub fn fit(act: Act, b: Bind) -> Result<Bind, String> {
    let n = act.span();
    if n == 1 {
        return Ok(b);
    }
    let mut cs = b.keys.chars();
    let c = cs.next().filter(|_| cs.next().is_none());
    let end = c.and_then(|c| char::from_u32(c as u32 + n as u32 - 1));
    match (c, end) {
        (Some(c), Some(e))
            if (c.is_ascii_digit() && e.is_ascii_digit())
                || (c.is_ascii_lowercase() && e.is_ascii_lowercase()) =>
        {
            Ok(Bind {
                keys: format!("{c}-{e}"),
                ..b
            })
        }
        _ => Err(tr!(
            "{} consecutive digits or letters are needed: press the first one (1 → 1-8, a → a-h)",
            n
        )),
    }
}

fn overlaps(a: &Bind, b: &Bind) -> bool {
    a.shift == b.shift
        && a.ctrl == b.ctrl
        && match (span(&a.keys), span(&b.keys)) {
            (Some(x), Some(y)) => x.0 <= y.1 && y.0 <= x.1,
            _ => a.keys == b.keys,
        }
}

/// The other actions of `rows` that a press of `b` would also trigger.
pub fn conflicts<'a>(rows: &'a [Row], id: &str, b: &Bind) -> Vec<&'a Row> {
    rows.iter()
        .filter(|r| r.id != id && r.bind.as_ref().is_some_and(|o| overlaps(o, b)))
        .collect()
}

#[cfg(test)]
mod tests;
