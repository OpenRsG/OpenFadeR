//! The track table's cells: what each column shows for a track, the master-deck hint markers, and the
//! reference (the master deck's key and tempo) the markers and the Track Filter compare with.

use crate::app::{
    App, DeckState,
    analyze::detected,
    camelot::{self, Key, camelot, related},
    prefs::{self, KeyNotation, view::TrafficLight},
};
use openfader::library::Track;
use slint::{ComponentHandle, Model, ModelRc, SharedString, StandardListViewItem, VecModel};

/// Table column indices, in the order of the model rows read by `TrackTable` in ui/browser-table.slint.
/// Columns 0..=7 are sortable text; `KEY_HUE` (Camelot number 1..12, "0" none), `HINTS` ("k" key compatible with the
/// master deck, "b" BPM within ±6 %) and `PLAYED` ("1" the track played: its title is dimmed) are not shown as text.
pub const BPM: usize = 4;
pub const TIME: usize = 5;
pub const RATING: usize = 6;
pub const KEY_HUE: usize = 8;
pub const HINTS: usize = 9;
pub const PLAYED: usize = 10;
pub const COLUMNS: usize = 11;
pub const KEY: usize = 3;

/// BPM hint window: ± this fraction of the playing deck's BPM.
const BPM_WINDOW: f64 = 0.06;

/// `bpm` within ±6 % of `reference` (both in BPM, > 0).
pub fn bpm_close(bpm: f64, reference: f64) -> bool {
    reference > 0.0 && bpm > 0.0 && (bpm - reference).abs() <= reference * BPM_WINDOW
}

/// A track's (BPM × 100, key): the library value, else the one bulk analysis detected.
pub fn bpm_key(t: &Track) -> (i64, String) {
    detected().fill(t)
}

/// What a track's BPM and key resolve to, computed once per track and passed on instead of recomputed per cell, filter test and compare.
pub struct Facts {
    pub bpm_x100: i64,
    pub bpm: f64,
    pub key: String,
    pub wheel: Option<Key>,
}

impl Facts {
    pub fn of(t: &Track) -> Self {
        let (bpm_x100, key) = bpm_key(t);
        Facts {
            bpm_x100,
            bpm: bpm_x100 as f64 / 100.0,
            wheel: camelot(&key),
            key,
        }
    }
}

/// The settings the cells and key tests depend on, read once per rebuild.
#[derive(Clone, Copy)]
pub struct Look {
    pub notation: KeyNotation,
    pub level: TrafficLight,
    /// Dim the titles of tracks that played.
    pub played: bool,
}

impl Look {
    pub fn now() -> Self {
        prefs::with(|p| Look {
            notation: p.key_notation,
            level: p.view.traffic_light,
            played: p.view.played_color,
        })
    }
}

/// What the master deck offers to mix with: its effective Camelot key and BPM.
#[derive(Clone, Default, PartialEq)]
pub struct Reference {
    pub key: Option<Key>,
    pub bpm: f64,
}

/// Reference from the master deck (last started) while it holds a track, else any playing deck.
pub fn reference(app: &App) -> Reference {
    let state = app.global::<DeckState>();
    let decks = state.get_decks();
    let master = usize::try_from(state.get_master()).ok();
    let pick = master
        .filter(|&m| decks.row_data(m).is_some_and(|d| d.loaded))
        .or_else(|| (0..decks.row_count()).find(|&i| decks.row_data(i).is_some_and(|d| d.playing)));
    pick.and_then(|i| decks.row_data(i))
        .map_or_else(Reference::default, |d| Reference {
            key: camelot(&d.key_eff),
            // Rounded to 0.1 so a tempo drag doesn't re-mark every row each frame.
            bpm: (d.bpm as f64 * 10.0).round() / 10.0,
        })
}

/// Text of column `col` (0..=7) for `t`.
pub fn cell_of(t: &Track, f: &Facts, col: usize, look: &Look) -> String {
    match col {
        0 => t.title.clone(),
        1 => t.artist.clone(),
        2 => t.genre.clone(),
        KEY => camelot::display(&f.key, look.notation),
        BPM if f.bpm_x100 > 0 => format!("{:.2}", f.bpm),
        BPM => String::new(),
        TIME => t.length_text(),
        RATING => "★".repeat(t.rating.into()),
        _ => t.comment.clone(),
    }
}

/// [`cell_of`] for callers that format a few cells (sorting a playlist, a palette line).
pub fn cell(t: &Track, col: usize) -> String {
    cell_of(t, &Facts::of(t), col, &Look::now())
}

/// The HINTS cell: "k" when the key mixes with the master deck's, "b" when the BPM is within the window.
fn hints(f: &Facts, r: &Reference, level: TrafficLight) -> &'static str {
    let key = r
        .key
        .zip(f.wheel)
        .is_some_and(|(a, b)| related(a, b, level));
    match (key, bpm_close(f.bpm, r.bpm)) {
        (true, true) => "kb",
        (true, false) => "k",
        (false, true) => "b",
        (false, false) => "",
    }
}

/// Displayed cell. The master-deck hints are carried in the `HINTS` cell and tinted by the table. The text columns convert
/// straight from the track's own strings (no intermediate `String`).
fn shown_cell(t: &Track, f: &Facts, col: usize, r: &Reference, look: &Look) -> SharedString {
    match col {
        KEY_HUE => f.wheel.map_or(0, |k| k.0).to_string().as_str().into(),
        HINTS => hints(f, r, look.level).into(),
        PLAYED => SharedString::new(), // filled in by [`row`]
        // A track whose file is gone is marked "[!]" (File > Display All Missing Files lists them).
        0 if t
            .file
            .as_deref()
            .is_some_and(crate::app::library::is_missing) =>
        {
            format!("[!] {}", t.title).as_str().into()
        }
        0 => t.title.as_str().into(),
        1 => t.artist.as_str().into(),
        2 => t.genre.as_str().into(),
        7 => t.comment.as_str().into(),
        _ => cell_of(t, f, col, look).as_str().into(),
    }
}

/// The table row of `t`, whose [`Facts`] are `f`.
pub fn row(
    t: &Track,
    f: &Facts,
    r: &Reference,
    played: bool,
    look: &Look,
) -> ModelRc<StandardListViewItem> {
    let cells: [StandardListViewItem; COLUMNS] = std::array::from_fn(|c| match c {
        PLAYED if played => "1".into(),
        c => shown_cell(t, f, c, r, look).into(),
    });
    ModelRc::new(VecModel::from(Vec::from(cells)))
}

/// Updates only the HINTS cell of an existing `row` for a new reference; the model notifies when the text changed.
pub fn remark(row: &ModelRc<StandardListViewItem>, t: &Track, r: &Reference, look: &Look) {
    let h = hints(&Facts::of(t), r, look.level);
    if row.row_data(HINTS).is_some_and(|c| c.text != h) {
        row.set_row_data(HINTS, h.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_window_is_six_percent() {
        assert!(bpm_close(130.0, 125.0) && bpm_close(118.0, 125.0));
        assert!(!bpm_close(133.0, 125.0) && !bpm_close(120.0, 0.0) && !bpm_close(0.0, 120.0));
    }
}
