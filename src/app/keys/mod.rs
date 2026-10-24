//! Keyboard shortcuts (`KeysState` in ui/help.slint, editor in ui/prefs-keyboard.slint). [`TABLE`] holds the
//! default binding of every action; the table that is live is the preset's defaults overridden by the user's
//! keymap (`prefs.keyboard.keymap`, see [`keymap`]), rebuilt whenever the settings change. It drives [`lookup`] →
//! dispatch onto the existing `DeckState` callbacks, and the `?` help overlay.
//!
//! Typed text never gets here: the search `LineEdit` consumes it before it bubbles up to the root
//! `FocusScope`, and the track table consumes its own arrows. Ctrl+F is not text, so it bubbles.
//! ponytail: shifted digits are read as the US layout's `!@#$%^&*`; other layouts need a keycode API.
//!
//! Children: [`table`] (the actions and the default binding of each), [`keymap`] (presets, map text ↔ table,
//! conflicts, the capture of a key press for the editor).

mod keymap;
mod table;
#[cfg(test)]
mod tests;

pub use keymap::{capture, conflicts, fit, Capture, Keymap, Preset};
pub use table::{Act, Bind, Row};

use table::{span, Def, TABLE};

use super::{
    deck::DECK_NAMES, prefs, App, AutomixState, BrowserState, DeckState, FxState, HelpLine,
    KeysState, LibraryState, PrefsState, SamplerState,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

/// A key event as the table sees it: `(name, shift)`, or `None` for multi-character text.
/// Space/Enter get names, letters lower-case, Ctrl+letter (sent as a control character by some
/// backends) its letter, shifted digits their digit, and punctuation ignores Shift.
fn normalize(text: &str, shift: bool, ctrl: bool) -> Option<(String, bool)> {
    let mut chars = text.chars();
    let c = chars.next().filter(|_| chars.next().is_none())?;
    Some(match c {
        ' ' => ("space".into(), shift),
        '\u{f702}' => ("left".into(), false),
        '\u{f703}' => ("right".into(), false),
        '\n' | '\r' => ("enter".into(), shift),
        '\u{1}'..='\u{1a}' if ctrl => (((c as u8 - 1 + b'a') as char).to_string(), shift),
        '!'..='/' | ':'..='@' | '['..='`' | '{'..='~' => match "!@#$%^&*".find(c) {
            Some(i) => ((i + 1).to_string(), true),
            None => (c.to_string(), false),
        },
        _ => (c.to_lowercase().collect(), shift),
    })
}

/// The offset of `key` within `keys`, or `None` if it does not match.
fn offset(keys: &str, key: &str) -> Option<usize> {
    let Some((a, b)) = span(keys) else {
        return (keys == key).then_some(0);
    };
    let mut k = key.chars();
    let k = k.next().filter(|_| k.next().is_none())?;
    (a..=b).contains(&k).then(|| k as usize - a as usize)
}

/// The row of `rows` a key event triggers, with the key's offset inside the row's range.
fn find(rows: &[Row], text: &str, shift: bool, ctrl: bool) -> Option<(Row, usize)> {
    let (key, shift) = normalize(text, shift, ctrl)?;
    rows.iter().find_map(|r| {
        let b = r
            .bind
            .as_ref()
            .filter(|b| b.shift == shift && b.ctrl == ctrl)?;
        offset(&b.keys, &key).map(|n| (r.clone(), n))
    })
}

thread_local! {
    /// The live table (UI thread): the DEFAULT preset until [`wire`] has read the settings.
    static LIVE: RefCell<Vec<Row>> = RefCell::new(Keymap::default().rows());
}

type Hook = Rc<dyn Fn(&str, bool, bool) -> bool>;

thread_local! {
    /// Sees every key press before the table (`text, shift, ctrl`); `true` = consumed. The keymap editor uses
    /// it to take the key it is waiting for.
    static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
}

/// Installs the key hook (see [`HOOK`]).
pub fn set_hook(f: impl Fn(&str, bool, bool) -> bool + 'static) {
    HOOK.with(|k| *k.borrow_mut() = Some(Rc::new(f)));
}

/// The live-table row a key event triggers, with the key's offset inside the row's range.
fn lookup(text: &str, shift: bool, ctrl: bool) -> Option<(Row, usize)> {
    LIVE.with(|l| find(&l.borrow(), text, shift, ctrl))
}

/// "Ctrl+Shift+Space"-style label of a binding.
pub fn label(b: &Bind) -> String {
    let mut keys = b.keys.chars();
    let first: String = keys
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();
    format!(
        "{}{}{first}{}",
        if b.ctrl { "Ctrl+" } else { "" },
        if b.shift { "Shift+" } else { "" },
        keys.as_str()
    )
}

/// What a row does, followed by its deck letter for deck actions.
pub fn name(r: &Row) -> String {
    let deck = r
        .deck
        .map_or(String::new(), |d| tr!(", deck {}", DECK_NAMES[d as usize]));
    format!("{}{deck}", crate::app::i18n::tr_dyn(r.what))
}

fn help_line(r: &Row) -> HelpLine {
    HelpLine {
        keys: r.bind.as_ref().map_or(String::new(), label).into(),
        what: name(r).into(),
    }
}

/// The deck a row of "deck 1" / "deck 2" (any deck row) acts on: the one its slot draws.
fn slot_deck(r: &Row) -> i32 {
    super::deck::slots::deck_in(r.deck.unwrap_or(0) as usize) as i32
}

#[rustfmt::skip]
fn dispatch(app: &App, r: &Row, n: usize) {
    let (decks, deck) = (app.global::<DeckState>(), slot_deck(r));
    match r.act {
        Act::PlayPause => decks.invoke_play_pause(deck),
        Act::Cue => decks.invoke_cue(deck),
        Act::Load => decks.invoke_load(deck),
        Act::Sync => decks.invoke_sync(deck),
        Act::Master => decks.invoke_master_toggle(deck),
        Act::Pad => decks.invoke_pad(deck, n as i32),
        Act::AutoLoop => decks.invoke_auto_loop(deck, 0), // ≤ 0 = the length from Preferences > Controller
        Act::ExportLoop => app.global::<SamplerState>().invoke_export_loop(deck),
        Act::BeatJump(beats) => decks.invoke_beat_jump(deck, beats),
        Act::LoopIn => decks.invoke_loop_in(deck),
        Act::LoopOut => decks.invoke_loop_out(deck),
        Act::Reloop => decks.invoke_reloop(deck),
        Act::KeySync => decks.invoke_key_sync(deck),
        Act::KeyStep(st) => decks.invoke_key_step(deck, st),
        Act::KeyReset => decks.invoke_key_reset(deck),
        Act::Slip => decks.invoke_slip(deck),
        Act::Reverse => decks.invoke_reverse(deck),
        Act::Brake => decks.invoke_brake(deck),
        Act::GridToggle => decks.invoke_grid_toggle(deck),
        Act::GridTap => decks.invoke_grid_tap(deck),
        Act::GridBeat1 => decks.invoke_grid_beat1(deck),
        Act::GridShift(ms) => decks.invoke_grid_shift(deck, ms),
        Act::PadMode => {
            let mode = decks.get_decks().row_data(deck as usize).map_or(0, |d| d.pad_mode);
            decks.invoke_pad_mode(deck, (mode + 1) % super::deck::PadMode::count());
        }
        Act::TagTrack => app.global::<BrowserState>().invoke_tag_add(),
        Act::FxBeat(d) => { let s = app.global::<FxState>(); s.invoke_beat_step(s.get_unit(), d) }
        Act::FxToggle => {
            let s = app.global::<FxState>();
            let u = s.get_unit();
            let target = s.get_units().row_data(u.max(0) as usize).map_or(0, |r| r.target);
            s.invoke_toggled(u, target)
        }
        Act::SamplerToggle => {
            let s = app.global::<SamplerState>();
            s.set_open(!s.get_open());
        }
        Act::AutomixToggle => app.global::<AutomixState>().invoke_toggled(),
        Act::AutomixSkip => app.global::<AutomixState>().invoke_skip(),
        Act::DeckLayout => app.global::<PrefsState>().invoke_chose("decks".into(), (decks.get_count() != 4) as i32), // option 1 = 4 DECKS
        Act::FocusSearch => app.invoke_focus_search(),
        Act::BrowseView => app.invoke_browse_view(),
        Act::BrowseArea => app.invoke_browse_area(),
        Act::Undo => app.global::<LibraryState>().invoke_action("undo".into()),
        Act::Redo | Act::RedoAlt => app.global::<LibraryState>().invoke_action("redo".into()),
        Act::Help => {
            let keys = app.global::<KeysState>();
            keys.set_help_visible(!keys.get_help_visible());
        }
    }
}

/// Runs the row of the live table with the id `id` (`deck.play_pause.A`, `app.automix_toggle`) as if its key was pressed,
/// whatever its binding; `false` when there is no such row. A pad row presses its first pad.
pub(super) fn run(app: &App, id: &str) -> bool {
    let row = LIVE.with(|l| l.borrow().iter().find(|r| r.id == id).cloned());
    row.is_some_and(|r| {
        dispatch(app, &r, 0);
        true
    })
}

/// Whether releasing the row's key means something: CUE (hold to preview), and a hot-cue / beat-jump
/// pad hands back to the slip playhead.
fn released(r: &Row) -> bool {
    matches!(r.act, Act::Cue | Act::Pad | Act::BeatJump(_))
}

/// The physical key behind a key event: shifted digits / braces and capitals fold to the unshifted
/// key (US layout), so a press and its release agree even when Shift went up in between.
fn physical(text: &str) -> String {
    let c = text.chars().next().unwrap_or(' ');
    match ("!@#$%^&*".find(c), c) {
        (Some(i), _) => (i + 1).to_string(),
        (_, '{') => "[".into(),
        (_, '}') => "]".into(),
        _ => c.to_lowercase().collect(),
    }
}

type Held = HashMap<String, Row>;

/// Remember a press whose release matters, so the release hits the same row whatever Shift says then.
fn hold(held: &mut Held, text: &str, r: &Row) {
    if released(r) {
        held.insert(physical(text), r.clone());
    }
}

/// The row a release belongs to: the pressed one, else (press never seen) the plain lookup.
fn release(held: &mut Held, text: &str, shift: bool, ctrl: bool) -> Option<Row> {
    held.remove(&physical(text))
        .or_else(|| lookup(text, shift, ctrl).map(|(r, _)| r))
        .filter(released)
}

/// Makes the keymap `map` (the `keymap` setting) the live table and publishes it to the help overlay.
fn apply(app: &App, map: &str) {
    let rows = Keymap::parse(map).0.rows();
    let lines: Vec<_> = rows
        .iter()
        .filter(|r| r.bind.is_some())
        .map(help_line)
        .collect();
    let (left, right) = lines.split_at(lines.len().div_ceil(2));
    let keys = app.global::<KeysState>();
    keys.set_help(ModelRc::new(VecModel::from(left.to_vec())));
    keys.set_help_right(ModelRc::new(VecModel::from(right.to_vec())));
    LIVE.with(|l| *l.borrow_mut() = rows);
}

/// Applies the user keymap (again whenever it changes) and routes root key events through the live table.
pub fn wire(app: &App) {
    let map = prefs::with(|p| p.keyboard.keymap.clone());
    for note in Keymap::parse(&map).1 {
        eprintln!("keymap: {note}, ignored");
    }
    apply(app, &map);
    let (weak, last) = (app.as_weak(), RefCell::new(map));
    prefs::on_change(move |p| {
        if *last.borrow() != p.keyboard.keymap {
            last.replace(p.keyboard.keymap.clone());
            if let Some(app) = weak.upgrade() {
                apply(&app, &p.keyboard.keymap);
            }
        }
    });
    let keys = app.global::<KeysState>();
    let weak = app.as_weak();
    let held = Rc::new(RefCell::new(Held::new()));
    let eaten = Rc::new(RefCell::new(HashSet::<String>::new()));
    let (h, e) = (held.clone(), eaten.clone());
    keys.on_key(move |text: SharedString, shift, ctrl| {
        let hook = HOOK.with(|k| k.borrow().clone());
        if hook.is_some_and(|f| f(&text, shift, ctrl)) {
            e.borrow_mut().insert(physical(&text)); // its release is not a shortcut either
            return true;
        }
        let (Some((r, n)), Some(app)) = (lookup(&text, shift, ctrl), weak.upgrade()) else {
            return false;
        };
        hold(&mut h.borrow_mut(), &text, &r);
        dispatch(&app, &r, n);
        true
    });
    let weak = app.as_weak();
    keys.on_key_released(move |text: SharedString, shift, ctrl| {
        if eaten.borrow_mut().remove(&physical(&text)) {
            return true;
        }
        let (Some(r), Some(app)) = (
            release(&mut held.borrow_mut(), &text, shift, ctrl),
            weak.upgrade(),
        ) else {
            return false;
        };
        let (state, deck) = (app.global::<DeckState>(), slot_deck(&r));
        if r.act == Act::Cue {
            state.invoke_cue_released(deck)
        } else {
            state.invoke_pad_released(deck)
        }
        true
    });
}
