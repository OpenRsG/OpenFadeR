//! The editor page: draws the keymap into `KeymapState` and routes clicks and key presses to the [`Editor`].

use super::Editor;
use crate::app::keys::{self, Keymap, Preset};
use crate::app::prefs::{get, on_change, update};
use crate::app::{App, HelpLine, KeymapState, PrefsState};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use std::{cell::RefCell, rc::Rc};

/// `m` shows `v` (rows that did not change are left alone, so the list keeps its scroll position).
fn sync<T: Clone + PartialEq + 'static>(m: &VecModel<T>, v: Vec<T>) {
    if m.row_count() != v.len() {
        return m.set_vec(v);
    }
    for (i, x) in v.into_iter().enumerate() {
        if m.row_data(i).as_ref() != Some(&x) {
            m.set_row_data(i, x);
        }
    }
}

/// Edits the stored map (and so, through `on_change`, the live table and this page).
fn edit(f: impl FnOnce(&mut Keymap)) {
    update(|p| {
        let mut km = Keymap::parse(&p.keyboard.keymap).0;
        f(&mut km);
        p.keyboard.keymap = km.to_text();
    });
}

struct Page {
    app: Weak<App>,
    editor: RefCell<Editor>,
    rows: Rc<VecModel<HelpLine>>,
    custom: Rc<VecModel<bool>>,
    /// The map the page was last drawn for.
    drawn: RefCell<String>,
}

impl Page {
    fn render(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let map = get().keyboard.keymap;
        let km = Keymap::parse(&map).0;
        let rows = km.rows();
        let lines = rows.iter().map(|r| HelpLine {
            keys: r.bind.as_ref().map_or(String::new(), keys::label).into(),
            what: keys::name(r).into(),
        });
        sync(&self.rows, lines.collect());
        sync(&self.custom, rows.iter().map(|r| km.is_custom(r)).collect());
        let ed = self.editor.borrow();
        let s = app.global::<KeymapState>();
        s.set_selected(
            ed.selected
                .as_ref()
                .and_then(|id| rows.iter().position(|r| r.id == *id))
                .map_or(-1, |i| i as i32),
        );
        s.set_status(ed.status(&rows).into());
        s.set_conflict(ed.pending.is_some());
        s.set_preset(
            Preset::ALL
                .iter()
                .position(|p| *p == km.preset)
                .unwrap_or(0) as i32,
        );
        *self.drawn.borrow_mut() = map;
    }

    /// A key press (from the key hook or the CLEAR / REPLACE / CANCEL buttons); true = the editor took it.
    fn press(&self, text: &str, shift: bool, ctrl: bool) -> bool {
        if self.editor.borrow().selected.is_none() {
            return false;
        }
        let rows = Keymap::parse(&get().keyboard.keymap).0.rows();
        let edits = self.editor.borrow_mut().key(&rows, text, shift, ctrl);
        if !edits.is_empty() {
            edit(|km| edits.into_iter().for_each(|(id, b)| km.set(&id, b)));
        }
        self.render();
        true
    }

    /// The dialog is not on this page: nothing is waiting for a key.
    fn visible(&self) -> bool {
        let shown = self.app.upgrade().is_some_and(|a| {
            let s = a.global::<PrefsState>();
            s.get_visible() && s.get_category() == 4
        });
        if !shown && self.editor.borrow().selected.is_some() {
            self.editor.borrow_mut().cancel();
            self.render();
        }
        shown
    }
}

/// Connects the editor page.
pub fn wire(app: &App) {
    let s = app.global::<KeymapState>();
    let titles: Vec<slint::SharedString> = Preset::ALL.iter().map(|p| p.title().into()).collect();
    s.set_presets(ModelRc::new(VecModel::from(titles)));
    let page = Rc::new(Page {
        app: app.as_weak(),
        editor: RefCell::default(),
        rows: Rc::default(),
        custom: Rc::default(),
        drawn: RefCell::default(),
    });
    s.set_rows(page.rows.clone().into());
    s.set_custom(page.custom.clone().into());
    page.render();

    let p = page.clone();
    s.on_select(move |i| {
        let rows = Keymap::parse(&get().keyboard.keymap).0.rows();
        if let Some(r) = rows.get(i.max(0) as usize) {
            p.editor.borrow_mut().select(&r.id);
        }
        p.render();
    });
    let p = page.clone();
    s.on_key_captured(move |text, shift, ctrl| {
        p.press(&text, shift, ctrl);
    });
    let p = page.clone();
    s.on_preset_chosen(move |i| {
        p.editor.borrow_mut().cancel();
        if let Some(&preset) = Preset::ALL.get(i.max(0) as usize) {
            edit(|km| km.use_preset(preset));
        }
        p.render();
    });
    let p = page.clone();
    s.on_reset(move || {
        p.editor.borrow_mut().cancel();
        edit(|km| km.use_preset(Preset::Standard));
        p.render();
    });
    let p = page.clone();
    keys::set_hook(move |text, shift, ctrl| p.visible() && p.press(text, shift, ctrl));
    on_change(move |prefs| {
        if *page.drawn.borrow() != prefs.keyboard.keymap {
            page.render();
        }
    });
}
