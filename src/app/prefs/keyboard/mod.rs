//! KEYBOARD category: the user keymap. Typed section [`Keyboard`]; the editor page is `ui/prefs-keyboard.slint`
//! (`KeyboardPage`, global `KeymapState`) and the table it edits is `app/keys` (presets, map text, conflicts).
//!
//! The editor works on the setting alone: every assignment is `prefs::update`, so it is saved at once and
//! `app/keys` re-reads the map through `prefs::on_change` (live). The page selects a row, then the next key press
//! reaches [`Editor::key`] through `keys::set_hook` (whichever item has the focus; the hook only takes keys while
//! the dialog shows this category). Assigning a key another action uses asks REPLACE / CANCEL; replacing clears
//! the other action. Flow and wording are ours (select row, press key, conflict warning).

mod page;

pub use page::wire;

use super::{section, Prefs, Row};
use crate::app::keys::{self, capture, conflicts, fit, Bind, Capture, Row as KeyRow};

section!(
    Keyboard {
        /// The user's key bindings as `;`-separated `id=binding` pairs, with the reserved `preset=<name>` first
        /// (see `app/keys/keymap.rs`); empty = the DEFAULT preset's keys.
        keymap: String = String::new();
    }
);

/// The page is the custom editor of [`wire`], so there are no generic rows.
pub fn rows(_: &Prefs) -> Vec<Row> {
    Vec::new()
}

pub fn choose(_: &mut Prefs, _key: &str, _i: usize) -> bool {
    false
}

/// Changes to the keymap: `(action id, new binding)`, `None` = cleared.
type Edits = Vec<(String, Option<Bind>)>;

/// What the page is doing: the action waiting for a key, a key that clashes and awaits REPLACE / CANCEL, a hint.
#[derive(Default)]
struct Editor {
    selected: Option<String>,
    pending: Option<(Bind, Vec<String>)>,
    note: String,
}

impl Editor {
    /// A click on action `id`: edit it, or (it was the one being edited) stop.
    fn select(&mut self, id: &str) {
        let same = self.selected.as_deref() == Some(id);
        *self = Editor::default();
        if !same {
            self.selected = Some(id.to_owned());
        }
    }

    fn cancel(&mut self) {
        *self = Editor::default();
    }

    /// A key press while an action is selected (Enter / Esc answer a clash); the edits it makes.
    fn key(&mut self, rows: &[KeyRow], text: &str, shift: bool, ctrl: bool) -> Edits {
        let Some(id) = self.selected.clone() else {
            return Vec::new();
        };
        if let Some((bind, victims)) = self.pending.clone() {
            match text {
                "\n" | "\r" => {
                    self.cancel();
                    return victims
                        .into_iter()
                        .map(|v| (v, None))
                        .chain([(id, Some(bind))])
                        .collect();
                }
                "\u{1b}" => self.cancel(),
                _ => {}
            }
            return Vec::new();
        }
        self.note.clear();
        match capture(text, shift, ctrl) {
            Capture::Ignore => Vec::new(),
            Capture::Cancel => {
                self.cancel();
                Vec::new()
            }
            Capture::Clear => {
                self.cancel();
                vec![(id, None)]
            }
            Capture::Assign(b) => {
                let Some(row) = rows.iter().find(|r| r.id == id) else {
                    self.cancel();
                    return Vec::new();
                };
                match fit(row.act, b) {
                    Err(e) => {
                        self.note = e;
                        Vec::new()
                    }
                    Ok(b) => {
                        let victims: Vec<String> = conflicts(rows, &id, &b)
                            .into_iter()
                            .map(|r| r.id.clone())
                            .collect();
                        if victims.is_empty() {
                            self.cancel();
                            vec![(id, Some(b))]
                        } else {
                            self.pending = Some((b, victims));
                            Vec::new()
                        }
                    }
                }
            }
        }
    }

    fn status(&self, rows: &[KeyRow]) -> String {
        let what = |id: &String| {
            rows.iter()
                .find(|r| r.id == *id)
                .map_or_else(String::new, keys::name)
        };
        match (&self.pending, &self.selected) {
            (Some((b, victims)), _) => format!(
                "{} is already used by {}. REPLACE (Enter) takes it from there, CANCEL (Esc) keeps things as they are.",
                keys::label(b),
                victims.iter().map(what).collect::<Vec<_>>().join(", ")
            ),
            (None, Some(_)) if !self.note.is_empty() => self.note.clone(),
            (None, Some(id)) => format!("Press the new key for “{}”.  Backspace clears it, Esc cancels.", what(id)),
            _ => "Click an action, then press the key to give it.  Changes apply at once and are saved.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::keys::Keymap;

    fn rows() -> Vec<KeyRow> {
        Keymap::default().rows()
    }

    fn bind(keys: &str, shift: bool, ctrl: bool) -> Bind {
        Bind {
            keys: keys.into(),
            shift,
            ctrl,
        }
    }

    fn editing(id: &str) -> Editor {
        let mut e = Editor::default();
        e.select(id);
        e
    }

    #[test]
    fn a_free_key_is_assigned_at_once() {
        let mut e = editing("deck.cue.A");
        assert_eq!(
            e.key(&rows(), "j", false, false),
            [("deck.cue.A".to_owned(), Some(bind("j", false, false)))]
        );
        assert!(
            e.selected.is_none(),
            "done: nothing waits for a key any more"
        );
    }

    #[test]
    fn a_used_key_asks_and_enter_replaces_it_on_the_other_action() {
        let mut e = editing("deck.cue.A");
        assert!(
            e.key(&rows(), "s", false, false).is_empty(),
            "S is deck A's SYNC"
        );
        assert!(e
            .status(&rows())
            .contains("S is already used by sync (latch) to the other deck, deck A"));
        assert!(
            e.key(&rows(), "q", false, false).is_empty(),
            "other keys are ignored while asking"
        );
        let edits = e.key(&rows(), "\n", false, false);
        assert_eq!(
            edits,
            [
                ("deck.sync.A".to_owned(), None),
                ("deck.cue.A".to_owned(), Some(bind("s", false, false)))
            ]
        );
        assert!(e.selected.is_none() && e.pending.is_none());
    }

    #[test]
    fn escape_cancels_an_assignment_and_a_clash() {
        let mut e = editing("deck.cue.A");
        assert!(e.key(&rows(), "\u{1b}", false, false).is_empty() && e.selected.is_none());
        let mut e = editing("deck.cue.A");
        e.key(&rows(), "s", false, false);
        assert!(
            e.key(&rows(), "\u{1b}", false, false).is_empty()
                && e.selected.is_none()
                && e.pending.is_none()
        );
    }

    #[test]
    fn backspace_clears_and_a_modifier_alone_changes_nothing() {
        let mut e = editing("app.help");
        assert!(e.key(&rows(), "\u{10}", true, false).is_empty() && e.selected.is_some());
        assert_eq!(
            e.key(&rows(), "\u{8}", false, false),
            [("app.help".to_owned(), None)]
        );
        assert!(e.selected.is_none());
    }

    #[test]
    fn pad_rows_take_a_range_start_and_say_so_when_the_key_cannot_start_one() {
        let mut e = editing("deck.pad.A");
        assert!(
            e.key(&rows(), "9", false, false).is_empty()
                && e.status(&rows()).contains("consecutive")
        );
        let edits = e.key(&rows(), "2", false, false);
        assert_eq!(
            edits,
            [("deck.pad.A".to_owned(), Some(bind("2-9", false, false)))]
        );
    }

    #[test]
    fn clicking_the_selected_row_again_stops_editing() {
        let mut e = editing("deck.cue.A");
        e.select("deck.cue.A");
        assert!(e.selected.is_none());
        e.select("deck.cue.A");
        e.select("deck.cue.B");
        assert_eq!(e.selected.as_deref(), Some("deck.cue.B"));
    }
}
