//! The track table's cells: what each column shows for a track, the master-deck hint markers, and the
//! reference (the master deck's key and tempo) the markers and the Track Filter compare with.

use crate::app::{
    App, DeckState,
    analyze::detected,
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
    collections::{HashMap, HashSet},
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
pub const COLUMNS: usize = 14;
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

/// Updates only the HINTS cell; facts remain valid when just the reference changes.
fn remark(row: &ModelRc<StandardListViewItem>, f: &Facts, r: &Reference, look: &Look) {
    let h = hints(f, r, look.level);
    if row.row_data(HINTS).is_some_and(|c| c.text != h) {
        row.set_row_data(HINTS, h.into());
    }
}

struct RowData {
    list: Arc<Vec<Track>>,
    facts: Vec<Facts>,
    detected: u64,
    missing: u64,
    shown: Vec<usize>,
    notes: Vec<String>,
    played: HashSet<String>,
    reference: Reference,
    look: Look,
    cells: HashMap<usize, ModelRc<StandardListViewItem>>,
    dirty: bool,
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
                missing: crate::app::library::missing::version(),
                shown: Vec::new(),
                notes: Vec::new(),
                played,
                reference: Reference::default(),
                look: Look::now(),
                cells: HashMap::new(),
                dirty: false,
            }),
            notify: ModelNotify::default(),
        }
    }

    pub fn replace(&self, list: Arc<Vec<Track>>, notes: Vec<String>) {
        let mut d = self.data.borrow_mut();
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
        d.cells.clear();
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
                d.cells.clear();
                d.dirty = true;
            }
        }
        Ref::map(self.data.borrow(), |d| d.facts.as_slice())
    }

    pub fn invalidate(&self) {
        let mut d = self.data.borrow_mut();
        d.cells.clear();
        d.dirty = true;
    }

    pub fn update(&self, shown: &[usize], look: Look, reference: Reference) -> bool {
        let reset = {
            let mut d = self.data.borrow_mut();
            let missing = crate::app::library::missing::version();
            if d.missing != missing {
                d.missing = missing;
                d.cells.clear();
                d.dirty = true;
            }
            if d.look != look {
                d.look = look;
                d.cells.clear();
                d.dirty = true;
            }
            if d.reference != reference {
                for (&i, row) in &d.cells {
                    remark(row, &d.facts[i], &reference, &look);
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
        for (&i, row) in &d.cells {
            remark(row, &d.facts[i], &reference, &d.look);
        }
        d.reference = reference;
    }

    pub fn mark_played(&self, key: &str) -> bool {
        let mut d = self.data.borrow_mut();
        if !d.played.insert(key.to_owned()) {
            return false;
        }
        if d.look.played {
            for (&i, row) in &d.cells {
                if openfader::store::key(&d.list[i]) == key {
                    row.set_row_data(PLAYED, "1".into());
                }
            }
        }
        true
    }

    pub fn reset_played(&self) {
        let mut d = self.data.borrow_mut();
        d.played.clear();
        for row in d.cells.values() {
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
        let &i = d.shown.get(row_index)?;
        if let Some(row) = d.cells.get(&i) {
            return Some(row.clone());
        }
        let t = &d.list[i];
        let played =
            d.look.played && !d.played.is_empty() && d.played.contains(&openfader::store::key(t));
        let row = row(t, &d.facts[i], &d.reference, played, &d.look);
        if let Some(note) = d.notes.get(i).filter(|_| d.notes.len() == d.list.len()) {
            row.set_row_data(7, note.as_str().into());
        }
        d.cells.insert(i, row.clone());
        Some(row)
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

    #[test]
    fn lazy_rows_match_eager_cells_after_search_sort_and_reordering() {
        use super::super::filter::{self, Filter};
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
            let shown = {
                let facts = model.facts();
                let mut shown = filter::shown(
                    &list,
                    &facts,
                    query,
                    &Filter::default(),
                    &reference,
                    &look,
                    |_| true,
                );
                filter::sort(&mut shown, &list, &facts, column, ascending, &look);
                shown
            };
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
        assert_eq!(have.lookup(&t), Some((12850, "9A".into())));
        assert_eq!(have.version(), before + 1);
        assert_eq!(have.lookup(&t), Some((12850, "9A".into())));
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

    #[test]
    #[ignore = "20k/50k eager versus lazy browser refresh timings"]
    fn benchmark_lazy_browser_search_sort() {
        use super::super::filter::{self, Filter};
        use std::time::Instant;
        for count in [20_000, 50_000] {
            let list = tracks(count);
            let look = Look::now();
            let reference = Reference::default();
            let queries = ["t", "tr", "track", "track 0", "track 00"];
            let start = Instant::now();
            let eager: Vec<_> = queries
                .iter()
                .map(|q| {
                    let facts: Vec<_> = list.iter().map(Facts::of).collect();
                    let mut shown = filter::shown(
                        &list,
                        &facts,
                        q,
                        &Filter::default(),
                        &reference,
                        &look,
                        |_| true,
                    );
                    filter::sort(&mut shown, &list, &facts, BPM, true, &look);
                    let rows: Vec<_> = shown
                        .iter()
                        .map(|&i| row(&list[i], &facts[i], &reference, false, &look))
                        .collect();
                    (shown, rows.iter().take(32).map(texts).collect::<Vec<_>>())
                })
                .collect();
            let eager_time = start.elapsed();
            let start = Instant::now();
            let model = Rows::new(HashSet::new());
            model.replace(list.clone(), Vec::new());
            for (q, (expected_shown, expected_cells)) in queries.iter().zip(&eager) {
                let shown = {
                    let facts = model.facts();
                    let mut shown = filter::shown(
                        &list,
                        &facts,
                        q,
                        &Filter::default(),
                        &reference,
                        &look,
                        |_| true,
                    );
                    filter::sort(&mut shown, &list, &facts, BPM, true, &look);
                    shown
                };
                assert_eq!(&shown, expected_shown);
                model.update(&shown, look, reference.clone());
                let visible: Vec<_> = (0..shown.len().min(32))
                    .map(|i| texts(&model.row_data(i).unwrap()))
                    .collect();
                assert_eq!(&visible, expected_cells);
            }
            let lazy_time = start.elapsed();
            assert!(model.data.borrow().cells.len() <= queries.len() * 32);
            eprintln!(
                "{count} tracks, five search+sort refreshes: eager={eager_time:?}, lazy={lazy_time:?}, formatted={}",
                model.data.borrow().cells.len()
            );
        }
    }
}
