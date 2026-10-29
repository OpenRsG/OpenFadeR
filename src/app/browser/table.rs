//! The track table's cells: what each column shows for a track, the master-deck hint markers, and the
//! reference (the master deck's key and tempo) the markers and the Track Filter compare with.

use crate::app::{
    App, DeckState,
    analyze::detected,
    browser::filter::{self, Filter},
    camelot::{self, Key, camelot, related},
    prefs::{self, KeyNotation, view::TrafficLight},
};
use openfader::library::Track;
use slint::{
    ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString, StandardListViewItem,
    VecModel,
};
use std::{
    cell::{Ref, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::{Rc, Weak},
    sync::Arc,
};

/// Table column indices, in the order of the model rows read by `TrackTable` in ui/browser-table.slint.
/// Columns 0..=7 are sortable text; `KEY_HUE` (Camelot number 1..12, "0" none), `HINTS` ("k" key compatible with the
/// master deck, "b" BPM within ±6 %), `PLAYED` ("1" the track played: its title is dimmed) and `STEM_FILE` ("1" the file
/// is a stem file: its title wears a badge) are not shown as text.
pub const BPM: usize = 4;
pub const TIME: usize = 5;
pub const RATING: usize = 6;
pub const KEY_HUE: usize = 8;
pub const HINTS: usize = 9;
pub const PLAYED: usize = 10;
pub const STEM_FILE: usize = 11;
/// Album and the day the track was added (`YYYY-MM-DD`): columns of the Stage track list (ui/stage/table.slint); the desktop and
/// studio lists do not show them.
pub const ALBUM: usize = 12;
pub const ADDED: usize = 13;
pub const TRACK_ID: usize = 14;
pub const COLUMNS: usize = 15;
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
#[derive(Clone, Copy, PartialEq)]
pub struct Look {
    pub notation: KeyNotation,
    pub level: TrafficLight,
    pub tempo_digits: usize,
    /// Dim the titles of tracks that played.
    pub played: bool,
}

impl Look {
    pub fn now() -> Self {
        prefs::with(|p| Look {
            notation: p.key_notation,
            level: p.view.traffic_light,
            tempo_digits: p.view.tempo_decimals.digits(),
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

/// Text of column `col` (0..=7, `ALBUM`, `ADDED`) for `t`.
pub fn cell_of(t: &Track, f: &Facts, col: usize, look: &Look) -> String {
    match col {
        0 => t.title.clone(),
        1 => t.artist.clone(),
        2 => t.genre.clone(),
        KEY => camelot::display(&f.key, look.notation),
        BPM if f.bpm_x100 > 0 => format!("{:.*}", look.tempo_digits, f.bpm),
        BPM => String::new(),
        TIME => t.length_text(),
        ALBUM => t.album.clone(),
        ADDED => t.date_added.get(..10).unwrap_or(&t.date_added).to_owned(),
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
        TRACK_ID => crate::app::track_identity(t).into(),
        HINTS => hints(f, r, look.level).into(),
        PLAYED => SharedString::new(), // filled in by [`row`]
        STEM_FILE
            if t.file
                .as_deref()
                .is_some_and(openfader::audio::stemfile::is_stem_name) =>
        {
            "1".into()
        }
        STEM_FILE => SharedString::new(),
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
        ALBUM => t.album.as_str().into(),
        _ => cell_of(t, f, col, look).as_str().into(),
    }
}

#[cfg(test)]
/// The table row of `t`, whose [`Facts`] are `f`.
pub fn row(
    t: &Track,
    f: &Facts,
    r: &Reference,
    played: bool,
    look: &Look,
) -> ModelRc<StandardListViewItem> {
    ModelRc::new(VecModel::from(cells(t, f, r, played, look)))
}

fn cells(
    t: &Track,
    f: &Facts,
    r: &Reference,
    played: bool,
    look: &Look,
) -> Vec<StandardListViewItem> {
    Vec::from(std::array::from_fn::<_, COLUMNS, _>(|c| match c {
        PLAYED if played => "1".into(),
        c => shown_cell(t, f, c, r, look).into(),
    }))
}

/// Updates only the HINTS cell; facts remain valid when just the reference changes.
fn remark(row: &ModelRc<StandardListViewItem>, f: &Facts, r: &Reference, look: &Look) {
    let h = hints(f, r, look.level);
    if row.row_data(HINTS).is_some_and(|c| c.text != h) {
        row.set_row_data(HINTS, h.into());
    }
}

// This limits speculative ownership, not the number of rows Slint may keep alive.
const HOT_ROWS: usize = 64;

struct CachedRow {
    index: usize,
    model: VecModel<StandardListViewItem>,
    retired: Rc<RefCell<Vec<(usize, *const CachedRow)>>>,
}

impl Drop for CachedRow {
    fn drop(&mut self) {
        self.retired
            .borrow_mut()
            .push((self.index, self as *const Self));
    }
}

impl Model for CachedRow {
    type Data = StandardListViewItem;

    fn row_count(&self) -> usize {
        self.model.row_count()
    }

    fn row_data(&self, row: usize) -> Option<Self::Data> {
        self.model.row_data(row)
    }

    fn set_row_data(&self, row: usize, value: Self::Data) {
        self.model.set_row_data(row, value);
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        self.model.model_tracker()
    }
}

#[derive(Default)]
struct RowCache {
    live: HashMap<usize, Weak<CachedRow>>,
    hot: VecDeque<Rc<CachedRow>>,
    retired: Rc<RefCell<Vec<(usize, *const CachedRow)>>>,
}

impl RowCache {
    fn prune(&mut self) {
        // A separate queue also makes model-notification drops safe while RowData is borrowed.
        let mut retired = self.retired.borrow_mut();
        for (i, ptr) in retired.drain(..) {
            if self.live.get(&i).is_some_and(|weak| weak.as_ptr() == ptr) {
                self.live.remove(&i);
            }
        }
        if retired.capacity() > HOT_ROWS * 4 {
            retired.shrink_to(HOT_ROWS);
        }
        if self.live.capacity() > (self.live.len() + HOT_ROWS) * 4 {
            self.live.shrink_to(self.live.len() + HOT_ROWS);
        }
    }

    fn insert(
        &mut self,
        i: usize,
        cells: Vec<StandardListViewItem>,
    ) -> ModelRc<StandardListViewItem> {
        let row = Rc::new(CachedRow {
            index: i,
            model: VecModel::from(cells),
            retired: self.retired.clone(),
        });
        self.live.insert(i, Rc::downgrade(&row));
        if self.hot.len() == HOT_ROWS {
            self.hot.pop_front();
        }
        self.hot.push_back(row.clone());
        self.prune();
        row.into()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.live.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

struct Sorted {
    column: usize,
    ascending: bool,
    indices: Vec<usize>,
}

struct RowData {
    list: Arc<Vec<Track>>,
    facts: Vec<Facts>,
    detected: u64,
    sorted: Option<Sorted>,
    #[cfg(test)]
    sort_builds: usize,
    missing: u64,
    shown: Vec<usize>,
    notes: Vec<String>,
    played: HashSet<String>,
    reference: Reference,
    look: Look,
    cells: RowCache,
    dirty: bool,
}

impl RowData {
    fn formatted(&self, i: usize) -> Vec<StandardListViewItem> {
        let t = &self.list[i];
        let played = self.look.played
            && !self.played.is_empty()
            && self.played.contains(&openfader::store::key(t));
        let mut cells = cells(t, &self.facts[i], &self.reference, played, &self.look);
        if let Some(note) = self
            .notes
            .get(i)
            .filter(|_| self.notes.len() == self.list.len())
        {
            cells[7] = note.as_str().into();
        }
        cells
    }

    fn refresh_cells(&mut self) {
        self.cells.prune();
        for (&i, weak) in &self.cells.live {
            if let Some(row) = weak.upgrade() {
                // Removed indices reset their old handles, even before the outer model resets.
                row.model.set_vec(if i < self.list.len() {
                    self.formatted(i)
                } else {
                    Vec::new()
                });
            }
        }
    }
}

/// The outer model survives filtering; only requested rows allocate cell models.
pub(super) struct Rows {
    data: RefCell<RowData>,
    notify: ModelNotify,
}

impl Rows {
    pub fn new(played: HashSet<String>) -> Self {
        Self {
            data: RefCell::new(RowData {
                list: Arc::default(),
                facts: Vec::new(),
                detected: detected().version(),
                sorted: None,
                #[cfg(test)]
                sort_builds: 0,
                missing: crate::app::library::missing::version(),
                shown: Vec::new(),
                notes: Vec::new(),
                played,
                reference: Reference::default(),
                look: Look::now(),
                cells: RowCache::default(),
                dirty: false,
            }),
            notify: ModelNotify::default(),
        }
    }

    pub fn replace(&self, list: Arc<Vec<Track>>, notes: Vec<String>) {
        let mut d = self.data.borrow_mut();
        if !Arc::ptr_eq(&list, &d.list) {
            d.sorted = None;
        }
        if list.len() != d.list.len() {
            d.facts = list.iter().map(Facts::of).collect();
        } else {
            for (i, t) in list.iter().enumerate() {
                let old = &d.list[i];
                if t.bpm_x100 != old.bpm_x100 || t.key != old.key || t.file != old.file {
                    d.facts[i] = Facts::of(t);
                }
            }
        }
        d.list = list;
        d.notes = notes;
        d.refresh_cells();
        d.dirty = true;
    }

    pub fn facts(&self) -> Ref<'_, [Facts]> {
        let version = detected().version();
        {
            let mut d = self.data.borrow_mut();
            if d.detected != version {
                for i in 0..d.list.len() {
                    let t = &d.list[i];
                    if t.bpm_x100 <= 0 || t.key.is_empty() {
                        d.facts[i] = Facts::of(t);
                    }
                }
                d.detected = version;
                if d.sorted
                    .as_ref()
                    .is_some_and(|s| s.column == BPM || s.column == KEY)
                {
                    d.sorted = None;
                }
                d.refresh_cells();
                d.dirty = true;
            }
        }
        Ref::map(self.data.borrow(), |d| d.facts.as_slice())
    }

    /// Reuses the full-list stable order; query, tags and reference only remove rows from it.
    pub fn shown(
        &self,
        query: &str,
        filter: &Filter,
        reference: &Reference,
        look: &Look,
        sort: Option<(usize, bool)>,
        tags: impl Fn(&Track) -> bool,
    ) -> Vec<usize> {
        drop(self.facts());
        let mut d = self.data.borrow_mut();
        if let Some((column, ascending)) = sort {
            if !d
                .sorted
                .as_ref()
                .is_some_and(|s| s.column == column && s.ascending == ascending)
            {
                let mut indices: Vec<_> = (0..d.list.len()).collect();
                filter::sort(&mut indices, &d.list, &d.facts, column, ascending, look);
                d.sorted = Some(Sorted {
                    column,
                    ascending,
                    indices,
                });
                #[cfg(test)]
                {
                    d.sort_builds += 1;
                }
            }
            filter::shown_in_order(
                &d.list,
                &d.facts,
                d.sorted.as_ref().unwrap().indices.iter().copied(),
                query,
                filter,
                reference,
                look,
                tags,
            )
        } else {
            filter::shown(&d.list, &d.facts, query, filter, reference, look, tags)
        }
    }

    pub fn invalidate(&self) {
        let mut d = self.data.borrow_mut();
        d.refresh_cells();
        d.dirty = true;
    }

    pub fn update(&self, shown: &[usize], look: Look, reference: Reference) -> bool {
        let reset = {
            let mut d = self.data.borrow_mut();
            d.cells.prune();
            let missing = crate::app::library::missing::version();
            if d.missing != missing {
                d.missing = missing;
                d.refresh_cells();
                d.dirty = true;
            }
            if d.look != look {
                d.look = look;
                d.refresh_cells();
                d.dirty = true;
            }
            if d.reference != reference {
                d.cells.prune();
                for (&i, weak) in &d.cells.live {
                    if let (Some(row), Some(f)) = (weak.upgrade(), d.facts.get(i)) {
                        remark(&row.into(), f, &reference, &look);
                    }
                }
                d.reference = reference;
            }
            let changed = d.shown != shown;
            if changed {
                d.shown.clear();
                d.shown.extend_from_slice(shown);
            }
            std::mem::take(&mut d.dirty) || changed
        };
        if reset {
            self.notify.reset();
        }
        reset
    }

    pub fn remark(&self, reference: Reference) {
        let mut d = self.data.borrow_mut();
        d.cells.prune();
        for (&i, weak) in &d.cells.live {
            if let (Some(row), Some(f)) = (weak.upgrade(), d.facts.get(i)) {
                remark(&row.into(), f, &reference, &d.look);
            }
        }
        d.reference = reference;
    }

    pub fn mark_played(&self, key: &str) -> bool {
        let mut d = self.data.borrow_mut();
        if !d.played.insert(key.to_owned()) {
            return false;
        }
        d.cells.prune();
        if d.look.played {
            for (&i, weak) in &d.cells.live {
                if let (Some(row), Some(t)) = (weak.upgrade(), d.list.get(i)) {
                    if openfader::store::key(t) == key {
                        row.set_row_data(PLAYED, "1".into());
                    }
                }
            }
        }
        true
    }

    pub fn reset_played(&self) {
        let mut d = self.data.borrow_mut();
        d.played.clear();
        d.cells.prune();
        for row in d.cells.live.values().filter_map(Weak::upgrade) {
            row.set_row_data(PLAYED, "".into());
        }
    }
}

impl Model for Rows {
    type Data = ModelRc<StandardListViewItem>;

    fn row_count(&self) -> usize {
        self.data.borrow().shown.len()
    }

    fn row_data(&self, row_index: usize) -> Option<Self::Data> {
        let mut d = self.data.borrow_mut();
        d.cells.prune();
        let &i = d.shown.get(row_index)?;
        if let Some(row) = d.cells.live.get(&i).and_then(Weak::upgrade) {
            return Some(row.into());
        }
        let cells = d.formatted(i);
        Some(d.cells.insert(i, cells))
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
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

    #[test]
    fn bpm_display_precision_preserves_facts_and_unknown_values() {
        let t = Track {
            bpm_x100: 12696,
            ..Default::default()
        };
        let f = Facts::of(&t);
        let mut look = Look::now();
        look.tempo_digits = 1;
        assert_eq!(cell_of(&t, &f, BPM, &look), "127.0");
        look.tempo_digits = 2;
        assert_eq!(cell_of(&t, &f, BPM, &look), "126.96");
        assert_eq!(f.bpm_x100, 12696);
        let unknown = Track::default();
        assert_eq!(cell_of(&unknown, &Facts::of(&unknown), BPM, &look), "");
    }

    #[test]
    fn album_and_the_day_added_are_cells_and_sort_like_text() {
        let t = Track {
            album: "Stage Demo".into(),
            date_added: "2026-10-24 18:11:00".into(),
            ..Track::default()
        };
        let (f, look) = (Facts::of(&t), Look::now());
        assert_eq!(cell_of(&t, &f, ALBUM, &look), "Stage Demo");
        assert_eq!(cell_of(&t, &f, ADDED, &look), "2026-10-24", "the day only");
        let row = row(&t, &f, &Reference::default(), false, &look);
        assert_eq!(row.row_count(), COLUMNS);
        assert_eq!(texts(&row)[ALBUM], "Stage Demo");
        assert_eq!(texts(&row)[ADDED], "2026-10-24");
        let (a, b) = (Track { date_added: "2026-01-02".into(), ..Track::default() }, t);
        let list = [b, a];
        let facts: Vec<Facts> = list.iter().map(Facts::of).collect();
        let mut shown = vec![0, 1];
        super::super::filter::sort(&mut shown, &list, &facts, ADDED, true, &look);
        assert_eq!(shown, [1, 0]);
    }

    fn tracks(count: usize) -> Arc<Vec<Track>> {
        Arc::new(
            (0..count)
                .map(|i| Track {
                    title: format!("track {i:05}"),
                    artist: format!("artist {}", i % 7),
                    genre: "House".into(),
                    comment: format!("comment {i}"),
                    bpm_x100: 12000 + (i % 100) as i64,
                    key: if i % 2 == 0 { "8A" } else { "3B" }.into(),
                    rating: (i % 6) as u8,
                    length_s: 180 + (i % 60) as i64,
                    ..Track::default()
                })
                .collect(),
        )
    }

    fn texts(row: &ModelRc<StandardListViewItem>) -> Vec<String> {
        row.iter().map(|cell| cell.text.to_string()).collect()
    }
    fn eager_shown(
        list: &[Track],
        query: &str,
        filter: &Filter,
        reference: &Reference,
        look: &Look,
        sort: Option<(usize, bool)>,
        tags: impl Fn(&Track) -> bool,
    ) -> Vec<usize> {
        let facts: Vec<_> = list.iter().map(Facts::of).collect();
        let mut shown = filter::shown(list, &facts, query, filter, reference, look, tags);
        if let Some((column, ascending)) = sort {
            filter::sort(&mut shown, list, &facts, column, ascending, look);
        }
        shown
    }

    #[test]
    fn hundred_thousand_rows_bound_retention_without_losing_live_identity_or_updates() {
        let list = tracks(100_000);
        let model = Rows::new(HashSet::new());
        let mut look = Look::now();
        look.played = true;
        model.replace(list.clone(), Vec::new());
        let shown: Vec<_> = (0..list.len()).collect();
        model.update(&shown, look, Reference::default());
        let held: Vec<_> = [0, 7, 23]
            .into_iter()
            .map(|i| (i, model.row_data(i).unwrap()))
            .collect();
        for i in 0..list.len() {
            let row = model.row_data(i).unwrap();
            assert_eq!(row.row_data(0).unwrap().text, list[i].title);
            let d = model.data.borrow();
            assert!(d.cells.hot.len() <= HOT_ROWS);
            assert!(d.cells.live.len() <= HOT_ROWS + held.len());
            assert!(d.cells.retired.borrow().is_empty());
        }
        for (i, row) in &held {
            assert_eq!(*row, model.row_data(*i).unwrap());
        }
        let reference = Reference {
            key: camelot("8A"),
            bpm: 120.0,
        };
        model.remark(reference.clone());
        for (i, row) in &held {
            assert_eq!(
                texts(row),
                texts(&super::row(
                    &list[*i],
                    &Facts::of(&list[*i]),
                    &reference,
                    false,
                    &look
                ))
            );
            assert!(model.mark_played(&openfader::store::key(&list[*i])));
            assert_eq!(row.row_data(PLAYED).unwrap().text, "1");
        }
        model.reset_played();
        for (_, row) in &held {
            assert_eq!(row.row_data(PLAYED).unwrap().text, "");
        }
        let d = model.data.borrow();
        assert!(d.cells.live.capacity() <= (HOT_ROWS + held.len()) * 4);
        assert!(d.cells.retired.borrow().capacity() <= HOT_ROWS * 4);
    }

    #[test]
    fn evicted_live_rows_refresh_detected_look_metadata_notes_and_reset() {
        let mut list = tracks(HOT_ROWS * 3).as_ref().clone();
        list[0].bpm_x100 = 0;
        list[0].key.clear();
        list[0].file =
            Some(format!("/openfader-live-row-detected-{}.wav", std::process::id()).into());
        let model = Rows::new(HashSet::new());
        let mut look = Look::now();
        look.tempo_digits = 2;
        look.notation = KeyNotation::Camelot;
        look.played = true;
        let shown: Vec<_> = (0..list.len()).collect();
        model.replace(
            Arc::new(list.clone()),
            vec!["suggestion".into(); list.len()],
        );
        model.update(&shown, look, Reference::default());
        let first = model.row_data(0).unwrap();
        let second = model.row_data(1).unwrap();
        let removed = model.row_data(2).unwrap();
        for i in 3..list.len() {
            drop(model.row_data(i));
        }
        assert!(
            !model
                .data
                .borrow()
                .cells
                .hot
                .iter()
                .any(|row| row.index < 3)
        );
        detected().remember(&list[0], (12825, "9A".into()));
        drop(model.facts());
        assert_eq!(first.row_data(BPM).unwrap().text, "128.25");
        assert_eq!(first.row_data(KEY).unwrap().text, "9A");
        look.tempo_digits = 1;
        look.notation = KeyNotation::Classic;
        model.update(&shown, look, Reference::default());
        assert_eq!(first.row_data(BPM).unwrap().text, "128.2");
        assert_eq!(
            first.row_data(KEY).unwrap().text,
            camelot::display("9A", look.notation)
        );
        assert_eq!(first, model.row_data(0).unwrap());
        model.mark_played(&openfader::store::key(&list[0]));
        assert_eq!(first.row_data(PLAYED).unwrap().text, "1");
        look.played = false;
        model.update(&shown, look, Reference::default());
        assert_eq!(first.row_data(PLAYED).unwrap().text, "");
        list[0].title = "edited title".into();
        list[1].comment = "edited comment".into();
        list[1].album = "edited album".into();
        model.replace(
            Arc::new(list.clone()),
            vec!["edited suggestion".into(); list.len()],
        );
        model.update(&shown, look, Reference::default());
        assert_eq!(first.row_data(0).unwrap().text, "edited title");
        assert_eq!(second.row_data(ALBUM).unwrap().text, "edited album");
        assert_eq!(second.row_data(7).unwrap().text, "edited suggestion");
        assert_eq!(second, model.row_data(1).unwrap());
        model.replace(Arc::new(list.clone()), Vec::new());
        model.invalidate();
        assert_eq!(second.row_data(7).unwrap().text, "edited comment");
        assert_eq!(first, model.row_data(0).unwrap());
        model.replace(Arc::new(list[..2].to_vec()), Vec::new());
        model.update(&[0, 1], look, Reference::default());
        assert_eq!(
            removed.row_count(),
            0,
            "removed indices explicitly reset old handles"
        );
        assert_eq!(first, model.row_data(0).unwrap());
        assert_eq!(second, model.row_data(1).unwrap());
    }

    #[test]
    fn released_live_rows_prune_bookkeeping_and_old_notices_cannot_remove_new_entries() {
        let mut cache = RowCache::default();
        let held: Vec<_> = (0..4096).map(|i| cache.insert(i, Vec::new())).collect();
        assert_eq!(cache.live.len(), held.len());
        drop(held);
        cache.prune();
        assert_eq!(cache.live.len(), HOT_ROWS);
        assert!(cache.live.capacity() <= HOT_ROWS * 4);
        assert!(cache.retired.borrow().is_empty());
        assert!(cache.retired.borrow().capacity() <= HOT_ROWS);

        let old = cache.insert(0, Vec::new());
        cache.hot.clear();
        let new = cache.insert(0, Vec::new());
        drop(old);
        cache.prune();
        assert_eq!(ModelRc::from(cache.live[&0].upgrade().unwrap()), new);
    }

    #[test]
    fn cached_order_matches_filter_then_stable_sort_across_transitions() {
        let mut list = tracks(12).as_ref().clone();
        for (i, t) in list.iter_mut().enumerate() {
            t.title = ["Café", "Cafe\u{301}", "Straße", "STRASSE"][i % 4].into();
            t.album = format!("album {}", i % 3);
            t.date_added = format!("2026-01-{:02} 12:00:00", i % 3 + 1);
            t.key = ["8A", "9B", "", "unknown"][i % 4].into();
            t.bpm_x100 = if i % 3 == 0 { 0 } else { 12000 };
        }
        let list = Arc::new(list);
        let tag_map: HashMap<_, _> = list
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    openfader::store::key(t),
                    if i % 3 == 0 { vec![1, 2] } else { vec![2] },
                )
            })
            .collect();
        let tag_filters: [&[i64]; 3] = [&[], &[1], &[1, 2]];
        let model = Rows::new(HashSet::new());
        model.replace(list.clone(), Vec::new());
        let references = [
            Reference::default(),
            Reference {
                key: camelot("8A"),
                bpm: 120.0,
            },
            Reference {
                key: camelot("3B"),
                bpm: 130.0,
            },
        ];
        let filters = [
            Filter::default(),
            Filter {
                bpm_min: 119.0,
                min_rating: 3,
                ..Default::default()
            },
            Filter {
                around_master: true,
                key_match: true,
                ..Default::default()
            },
        ];
        let mut builds = 0;
        for column in (0..=7).chain([ALBUM, ADDED]) {
            for ascending in [true, false] {
                let sort = Some((column, ascending));
                for notation in [
                    KeyNotation::Camelot,
                    KeyNotation::Classic,
                    KeyNotation::OpenKey,
                    KeyNotation::Database,
                ] {
                    let mut look = Look::now();
                    look.notation = notation;
                    for level in [TrafficLight::Same, TrafficLight::Related1] {
                        look.level = level;
                        for reference in &references {
                            for filter in &filters {
                                for query in
                                    ["", " \t ", "cafe", "straße", "artist 3", "8a", "am", "8m"]
                                {
                                    for wanted in tag_filters {
                                        let tags = |t: &Track| {
                                            super::super::tagging::passes(&tag_map, wanted, t)
                                        };
                                        assert_eq!(
                                            model
                                                .shown(query, filter, reference, &look, sort, tags),
                                            eager_shown(
                                                &list, query, filter, reference, &look, sort, tags
                                            ),
                                            "column={column}, asc={ascending}, query={query:?}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                builds += 1;
                assert_eq!(model.data.borrow().sort_builds, builds);
                // Manual/ranked views keep list order without consuming or replacing the cached header order.
                let look = Look::now();
                assert_eq!(
                    model.shown(
                        "cafe",
                        &Filter::default(),
                        &references[0],
                        &look,
                        None,
                        |_| true
                    ),
                    eager_shown(
                        &list,
                        "cafe",
                        &Filter::default(),
                        &references[0],
                        &look,
                        None,
                        |_| true
                    )
                );
                model.shown("", &Filter::default(), &references[0], &look, sort, |_| {
                    true
                });
                assert_eq!(model.data.borrow().sort_builds, builds);
            }
        }
        assert!(
            model.data.borrow().cells.is_empty(),
            "ordering must keep cells lazy"
        );
    }

    #[test]
    fn order_invalidates_for_new_arc_and_copy_on_write_metadata_but_not_same_snapshot() {
        let mut list = tracks(3);
        let model = Rows::new(HashSet::new());
        let look = Look::now();
        let reference = Reference::default();
        let filter = Filter::default();
        let sort = Some((0, true));
        model.replace(list.clone(), Vec::new());
        assert_eq!(
            model.shown("", &filter, &reference, &look, sort, |_| true),
            [0, 1, 2]
        );
        model.replace(list.clone(), Vec::new());
        model.shown("", &filter, &reference, &look, sort, |_| true);
        assert_eq!(model.data.borrow().sort_builds, 1);
        let original = list.clone();
        list = Arc::new(list.as_ref().clone());
        model.replace(list.clone(), Vec::new());
        model.shown("", &filter, &reference, &look, sort, |_| true);
        assert_eq!(
            model.data.borrow().sort_builds,
            2,
            "a different Arc is not the same snapshot"
        );
        let edited = Arc::make_mut(&mut list);
        edited[2].title = "aaa".into();
        edited[0].artist = "changed artist".into();
        edited[0].bpm_x100 = 13900;
        edited[0].key = "12B".into();
        edited[0].rating = 5;
        edited[0].length_s = 999;
        model.replace(list.clone(), Vec::new());
        assert_eq!(
            model.shown("", &filter, &reference, &look, sort, |_| true),
            [2, 0, 1]
        );
        assert_eq!(model.data.borrow().sort_builds, 3);
        for column in [0, 1, BPM, KEY, RATING, TIME] {
            for ascending in [true, false] {
                for query in ["", "changed artist", "12b"] {
                    let sort = Some((column, ascending));
                    assert_eq!(
                        model.shown(query, &filter, &reference, &look, sort, |_| true),
                        eager_shown(&list, query, &filter, &reference, &look, sort, |_| true)
                    );
                }
            }
        }
        model.replace(original.clone(), Vec::new());
        for column in [0, BPM, KEY, RATING, TIME] {
            let sort = Some((column, true));
            assert_eq!(
                model.shown("", &filter, &reference, &look, sort, |_| true),
                eager_shown(&original, "", &filter, &reference, &look, sort, |_| true)
            );
        }
    }

    #[test]
    fn detected_updates_rebuild_order_and_refresh_search_and_reference_filters() {
        let unknown = Track {
            title: "unknown".into(),
            file: Some(format!("/openfader-order-detected-{}.wav", std::process::id()).into()),
            ..Track::default()
        };
        let list = Arc::new(vec![
            unknown.clone(),
            Track {
                bpm_x100: 12000,
                key: "8A".into(),
                ..Track::default()
            },
            Track {
                bpm_x100: 13000,
                key: "10A".into(),
                ..Track::default()
            },
        ]);
        let model = Rows::new(HashSet::new());
        let look = Look::now();
        let reference = Reference {
            key: camelot("9A"),
            bpm: 128.0,
        };
        let filter = Filter::default();
        model.replace(list.clone(), Vec::new());
        let text_model = Rows::new(HashSet::new());
        text_model.replace(list.clone(), Vec::new());
        let title_order =
            text_model.shown("", &filter, &reference, &look, Some((0, true)), |_| true);
        assert_eq!(
            model.shown("", &filter, &reference, &look, Some((BPM, true)), |_| true),
            [0, 1, 2]
        );
        detected().remember(&unknown, (12800, "9A".into()));
        assert_eq!(
            model.shown("", &filter, &reference, &look, Some((BPM, true)), |_| true),
            [1, 0, 2]
        );
        assert_eq!(model.data.borrow().sort_builds, 2);
        assert_eq!(
            text_model.shown("9a", &filter, &reference, &look, Some((0, true)), |_| true),
            [0],
        );
        assert_eq!(
            text_model.shown("", &filter, &reference, &look, Some((0, true)), |_| true),
            title_order,
        );
        assert_eq!(
            text_model.data.borrow().sort_builds,
            1,
            "detected values do not change title order"
        );
        let relative = Filter {
            around_master: true,
            key_match: true,
            ..Default::default()
        };
        for filter in [&filter, &relative] {
            for column in [BPM, KEY] {
                for ascending in [true, false] {
                    for query in ["", "9a", "unknown"] {
                        let sort = Some((column, ascending));
                        assert_eq!(
                            model.shown(query, filter, &reference, &look, sort, |_| true),
                            eager_shown(&list, query, filter, &reference, &look, sort, |_| true)
                        );
                    }
                }
            }
        }
        assert_eq!(model.facts()[0].key, "9A");
    }

    #[test]
    fn lazy_rows_match_eager_cells_after_search_sort_and_reordering() {
        let list = tracks(20_000);
        let model = Rows::new(HashSet::new());
        model.replace(list.clone(), Vec::new());
        let look = Look::now();
        let reference = Reference {
            key: camelot("8A"),
            bpm: 120.0,
        };
        for (query, column, ascending) in [
            ("track", BPM, true),
            ("track 000", 0, false),
            ("artist 3", KEY, true),
            ("missing", RATING, false),
            ("track", TIME, false),
        ] {
            let shown = model.shown(
                query,
                &Filter::default(),
                &reference,
                &look,
                Some((column, ascending)),
                |_| true,
            );
            model.update(&shown, look, reference.clone());
            assert_eq!(model.row_count(), shown.len());
            for n in 0..shown.len().min(32) {
                let t = &list[shown[n]];
                assert_eq!(
                    texts(&model.row_data(n).unwrap()),
                    texts(&row(t, &Facts::of(t), &reference, false, &look))
                );
            }
            assert!(model.row_data(shown.len()).is_none());
            assert!(
                model.data.borrow().cells.len() <= 128,
                "filtering must not format all 20,000 rows"
            );
        }
        let unchanged = model.data.borrow().shown.clone();
        assert!(!model.update(&unchanged, look, reference));
    }

    #[test]
    fn cached_rows_follow_reference_preferences_played_and_suggestion_notes() {
        let list = tracks(3);
        let key = openfader::store::key(&list[0]);
        let model = Rows::new(HashSet::new());
        model.replace(
            list.clone(),
            vec!["first".into(), "second".into(), "third".into()],
        );
        let mut look = Look::now();
        look.played = true;
        look.tempo_digits = 1;
        model.update(&[0, 1, 2], look, Reference::default());
        assert_eq!(model.data.borrow().cells.len(), 0);
        let first = model.row_data(0).unwrap();
        assert_eq!(first.row_data(7).unwrap().text, "first");
        assert_eq!(first.row_data(BPM).unwrap().text, "120.0");
        assert_eq!(first.row_data(HINTS).unwrap().text, "");
        model.remark(Reference {
            key: camelot("8A"),
            bpm: 120.0,
        });
        assert_eq!(first.row_data(HINTS).unwrap().text, "kb");
        assert_eq!(model.data.borrow().cells.len(), 1);
        assert_eq!(
            model.row_data(1).unwrap().row_data(HINTS).unwrap().text,
            "b"
        );
        model.mark_played(&key);
        assert_eq!(first.row_data(PLAYED).unwrap().text, "1");
        model.update(&[2, 0], look, Reference::default());
        assert_eq!(
            model.row_data(0).unwrap().row_data(7).unwrap().text,
            "third"
        );
        assert_eq!(
            model.row_data(1).unwrap().row_data(0).unwrap().text,
            list[0].title
        );
        model.reset_played();
        assert_eq!(first.row_data(PLAYED).unwrap().text, "");
        look.tempo_digits = 2;
        look.notation = KeyNotation::Classic;
        model.update(&[0, 1], look, Reference::default());
        let refreshed = model.row_data(0).unwrap();
        assert_eq!(refreshed.row_data(BPM).unwrap().text, "120.00");
        assert_eq!(
            refreshed.row_data(KEY).unwrap().text,
            camelot::display(&list[0].key, KeyNotation::Classic)
        );
        assert_eq!(refreshed.row_data(7).unwrap().text, "first");
        let reference = Reference {
            key: camelot("8B"),
            bpm: 120.0,
        };
        look.level = TrafficLight::Same;
        model.update(&[0], look, reference.clone());
        assert_eq!(
            model.row_data(0).unwrap().row_data(HINTS).unwrap().text,
            "b"
        );
        look.level = TrafficLight::Related1;
        model.update(&[0], look, reference);
        assert_eq!(
            model.row_data(0).unwrap().row_data(HINTS).unwrap().text,
            "kb"
        );
        model.mark_played(&key);
        look.played = false;
        model.update(&[0], look, Reference::default());
        assert_eq!(
            model.row_data(0).unwrap().row_data(PLAYED).unwrap().text,
            ""
        );
    }

    #[test]
    fn metadata_replace_and_restore_refresh_facts_and_cells() {
        let original = tracks(2);
        let model = Rows::new(HashSet::new());
        let look = Look::now();
        model.replace(original.clone(), Vec::new());
        model.update(&[0, 1], look, Reference::default());
        assert_eq!(
            model.row_data(0).unwrap().row_data(0).unwrap().text,
            original[0].title
        );
        let mut edited = original.as_ref().clone();
        edited[0].title = "changed".into();
        edited[0].bpm_x100 = 13725;
        edited[0].key = "11A".into();
        edited[0].comment = "edited comment".into();
        model.replace(Arc::new(edited.clone()), Vec::new());
        assert_eq!(model.facts()[0].bpm_x100, 13725);
        model.update(&[1, 0], look, Reference::default());
        assert_eq!(
            texts(&model.row_data(1).unwrap()),
            texts(&row(
                &edited[0],
                &Facts::of(&edited[0]),
                &Reference::default(),
                false,
                &look
            ))
        );
        model.replace(original.clone(), Vec::new());
        model.update(&[0, 1], look, Reference::default());
        assert_eq!(model.facts()[0].bpm_x100, original[0].bpm_x100);
        assert_eq!(
            texts(&model.row_data(0).unwrap()),
            texts(&row(
                &original[0],
                &Facts::of(&original[0]),
                &Reference::default(),
                false,
                &look
            ))
        );
        model.replace(Arc::new(edited.clone()), Vec::new());
        model.update(&[0, 1], look, Reference::default());
        assert_eq!(model.facts()[0].key, "11A");
        assert_eq!(
            texts(&model.row_data(0).unwrap()),
            texts(&row(
                &edited[0],
                &Facts::of(&edited[0]),
                &Reference::default(),
                false,
                &look
            ))
        );
    }

    #[test]
    fn missing_generation_refreshes_cached_title_without_changing_facts() {
        use crate::app::library::missing;
        let path = std::path::PathBuf::from("/openfader-browser-missing-test.wav");
        let list = Arc::new(vec![Track {
            title: "missing test".into(),
            file: Some(path.clone()),
            ..Track::default()
        }]);
        let model = Rows::new(HashSet::new());
        let look = Look::now();
        model.replace(list, Vec::new());
        model.update(&[0], look, Reference::default());
        assert_eq!(
            model.row_data(0).unwrap().row_data(0).unwrap().text,
            "missing test"
        );
        missing::update_marks(HashSet::from([path.clone()]));
        let version = missing::version();
        missing::update_marks(HashSet::from([path]));
        assert_eq!(missing::version(), version);
        assert!(model.update(&[0], look, Reference::default()));
        assert_eq!(
            model.row_data(0).unwrap().row_data(0).unwrap().text,
            "[!] missing test"
        );
        missing::update_marks(HashSet::new());
        assert!(model.update(&[0], look, Reference::default()));
        assert_eq!(
            model.row_data(0).unwrap().row_data(0).unwrap().text,
            "missing test"
        );
    }

    #[test]
    fn late_analysis_lookup_invalidates_cached_facts_and_rows_once() {
        use openfader::analysis::{self, cache, options::Options};
        let root = std::env::temp_dir().join(format!(
            "openfader-browser-late-analysis-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let audio = root.join("audio.wav");
        std::fs::write(&audio, b"audio").unwrap();
        let t = Track {
            title: root.display().to_string(),
            file: Some(audio),
            ..Track::default()
        };
        let cached = cache::dir().unwrap().join(format!(
            "{:016x}.bin",
            cache::fnv1a(openfader::store::key(&t).bytes())
        ));
        let model = Rows::new(HashSet::new());
        let look = Look::now();
        model.replace(Arc::new(vec![t.clone()]), Vec::new());
        model.update(&[0], look, Reference::default());
        assert_eq!(model.row_data(0).unwrap().row_data(BPM).unwrap().text, "");
        cache::store(
            &t,
            &analysis::Analysis {
                key: Some("9A".into()),
                beats: vec![analysis::Beat {
                    time_ms: 0,
                    bpm_x100: 12850,
                    bar_pos: 1,
                }],
                ..analysis::Analysis::default()
            },
            Options::default().profile(),
        )
        .unwrap();
        let have = detected();
        let before = have.version();
        let track = t.clone();
        let found = std::thread::spawn(move || crate::app::analyze::Detected::read(&track))
            .join()
            .unwrap();
        assert_eq!(found, Some((12850, "9A".into())));
        have.remember(&t, found.clone().unwrap());
        assert_eq!(have.version(), before + 1);
        have.remember(&t, found.unwrap());
        assert_eq!(have.version(), before + 1);
        assert_eq!(model.facts()[0].bpm_x100, 12850);
        assert_eq!(model.facts()[0].key, "9A");
        assert!(model.update(&[0], look, Reference::default()));
        assert_eq!(
            texts(&model.row_data(0).unwrap()),
            texts(&row(
                &t,
                &Facts::of(&t),
                &Reference::default(),
                false,
                &look
            ))
        );
        std::fs::remove_file(cached).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "isolated 100k-row native-shaped scroll; reports retained rows and process RSS"]
    fn benchmark_browser_scroll_retention() {
        fn memory_kib(field: &str) -> usize {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix(field))
                .and_then(|value| value.split_whitespace().next())
                .unwrap()
                .parse()
                .unwrap()
        }
        let mut list = tracks(100_000).as_ref().clone();
        for (i, track) in list.iter_mut().enumerate() {
            track.file = Some(
                format!(
                    "/music/artist {}/album {}/track {i:06}.flac",
                    i % 1000,
                    i % 500
                )
                .into(),
            );
            track.album = format!("Collection {} — extended mix", i % 500);
            track.comment = "House · imported collection · warm-up / peak-time".repeat(4);
            track.date_added = "2026-10-20 12:30:00".into();
        }
        let model = Rows::new(HashSet::new());
        model.replace(Arc::new(list), Vec::new());
        let look = Look::now();
        let reference = Reference {
            key: camelot("8A"),
            bpm: 120.0,
        };
        let shown = model.shown(
            "",
            &Filter::default(),
            &reference,
            &look,
            Some((0, true)),
            |_| true,
        );
        model.update(&shown, look, reference);
        let held: Vec<_> = [0, 7, 23].map(|i| model.row_data(i).unwrap()).into();
        let baseline_kib = memory_kib("VmRSS:");
        let mut peak_kib = baseline_kib;
        let mut peak_rows = 0;
        let mut viewport = VecDeque::new();
        let start = std::time::Instant::now();
        for i in 0..shown.len() {
            let row = model.row_data(i).unwrap();
            std::hint::black_box(row.row_data(0));
            viewport.push_back(row);
            if viewport.len() > 48 {
                viewport.pop_front();
            }
            if i % 16 == 0 {
                assert_eq!(viewport.back().unwrap(), &model.row_data(i).unwrap());
            }
            let retained = model.data.borrow().cells.len();
            peak_rows = peak_rows.max(retained);
            assert!(retained <= HOT_ROWS + 48 + held.len());
            if i % 4096 == 0 {
                peak_kib = peak_kib.max(memory_kib("VmRSS:"));
            }
        }
        peak_kib = peak_kib.max(memory_kib("VmRSS:"));
        drop(viewport);
        drop(held);
        model.update(&shown, look, Reference::default());
        let d = model.data.borrow();
        assert_eq!(d.cells.len(), HOT_ROWS);
        assert!(d.cells.retired.borrow().is_empty());
        eprintln!(
            "100k tracks, 48-row scrolling viewport: elapsed={:?}, hot={}, retained={}, peak_retained={peak_rows}, weak_capacity={}, drop_queue_capacity={}, baseline_rss_kib={baseline_kib}, sampled_peak_rss_kib={peak_kib}, final_rss_kib={}, process_peak_rss_kib={}",
            start.elapsed(),
            d.cells.hot.len(),
            d.cells.len(),
            d.cells.live.capacity(),
            d.cells.retired.borrow().capacity(),
            memory_kib("VmRSS:"),
            memory_kib("VmHWM:")
        );
    }

    #[test]
    #[ignore = "20k/50k uncached versus cached sort with lazy browser rows"]
    fn benchmark_lazy_browser_search_sort() {
        use std::time::Instant;
        for count in [20_000, 50_000] {
            let list = tracks(count);
            let look = Look::now();
            let reference = Reference::default();
            let queries: Vec<_> = [0, BPM]
                .into_iter()
                .flat_map(|column| ["", "t", "track", "track 0", "track 00"].map(|q| (q, column)))
                .collect();
            let uncached = Rows::new(HashSet::new());
            uncached.replace(list.clone(), Vec::new());
            let mut uncached_sort_builds = 0;
            let start = Instant::now();
            let expected: Vec<_> = queries
                .iter()
                .map(|&(q, column)| {
                    let shown = {
                        let facts = uncached.facts();
                        let mut shown = filter::shown(
                            &list,
                            &facts,
                            q,
                            &Filter::default(),
                            &reference,
                            &look,
                            |_| true,
                        );
                        filter::sort(&mut shown, &list, &facts, column, true, &look);
                        uncached_sort_builds += 1;
                        shown
                    };
                    uncached.update(&shown, look, reference.clone());
                    let visible = (0..shown.len().min(32))
                        .map(|i| texts(&uncached.row_data(i).unwrap()))
                        .collect::<Vec<_>>();
                    (shown, visible)
                })
                .collect();
            let uncached_time = start.elapsed();
            let model = Rows::new(HashSet::new());
            model.replace(list.clone(), Vec::new());
            let start = Instant::now();
            for (&(q, column), (expected_shown, expected_cells)) in queries.iter().zip(&expected) {
                let shown = model.shown(
                    q,
                    &Filter::default(),
                    &reference,
                    &look,
                    Some((column, true)),
                    |_| true,
                );
                assert_eq!(&shown, expected_shown);
                model.update(&shown, look, reference.clone());
                let visible: Vec<_> = (0..shown.len().min(32))
                    .map(|i| texts(&model.row_data(i).unwrap()))
                    .collect();
                assert_eq!(&visible, expected_cells);
            }
            let lazy_time = start.elapsed();
            assert!(model.data.borrow().cells.len() <= queries.len() * 32);
            assert!(uncached.data.borrow().cells.len() <= queries.len() * 32);
            assert_eq!(
                model.data.borrow().cells.len(),
                uncached.data.borrow().cells.len()
            );
            let builds = model.data.borrow().sort_builds;
            assert_eq!(builds, 2, "one full-list sort per selected column");
            eprintln!(
                "{count} tracks, {} search+sort refreshes (both cached facts/lazy rows): uncached_sort={uncached_time:?}, cached_sort={lazy_time:?}, formatted={}, sort_builds={builds}/{uncached_sort_builds}, saved_sorts={}, retained_sort_bytes={}",
                queries.len(),
                model.data.borrow().cells.len(),
                uncached_sort_builds - builds,
                count * std::mem::size_of::<usize>()
            );
        }
    }
}
