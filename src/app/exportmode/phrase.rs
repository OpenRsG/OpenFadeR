//! PHRASE EDIT row of the GRID panel ↔ `PhraseTools` in ui/exportmode-grid.slint, the
//! overview's phrase strip (ui/deck-wave.slint `DeckOverview`) and `Deck::phrase_*` (deck/phrase.rs, the edits themselves are
//! `analysis::phrase_edit`). The row's UI state (panel open, CUT lit, selected phrase) lives in `ExportModeState.decks`; the grabbed
//! divider of a drag is the only state kept here. CUT stays lit until clicked again; a click on the strip with CUT off selects
//! the phrase, a press within a few pixels of a divider grabs it; releasing without moving changes nothing.

use super::{Deck, Decks, ExportDeck, ExportModeState, PLAYERS};
use crate::app::{App, PhraseSeg, deck::Hit};
use slint::{Color, ComponentHandle, Model, VecModel};
use std::{cell::Cell, rc::Rc};

/// Copies the deck's phrase state into its row: whether the edit is allowed (`locked`: the Analysis Lock), the selected phrase's name
/// and colour, the phrases' spans. The selection is dropped when its phrase is gone.
pub fn fill(deck: &Deck, row: &mut ExportDeck, locked: bool) {
    let sel = usize::try_from(row.phrase_sel - 1).ok();
    let info = deck.phrase_info(sel, locked);
    row.phrase_enabled = info.enabled;
    if sel.is_some() && info.selected.is_none() {
        row.phrase_sel = 0;
    }
    let (label, [r, g, b]) = info.selected.clone().unwrap_or_default();
    row.phrase_label = label.into();
    row.phrase_color = Color::from_rgb_u8(r, g, b);
    let sel = usize::try_from(row.phrase_sel - 1).ok();
    let segs: Vec<PhraseSeg> = info
        .segs
        .iter()
        .enumerate()
        .map(|(i, &(x0, x1))| PhraseSeg {
            x0,
            x1,
            selected: sel == Some(i),
        })
        .collect();
    if !row.phrase_segs.iter().eq(segs.iter().cloned()) {
        row.phrase_segs = slint::ModelRc::new(VecModel::from(segs));
    }
}

struct Ctx {
    decks: Decks,
    model: Rc<VecModel<ExportDeck>>,
    /// The divider being dragged per player: `(divider, moved)`.
    grab: [Cell<Option<(usize, bool)>>; PLAYERS],
}

impl Ctx {
    fn update(&self, i: usize, f: impl FnOnce(&mut ExportDeck)) {
        if let Some(mut row) = self.model.row_data(i) {
            f(&mut row);
            self.model.set_row_data(i, row);
        }
    }

    /// Row `i` after an edit (the 100 ms refresh would show it too, a click should not wait for that).
    fn sync(&self, i: usize, locked: bool) {
        self.update(i, |r| fill(&self.decks[i], r, locked));
    }

    fn panel(&self, i: usize, open: bool) {
        self.grab[i].set(None);
        self.update(i, |r| {
            r.phrase_panel = open;
            r.phrase_cut &= open;
        });
        self.sync(i, self.decks[i].grid_locked());
    }

    fn press(&self, i: usize, x: f32, tol: f32) {
        let Some(row) = self.model.row_data(i) else {
            return;
        };
        if row.phrase_cut {
            if self.decks[i].phrase_cut(x) {
                self.update(i, |r| r.phrase_sel = 0);
            }
        } else {
            match self.decks[i].phrase_hit(x, tol) {
                Hit::Divider(d) => self.grab[i].set(Some((d, false))),
                Hit::Phrase(p) => self.update(i, |r| r.phrase_sel = p as i32 + 1),
                Hit::Nothing => self.update(i, |r| r.phrase_sel = 0),
            }
        }
        self.sync(i, false);
    }

    fn drag(&self, i: usize, x: f32) {
        if let Some((d, _)) = self.grab[i].get() {
            self.grab[i].set(Some((d, true)));
            self.decks[i].phrase_move(d, x, false);
            self.sync(i, false);
        }
    }

    fn release(&self, i: usize, x: f32) {
        if let Some((d, true)) = self.grab[i].take() {
            self.decks[i].phrase_move(d, x, true);
            self.sync(i, false);
        }
    }

    fn kind(&self, i: usize) {
        if let Ok(sel) = usize::try_from(self.model.row_data(i).map_or(0, |r| r.phrase_sel) - 1) {
            self.decks[i].phrase_next_kind(sel);
            self.sync(i, false);
        }
    }
}

pub fn wire(app: &App, decks: &Decks, model: &Rc<VecModel<ExportDeck>>) {
    let s = app.global::<ExportModeState>();
    let cx = Rc::new(Ctx {
        decks: decks.clone(),
        model: model.clone(),
        grab: Default::default(),
    });
    let idx = |i: i32| i as usize % PLAYERS;
    s.on_phrase_panel({
        let cx = cx.clone();
        move |i, open| cx.panel(idx(i), open)
    });
    s.on_phrase_cut_toggle({
        let cx = cx.clone();
        move |i| cx.update(idx(i), |r| r.phrase_cut = !r.phrase_cut)
    });
    s.on_phrase_clear({
        let cx = cx.clone();
        move |i| {
            let i = idx(i);
            if cx.decks[i].phrase_clear() {
                cx.update(i, |r| r.phrase_sel = 0);
                cx.sync(i, false);
            }
        }
    });
    s.on_phrase_reset({
        let cx = cx.clone();
        move |i| {
            let i = idx(i);
            cx.decks[i].phrase_reset();
            cx.update(i, |r| r.phrase_sel = 0);
            cx.sync(i, false);
        }
    });
    s.on_phrase_kind({
        let cx = cx.clone();
        move |i| cx.kind(idx(i))
    });
    s.on_phrase_press({
        let cx = cx.clone();
        move |i, x, tol| cx.press(idx(i), x, tol)
    });
    s.on_phrase_drag({
        let cx = cx.clone();
        move |i, x| cx.drag(idx(i), x)
    });
    s.on_phrase_release(move |i, x| cx.release(idx(i), x));
}
