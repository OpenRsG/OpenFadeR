//! Library browser: playlist tree + session Tag List and Session history, search box, track filter and track
//! table (`BrowserState` in ui/browser.slint). It also owns the AUTOMIX button: the visible list, from the
//! chosen track on, is the queue handed to [`automix`](super::automix).
//!
//! BPM and key cells show the library value, else what bulk analysis detected ([`table::bpm_key`]); keys are written
//! in the notation chosen in the PREFS dialog.
//!
//! Focus (all in ui/browser.slint, since the tree and table widgets own it): the browse view (controller VIEW, `W`) hides the
//! deck panels, FX / sampler and mixer so the browser fills the window under the waveforms, and Area (Back+Shift, `Shift+W`)
//! walks the focus tree → Tag List → track list → related. The Tag List's sidebar row is published as `BrowserState.tag-row`.
//!
//! What the sidebar rows are ([`sidebar::Entry`]) decides what [`Home`] fills the table; the user's own data (My Playlists,
//! Tags, the play history) lives in the [`store`](openfader::store) and the library stays read-only.
//!
//! Children: [`wire`] (builds the [`View`], connects the `BrowserState` callbacks), [`list`] (loading a list, the
//! search / filter / sort pass, cheap row updates), [`session`] (Tag List, session history, the deck poll),
//! [`table`] (the cells), [`filter`] (the Track Filter and the Related-tracks ranking, pure functions), [`history`] (what
//! played on air, the on-air rule, M3U8 export, the stored play history), [`sidebar`] (the tree's rows),
//! [`playlists`] (My Playlists, the create / rename / delete bar, "duplicate to my playlists", ADD TO), [`tagging`]
//! (tag categories, the assign and filter panels), [`smart`] + [`editor`] (smart playlists: evaluator and editor),
//! [`library::dupes`](super::library::dupes) (duplicate-track finder, used by the "Duplicate tracks" list), [`hotcuebank`] (the
//! Cue Bank node: lists of up to 8 cues A to H, loaded onto a deck's pads, carried by the device export), [`palette`]
//! (Playlist Palette: the TAG LIST and three playlists under the track list, drag and drop), [`strip`] (the Preview column's
//! waveform strips with the cue markers), [`preview`] (the PREVIEW button and the strips' click-to-preview player), [`remote`]
//! (Enter in the search box on the search entry of a plugin source searches online), [`health`] (the Library health node: a scan of
//! the Collection for problems on a worker thread, a row per check, the fixes and ignores), [`stages`] (the Stages node: INBOX →
//! REVIEW → COLLECTION, moving tracks between them, recent moves), [`select`] (the table's multi-selection: Ctrl / Shift click,
//! Shift+arrows, Ctrl+A; the chosen row is always one of the selected rows, the menus act on [`Context::tracks`]) and [`saved`]
//! (saved searches: the search box and filter bar under a name, recalled from the SAVED panel).

mod cover;
mod editor;
mod filter;
mod health;
pub(in crate::app) mod history;
mod hotcuebank;
mod list;
mod palette;
mod played;
mod playlists;
mod preview;
mod remote;
mod saved;
mod select;
mod session;
mod sidebar;
mod smart;
mod sources;
mod stages;
mod strip;
mod suggest;
mod table;
mod tagging;
mod wire;

pub use sources::SourceType;
pub use wire::wire;

use super::{App, BrowserState, TreeRow, deck::Decks};
use filter::Filter;
use openfader::{
    library::{Library, Playlist, Track},
    store::{self, HotCueBank, Node, Store, Tag, TagGroup},
};
use playlists::{Ask, Panel};
use sidebar::Entry;
use slint::{ComponentHandle, Model, StandardListViewItem, Timer, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use table::Reference;

/// Two presses on one row closer than this load it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What the library menus act on: the chosen track, the list on screen (in row order), its name, and its playlist id as an
/// export snapshot names it (`store:<node>` for My Playlists, the library's own id otherwise; `None` for other lists).
pub struct Context {
    pub track: Option<Track>,
    /// The selected tracks in row order (the chosen one alone when a single row is selected; empty when none).
    pub tracks: Vec<Track>,
    pub list: Vec<Track>,
    pub name: String,
    pub playlist: Option<String>,
    /// An opened iTunes / xml / Explorer list on screen: where to read it again from (for IMPORT TO COLLECTION).
    pub source: Option<(std::path::PathBuf, sources::SourceType, Option<String>)>,
    /// The id of the device whose tracks are on screen (for IMPORT TO COLLECTION, which copies them).
    pub device: Option<String>,
}

thread_local! {
    static VIEW: RefCell<Option<std::rc::Weak<RefCell<View>>>> = const { RefCell::new(None) };
}

/// The browser, once wired (UI thread).
fn view() -> Option<Rc<RefCell<View>>> {
    VIEW.with(|v| v.borrow().as_ref().and_then(std::rc::Weak::upgrade))
}

/// Current metadata from the Collection or an opened source, including removed overrides after undo.
pub fn fresh_track(t: &Track) -> Option<Track> {
    view()?.borrow_mut().fresh_track(t)
}

/// The browser's selection now (empty before the browser is wired).
pub fn context() -> Context {
    let view = view();
    view.map_or(
        Context {
            track: None,
            tracks: Vec::new(),
            list: Vec::new(),
            name: String::new(),
            playlist: None,
            source: None,
            device: None,
        },
        |v| {
            let v = v.borrow();
            Context {
                track: v.selected.clone(),
                tracks: v.selection(),
                list: v.shown.iter().map(|&i| v.list[i].clone()).collect(),
                name: v.list_name(),
                source: v.source_sel(),
                device: v.device_target(&v.entry).map(|d| d.device),
                playlist: match &v.entry {
                    Entry::User(id) => Some(format!("store:{id}")),
                    Entry::Lib(i) => Some(v.lib_playlists[*i].id.clone()),
                    _ => None,
                },
            }
        },
    )
}

/// Analysis keeps the list shared and captures row indices instead of cloning every selected track.
pub fn analysis_selection() -> Option<(Arc<Vec<Track>>, Vec<usize>)> {
    let browser = view()?;
    let browser = browser.borrow();
    let selected = browser.selected.as_ref()?;
    let rows = browser.sel.ordered(&browser.shown);
    if rows.len() > 1 {
        Some((browser.list.clone(), rows))
    } else {
        Some((Arc::new(vec![selected.clone()]), vec![0]))
    }
}

/// The status bar's loading line: `text` beside a bar filled to `progress` (0 to 1); `None` hides both. Each step also goes to
/// the terminal, the readable log of a device being mounted and read.
// ponytail: one line for the whole app; two sticks read at once take turns writing it.
pub fn loading(app: &App, text: &str, progress: Option<f32>) {
    if !text.is_empty() {
        eprintln!("{text}");
    }
    let s = app.global::<BrowserState>();
    s.set_load_text(text.into());
    s.set_load_progress(progress.unwrap_or(-1.0));
}

/// A cover was chosen or undone: the Cover column reads its pictures again.
pub fn covers_changed(app: &App) {
    if let Some(v) = view() {
        let v = v.borrow();
        v.covers.reset(app, v.shown.len());
    }
}

/// Device `id` was written to (a transfer, an export): the tree reads its library again if it showed it.
pub fn device_written(app: &App, id: &str) {
    if let Some(v) = view() {
        v.borrow_mut().reread_device(app, id);
    }
}

/// Where the list comes from when Related is off.
#[derive(Clone)]
enum Home {
    /// Folders and headings: no tracks of their own.
    Nothing,
    /// `None`: all tracks.
    Playlist(Option<String>),
    /// The session Tag List.
    Tags,
    /// Tracks that played 30 s on air this session.
    History,
    /// A user playlist (store node id), in its own order.
    User(i64),
    /// A smart playlist (store node id), evaluated over the whole library.
    Smart(i64),
    /// A day of the stored play history.
    Day(String),
    /// Every track carrying this tag.
    Tag(i64),
    /// A Cue Bank (store id): its banks A to H, in order.
    Bank(i64),
    /// Tracks that share title, artist and length with another one.
    Dupes,
    /// Tracks CUE Analysis set cues on ([`analyze::cue_analysed`]).
    CueAnalysis,
    /// The tracks a Library health check lists ([`health`]).
    Health(crate::app::library::health::Check),
    /// A list of an opened external source (`None`: all its tracks).
    Source(usize, Option<String>),
    /// The tracks waiting in a stage before the Collection (store stage id, [`stages`]).
    Stage(i64),
}

/// The whole library with each track's store key, read again only when the store changed ([`Store::generation`]).
struct Cache {
    generation: Cell<u64>,
    tracks: Arc<Vec<Track>>,
    keys: Vec<String>,
    by_key: HashMap<String, usize>,
}

impl Cache {
    fn read(
        kept: &mut Option<Rc<Self>>,
        generation: u64,
        library: &dyn Library,
    ) -> Result<Rc<Self>, String> {
        if let Some(cache) = kept.as_ref().filter(|c| c.generation.get() == generation) {
            return Ok(cache.clone());
        }
        let tracks = library.tracks_all()?;
        if let Some(cache) = kept.as_ref().filter(|c| Arc::ptr_eq(&c.tracks, &tracks)) {
            // Other UI consumers may retain this Rc; its immutable indexes still describe the same list.
            cache.generation.set(generation);
            return Ok(cache.clone());
        }
        let keys: Vec<String> = tracks.iter().map(store::key).collect();
        let mut by_key = HashMap::with_capacity(keys.len());
        for (i, key) in keys.iter().enumerate() {
            by_key.entry(key.clone()).or_insert(i);
        }
        Ok(kept
            .insert(Rc::new(Self {
                generation: Cell::new(generation),
                tracks,
                keys,
                by_key,
            }))
            .clone())
    }
}

/// The current list plus the search/filter/sort state that turns it into the visible rows.
struct View {
    lib: Rc<dyn Library>,
    store: Arc<Store>,
    rows: Rc<table::Rows>,
    sidebar: Rc<VecModel<TreeRow>>,
    panel_rows: Rc<VecModel<StandardListViewItem>>,
    /// Shared with the library cache for the whole library ([`View::load`]), so a view change copies nothing.
    list: Arc<Vec<Track>>,
    /// Visible row → index into `list`.
    shown: Vec<usize>,
    query: String,
    filter: Filter,
    /// tag ids a track must all carry (the TAG FILTER panel).
    tag_filter: Vec<i64>,
    /// The saved searches by name ([`saved`]), in the order of the SAVED panel.
    saved: Vec<store::SavedSearch>,
    /// (column, ascending); kept across playlist changes, like the header's sort arrow.
    sort: Option<(usize, bool)>,
    home: Home,
    /// Related view: the tracks that mix with this one (fixed until Related is switched off).
    related: Option<Track>,
    /// Session Tag List, in queue order. Never written to disk.
    tags: Vec<Track>,
    /// Session history, in play order (see [`history`]).
    history: Vec<Track>,
    /// When each track of [`View::history`] reached the session threshold (Unix seconds), same order.
    history_at: Vec<i64>,
    /// The track chosen in the table last.
    selected: Option<Track>,
    /// The selected rows (the chosen one is always among them).
    sel: select::Selection,
    /// The entry `sel` belongs to: another one resets it.
    sel_entry: Entry,
    /// `BrowserState.selected-rows` while several rows are selected (empty otherwise).
    marks: Rc<VecModel<bool>>,
    /// The status line without the "{} selected" suffix ([`View::publish_selection`]).
    status_base: String,
    reference: Reference,
    /// `detected().version()` the rows were last built for.
    detected_ver: u64,
    /// Index into `list` of the row chosen last: tells apart the rows of a track that is listed twice.
    sel_at: Option<usize>,
    /// Sidebar row → what it is; `entry` is the chosen one.
    entries: Vec<Entry>,
    entry: Entry,
    /// Headings unfolded this session ([`sidebar::fold`]); every other one shows only itself.
    open: Vec<Entry>,
    lib_playlists: Vec<Playlist>,
    nodes: Vec<Node>,
    cats: Vec<TagGroup>,
    mytags: Vec<Tag>,
    /// The store's Cue Banks ([`hotcuebank`]) and the decks they load onto.
    banks: Vec<HotCueBank>,
    decks: Decks,
    /// Tag ids of every tagged track, by store key.
    tag_map: HashMap<String, Vec<i64>>,
    cache: Option<Rc<Cache>>,
    /// User playlist on screen: list index → index among the playlist's stored entries.
    slots: Vec<usize>,
    /// Groups the Duplicate tracks list found.
    dupe_groups: usize,
    /// Library health: the scan and its result ([`health`]).
    health: health::Monitor,
    ask: Option<Ask>,
    panel: Panel,
    editing: Option<editor::Editing>,
    /// DELETE was pressed once on this entry.
    confirm: Option<Entry>,
    /// The mounted devices and the iTunes / xml / Explorer sources (see [`sources`]).
    sources: Vec<sources::Source>,
    _timer: Timer,
    _preview: Timer,
    /// The SUGGEST tab: the list ranked for a track ([`suggest`]).
    suggest: suggest::State,
    /// The Preview column's waveform strips ([`strip`]).
    strips: strip::Strips,
    /// The stages INBOX → REVIEW → COLLECTION and their counts ([`stages`]).
    stages: stages::Stages,
    /// The Cover column's pictures ([`cover`]).
    covers: cover::Covers,
}

impl View {
    /// Related and the Tag List keep their own order (the header sort does not apply).
    fn ordered(&self) -> bool {
        self.related.is_some()
            || self.suggest.on
            || matches!(
                self.home,
                Home::Tags
                    | Home::History
                    | Home::User(_)
                    | Home::Bank(_)
                    | Home::Day(_)
                    | Home::Dupes
            )
    }

    /// Visible row of `t`, if it is shown (see [`row_among`]).
    fn row_of(&self, t: &Track) -> Option<usize> {
        row_among(&self.list, &self.shown, self.sel_at, t)
    }

    /// The track after `t` in the visible list (Preferences > Controller > Continuous Playing); `None` at the end or when `t` is not shown.
    fn next_after(&self, t: &Track) -> Option<Track> {
        let at = self.row_of(t)?;
        self.shown.get(at + 1).map(|&i| self.list[i].clone())
    }

    /// The shown list from the chosen track on (the first row when none is chosen or it is filtered out): what AUTOMIX plays.
    fn queue(&self) -> Vec<Track> {
        let from = self
            .selected
            .as_ref()
            .and_then(|s| self.row_of(s))
            .unwrap_or(0);
        self.shown[from..]
            .iter()
            .map(|&i| self.list[i].clone())
            .collect()
    }

    /// The library with store keys: read once per [`Store::generation`], so a refresh or a Related toggle never reads the
    /// whole library again while nothing changed.
    fn cache(&mut self) -> Result<Rc<Cache>, String> {
        Cache::read(&mut self.cache, self.store.generation(), self.lib.as_ref())
    }

    /// The table row of the chosen track, as an index into `list`.
    fn cursor(&self, app: &App) -> Option<usize> {
        let row = usize::try_from(app.global::<BrowserState>().get_current_row()).ok()?;
        self.shown.get(row).copied()
    }

    /// The selected tracks in row order; the chosen track alone when at most one row is selected.
    fn selection(&self) -> Vec<Track> {
        let rows = self.sel.ordered(&self.shown);
        if rows.len() > 1 || self.selected.is_none() {
            return rows.iter().map(|&i| self.list[i].clone()).collect();
        }
        self.selected.iter().cloned().collect()
    }

    /// `t` was chosen outside the table (a palette, a drop on a deck): the table's multi-selection ends.
    fn choose_elsewhere(&mut self, app: &App, t: &Track) {
        self.selected = Some(t.clone());
        self.sel.clear();
        self.publish_selection(app);
    }

    /// Tells the table which rows are selected and the status line how many; call after the selection or the rows changed.
    fn publish_selection(&mut self, app: &App) {
        let state = app.global::<BrowserState>();
        let n = self.sel.len();
        let was = self.marks.row_count() > 0;
        self.marks.set_vec(if n > 1 {
            self.sel.marks(&self.shown)
        } else {
            Vec::new()
        });
        state.set_selected_count(n.max(1) as i32);
        if n > 1 {
            let text = format!("{} · {}", self.status_base, tr!("{} selected", n));
            state.set_status(text.into());
        } else if was {
            state.set_status(self.status_base.as_str().into());
        }
    }
}

/// Visible row (position in `shown`, the indices of `list` on screen) of `t`. A track listed twice (a playlist entry added twice,
/// a track played twice) is the row that was chosen (`at`, an index into `list`) while that still holds `t`, else its first row.
fn row_among(list: &[Track], shown: &[usize], at: Option<usize>, t: &Track) -> Option<usize> {
    at.filter(|&at| list.get(at) == Some(t))
        .and_then(|at| shown.iter().position(|&i| i == at))
        .or_else(|| shown.iter().position(|&i| list[i] == *t))
}

/// The track at visible row `i`.
fn track_at(v: &RefCell<View>, i: i32) -> Option<Track> {
    let v = v.borrow();
    usize::try_from(i)
        .ok()
        .and_then(|i| v.shown.get(i))
        .map(|&k| v.list[k].clone())
}

/// `s` lower-cased, without combining accents (so a decomposed "é" equals "é"), with the Latin letters that carry a stroke or
/// a diacritic written as their base letter, "ß" as "ss" and the Turkish dotless "ı" / dotted "İ" as "i".
fn fold(s: &str) -> String {
    const BASE: [(&str, char); 19] = [
        ("àáâãäåāăą", 'a'),
        ("çćĉċč", 'c'),
        ("ďđ", 'd'),
        ("èéêëēĕėęě", 'e'),
        ("ĝğġģ", 'g'),
        ("ĥħ", 'h'),
        ("ìíîïĩīĭįı", 'i'),
        ("ĵ", 'j'),
        ("ķ", 'k'),
        ("ĺļľŀł", 'l'),
        ("ñńņňŉ", 'n'),
        ("òóôõöøōŏő", 'o'),
        ("ŕŗř", 'r'),
        ("śŝşš", 's'),
        ("ţťŧ", 't'),
        ("ùúûüũūŭůűų", 'u'),
        ("ŵ", 'w'),
        ("ýÿŷ", 'y'),
        ("źżž", 'z'),
    ];
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            '\u{300}'..='\u{36f}' => {}
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            c if c.is_ascii() => out.push(c),
            c => out.push(
                BASE.iter()
                    .find(|(set, _)| set.contains(c))
                    .map_or(c, |&(_, base)| base),
            ),
        }
    }
    out
}

/// Whether `hay` contains `needle`, which [`fold`] has already folded: letter case, accents and "ß" do not matter. ASCII on
/// both sides (the usual case) compares bytes without allocating.
fn contains_folded(hay: &str, needle: &str) -> bool {
    if hay.is_ascii() && needle.is_ascii() {
        let n = needle.len();
        return n == 0
            || hay
                .as_bytes()
                .windows(n)
                .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()));
    }
    fold(hay).contains(needle)
}

/// [`contains_folded`] for a needle as typed.
fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    contains_folded(hay, &fold(needle))
}

#[cfg(test)]
fn timer_test_pump(duration: Duration) {
    let until = std::time::Instant::now() + duration;
    while std::time::Instant::now() < until {
        slint::platform::update_timers_and_animations();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
fn timer_test_view(app: &App) -> Rc<RefCell<View>> {
    let store = Arc::new(Store::in_memory().unwrap());
    let lib: Rc<dyn Library> = Rc::new(openfader::library::own::Own::new(store.clone()));
    let decks = crate::app::deck::wire(app, lib.clone(), Default::default());
    Rc::new(RefCell::new(View {
        lib,
        store,
        decks,
        rows: Rc::new(table::Rows::new(Default::default())),
        sidebar: Rc::default(),
        panel_rows: Rc::default(),
        list: Arc::default(),
        shown: Vec::new(),
        query: String::new(),
        filter: Filter::default(),
        tag_filter: Vec::new(),
        saved: Vec::new(),
        sort: None,
        home: Home::Playlist(None),
        related: None,
        tags: Vec::new(),
        history: Vec::new(),
        history_at: Vec::new(),
        selected: None,
        sel: Default::default(),
        sel_entry: Entry::All,
        marks: Rc::default(),
        status_base: String::new(),
        reference: Reference::default(),
        detected_ver: 0,
        sel_at: None,
        entries: Vec::new(),
        entry: Entry::All,
        open: Vec::new(),
        lib_playlists: Vec::new(),
        nodes: Vec::new(),
        cats: Vec::new(),
        mytags: Vec::new(),
        banks: Vec::new(),
        tag_map: HashMap::new(),
        cache: None,
        slots: Vec::new(),
        dupe_groups: 0,
        health: health::Monitor::new(),
        ask: None,
        panel: Panel::Closed,
        editing: None,
        confirm: None,
        sources: Vec::new(),
        _timer: Timer::default(),
        _preview: Timer::default(),
        suggest: suggest::State::default(),
        strips: strip::Strips::new(app),
        stages: stages::Stages::default(),
        covers: cover::Covers::new(app),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_listed_twice_is_found_at_the_row_that_was_chosen() {
        let t = |n: &str| Track {
            title: n.into(),
            ..Track::default()
        };
        let list = [t("A"), t("B"), t("A"), t("C")];
        let shown = [0, 1, 2, 3];
        assert_eq!(row_among(&list, &shown, Some(2), &list[0]), Some(2));
        assert_eq!(row_among(&list, &shown, Some(0), &list[0]), Some(0));
        assert_eq!(row_among(&list, &shown, None, &list[0]), Some(0));
        // the chosen row went stale (another list, another track there): the first row of the track
        assert_eq!(row_among(&list, &shown, Some(1), &list[0]), Some(0));
        // filtered: only the second A is shown
        assert_eq!(row_among(&list, &[2, 3], Some(0), &list[0]), Some(0));
        assert_eq!(row_among(&list, &[2, 3], Some(2), &list[0]), Some(0));
        assert_eq!(row_among(&list, &[1, 3], Some(2), &list[0]), None);
    }

    #[test]
    fn library_indexes_survive_history_writes_but_follow_metadata_and_collection_changes() {
        let store = Arc::new(Store::in_memory().unwrap());
        let track = |name: &str| Track {
            title: name.into(),
            file: Some(format!("/audit/{name}.wav").into()),
            ..Default::default()
        };
        store
            .collection_put(&[track("a"), track("b")], false)
            .unwrap();
        let library = openfader::library::own::Own::new(store.clone());
        let mut kept = None;
        let first = Cache::read(&mut kept, store.generation(), &library).unwrap();
        let b = first
            .tracks
            .iter()
            .position(|track| track.title == "b")
            .unwrap();
        let key = first.keys[b].clone();
        store.history_append("2026-10-27", &key, 1).unwrap();
        assert_ne!(store.generation(), first.generation.get());
        let history = Cache::read(&mut kept, store.generation(), &library).unwrap();
        assert!(
            Rc::ptr_eq(&first, &history),
            "an unrelated write must retain the indexes"
        );
        assert_eq!(history.by_key[&key], b);
        store.set_rating(&key, Some(5)).unwrap();
        let edited = Cache::read(&mut kept, store.generation(), &library).unwrap();
        assert!(!Rc::ptr_eq(&history, &edited));
        assert_eq!(edited.tracks[edited.by_key[&key]].rating, 5);
        assert_eq!(
            first.tracks[b].rating, 0,
            "retained snapshots stay immutable"
        );
        store.collection_put(&[track("c")], false).unwrap();
        let added = Cache::read(&mut kept, store.generation(), &library).unwrap();
        let c = added
            .keys
            .iter()
            .find(|key| added.tracks[added.by_key[*key]].title == "c")
            .unwrap();
        assert_eq!(
            added.tracks[added.by_key[c]].file.as_deref(),
            Some(std::path::Path::new("/audit/c.wav"))
        );
        assert_eq!(added.tracks[added.by_key[&key]].rating, 5);
    }

    #[test]
    #[ignore = "10k/100k unchanged-library index refresh timings"]
    fn benchmark_unchanged_library_index_refresh() {
        struct Shared(Arc<Vec<Track>>);
        impl Library for Shared {
            fn kind(&self) -> &str {
                "shared benchmark"
            }
            fn playlists(&self) -> Result<Vec<Playlist>, String> {
                Ok(Vec::new())
            }
            fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
                Ok(self.0.as_ref().clone())
            }
            fn tracks_all(&self) -> Result<Arc<Vec<Track>>, String> {
                Ok(self.0.clone())
            }
        }
        for count in [10_000, 100_000] {
            let library = Shared(Arc::new(
                (0..count)
                    .map(|i| Track {
                        id: Some(i as i64 + 1),
                        title: format!("Track {i}"),
                        ..Default::default()
                    })
                    .collect(),
            ));
            let start = std::time::Instant::now();
            for generation in 0..5 {
                let fresh = Cache::read(&mut None, generation, &library).unwrap();
                assert_eq!(fresh.by_key[&store::id_key(count as i64)], count - 1);
            }
            let rebuilding = start.elapsed();
            let mut kept = None;
            let first = Cache::read(&mut kept, 0, &library).unwrap();
            let start = std::time::Instant::now();
            for generation in 1..=5 {
                let reused = Cache::read(&mut kept, generation, &library).unwrap();
                assert!(Rc::ptr_eq(&first, &reused));
                assert_eq!(reused.by_key[&store::id_key(count as i64)], count - 1);
            }
            eprintln!(
                "{count} tracks, five generation changes: rebuilding={rebuilding:?}, shared-index-reuse={:?}",
                start.elapsed()
            );
        }
    }
}
