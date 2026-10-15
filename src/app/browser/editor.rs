//! Smart playlist editor (the panel over the table; `editor-*` / `cond-*` in ui/browser.slint). Opens for a new smart
//! playlist at once and from EDIT. One row per condition: a kind chip (click cycles through [`smart::Cond`]'s six
//! kinds) and two text boxes. The rows live here (`Editing`), typing only updates them; SAVE checks every row
//! ([`smart::Cond::from_fields`]) and writes [`smart::Smart::to_text`] to the store, nothing is saved before that.

use super::{App, BrowserState, Entry, View, smart::{Cond, Smart}};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc};

/// More rows than this do not fit the panel.
const MAX_CONDS: usize = 8;
/// Number of condition kinds the kind chip cycles through.
const KINDS: i32 = 6;

/// (kind, first box, second box) per condition.
type Fields = (i32, String, String);

pub struct Editing {
    id: i64,
    name: String,
    rows: Vec<Fields>,
}

/// Rows of the editor for `smart`; an empty smart playlist starts with one blank BPM row.
pub fn to_rows(smart: &Smart, tag_name: impl Fn(i64) -> String) -> Vec<Fields> {
    let mut rows: Vec<Fields> = smart.conds.iter().map(|c| c.fields(&tag_name)).collect();
    if rows.is_empty() {
        rows.push((0, String::new(), String::new()));
    }
    rows
}

/// The smart playlist the rows describe; the error names the first bad row.
pub fn from_rows(rows: &[Fields], any: bool, tag_id: impl Fn(&str) -> Option<i64>) -> Result<Smart, String> {
    if rows.is_empty() {
        return Err("add at least one condition".into());
    }
    let conds = rows.iter().enumerate().map(|(n, (k, a, b))| Cond::from_fields(*k, a, b, &tag_id).map_err(|e| format!("condition {}: {e}", n + 1))).collect::<Result<_, _>>()?;
    Ok(Smart { any, conds })
}

impl View {
    fn tag_name(&self, id: i64) -> String {
        self.mytags.iter().find(|t| t.id == id).map_or_else(|| "?".to_owned(), |t| t.name.clone())
    }

    pub(super) fn open_editor(&mut self, app: &App, id: i64) {
        let Some(name) = self.nodes.iter().find(|n| n.id == id).map(|n| n.name.clone()) else { return };
        let text = self.ok(app, self.store.smart(id), None);
        let smart = match text.as_deref().map(Smart::parse).transpose() {
            Ok(s) => s.unwrap_or_default(),
            Err(e) => {
                self.say(app, e);
                Smart::default()
            }
        };
        app.global::<BrowserState>().set_editor_any(smart.any);
        self.editing = Some(Editing { id, name, rows: to_rows(&smart, |t| self.tag_name(t)) });
        self.editor_sync(app, "");
        app.global::<BrowserState>().set_editor_open(true);
    }

    /// Pushes the rows into the Slint models (after a structural change; typing needs no push).
    fn editor_sync(&self, app: &App, error: &str) {
        let (s, Some(ed)) = (app.global::<BrowserState>(), &self.editing) else { return };
        s.set_editor_title(ed.name.as_str().into());
        s.set_editor_error(error.into());
        let strings = |f: fn(&Fields) -> &String| ModelRc::from(Rc::new(VecModel::from(ed.rows.iter().map(|r| SharedString::from(f(r).as_str())).collect::<Vec<_>>())));
        s.set_cond_kinds(ModelRc::from(Rc::new(VecModel::from(ed.rows.iter().map(|r| r.0).collect::<Vec<_>>()))));
        s.set_cond_a(strings(|r| &r.1));
        s.set_cond_b(strings(|r| &r.2));
    }

    fn editor_close(&mut self, app: &App) {
        self.editing = None;
        app.global::<BrowserState>().set_editor_open(false);
    }

    fn editor_save(&mut self, app: &App) {
        let Some(ed) = &self.editing else { return };
        let tag_id = |name: &str| {
            let name = name.to_lowercase();
            self.mytags.iter().find(|t| t.name.to_lowercase() == name).map(|t| t.id)
        };
        let any = app.global::<BrowserState>().get_editor_any();
        let id = ed.id;
        let saved = from_rows(&ed.rows, any, tag_id).and_then(|s| self.store.set_smart(id, &s.to_text()));
        match saved {
            Err(e) => self.editor_sync(app, &e),
            Ok(()) => {
                self.editor_close(app);
                if self.entry == Entry::User(id) {
                    self.show(app);
                }
            }
        }
    }
}

pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>) {
    let state = app.global::<BrowserState>();
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_cond_kind(move |i| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        if let Some(r) = v.editing.as_mut().and_then(|e| e.rows.get_mut(i as usize)) {
            r.0 = (r.0 + 1) % KINDS;
        }
        v.editor_sync(&app, "");
    });
    let v = view.clone();
    state.on_cond_edit(move |i, which, text| {
        if let Some(r) = v.borrow_mut().editing.as_mut().and_then(|e| e.rows.get_mut(i as usize)) {
            *(if which == 0 { &mut r.1 } else { &mut r.2 }) = text.to_string();
        }
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_cond_add(move || {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        let full = v.editing.as_ref().is_some_and(|e| e.rows.len() >= MAX_CONDS);
        if let Some(ed) = v.editing.as_mut().filter(|_| !full) {
            ed.rows.push((0, String::new(), String::new()));
        }
        v.editor_sync(&app, if full { "that is the most conditions the editor holds" } else { "" });
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_cond_remove(move |i| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        if let Some(ed) = v.editing.as_mut().filter(|e| (i as usize) < e.rows.len()) {
            ed.rows.remove(i as usize);
        }
        v.editor_sync(&app, "");
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_editor_save(move || v.borrow_mut().editor_save(&weak.unwrap()));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_editor_cancel(move || v.borrow_mut().editor_close(&weak.unwrap()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_become_a_smart_playlist_and_back() {
        let tag = |n: &str| (n == "Peak").then_some(4);
        let rows = vec![(0, "120".to_owned(), "130".to_owned()), (4, "Peak".to_owned(), String::new()), (3, "house".to_owned(), String::new())];
        let s = from_rows(&rows, true, tag).unwrap();
        assert_eq!(s.conds, [Cond::Bpm { min: 120.0, max: 130.0 }, Cond::Tag(4), Cond::Genre("house".into())]);
        assert!(s.any);
        assert_eq!(to_rows(&s, |_| "Peak".to_owned()), rows);
    }

    #[test]
    fn the_first_bad_row_is_named_and_empty_is_refused() {
        let rows = vec![(2, "3".to_owned(), String::new()), (1, "nonsense".to_owned(), String::new())];
        assert!(from_rows(&rows, false, |_| None).unwrap_err().starts_with("condition 2:"));
        assert!(from_rows(&[], false, |_| None).is_err());
    }

    #[test]
    fn an_empty_smart_playlist_opens_with_one_blank_row() {
        assert_eq!(to_rows(&Smart::default(), |_| String::new()), [(0, String::new(), String::new())]);
    }
}
