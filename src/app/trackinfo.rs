//! Selected-track details panel (`TrackInfoState` in ui/trackinfo.slint): cover, tags, rating, comment, phrase mood,
//! our detected BPM/key where it differs from the library's, and the bulk-analysis progress ([`super::analyze`]).

use super::{App, TrackInfoState, analyze, camelot::display, prefs};
use openfader::{
    library::{Library, Track},
    store,
};
use slint::{Color, ComponentHandle, Image, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
};

/// A detected BPM this far (BPM) from the library's counts as different (the grid detector itself is accurate to 0.1).
const BPM_TOLERANCE: f64 = 0.1;

thread_local! {
    /// The track the panel shows, so a finished analysis or a notation change can refresh it.
    static SHOWN: RefCell<Option<Track>> = const { RefCell::new(None) };
    static WIRED: Cell<bool> = const { Cell::new(false) };
    static LISTENER: RefCell<Option<Box<dyn Fn(&Track)>>> = const { RefCell::new(None) };
    /// The library the panel reads the selected track's phrases from.
    static LIBRARY: RefCell<Option<Rc<dyn Library>>> = const { RefCell::new(None) };
}

/// The browser's library: where the panel finds the analysis of the track it shows.
pub fn use_library(lib: Rc<dyn Library>) {
    LIBRARY.with_borrow_mut(|l| *l = Some(lib));
}

fn shown() -> Option<Track> {
    SHOWN.with_borrow(|t| t.clone())
}

/// Re-shows the selected track when it is `path` (its analysis just arrived).
pub fn detected_changed(app: &App, path: &Path) {
    if let Some(t) = shown().filter(|t| t.file.as_deref() == Some(path)) {
        show(app, &t);
    }
}

pub fn set_progress(app: &App, text: &str) {
    app.global::<TrackInfoState>().set_progress(text.into());
}

/// (BPM × 100, key) our analysis found for `t` (bulk run or cache), `None` when not analysed.
fn found(t: &Track) -> Option<analyze::Found> {
    t.file
        .as_deref()
        .and_then(|f| analyze::detected().lookup(f))
}

/// "detected 125.00 BPM (library 128.00)" / "detected 9A (library 8A)" for each value that exists on both sides and differs.
fn detected_text(t: &Track, (bpm, key): &analyze::Found, notation: prefs::KeyNotation) -> String {
    let mut parts = Vec::new();
    if t.bpm_x100 > 0 && *bpm > 0 && (bpm - t.bpm_x100).abs() as f64 / 100.0 > BPM_TOLERANCE {
        parts.push(format!(
            "detected {:.2} BPM (library {})",
            *bpm as f64 / 100.0,
            t.bpm_text()
        ));
    }
    if !t.key.is_empty() && !key.is_empty() && analyze::keys_differ(&t.key, key) {
        parts.push(format!(
            "detected {} (library {})",
            display(key, notation),
            display(&t.key, notation)
        ));
    }
    parts.join(" · ")
}

/// Called with the updated track after an edit made in the panel (rating, comment, colour), so the track lists can refresh.
pub fn on_edit(f: impl Fn(&Track) + 'static) {
    LISTENER.with_borrow_mut(|l| *l = Some(Box::new(f)));
}

/// Saves one edit of the shown track to the store and re-shows it (`show` reads what the store holds). A store
/// failure is said in the panel.
fn edit(app: &App, save: impl FnOnce(&store::Store, &str) -> Result<(), String>) {
    let Some(t) = shown() else {
        return;
    };
    if let Err(e) = save(&store::global(), &store::key(&t)) {
        return set_progress(app, &format!("not saved: {e}"));
    }
    show(app, &t);
    if let Some(t) = shown() {
        LISTENER.with_borrow(|l| l.iter().for_each(|f| f(&t)));
    }
}

/// Connects the panel's edit callbacks and the notation-change refresh (once).
fn wire(app: &App) {
    let weak = app.as_weak();
    prefs::on_change(move |_| {
        if let (Some(app), Some(t)) = (weak.upgrade(), shown()) {
            show(&app, &t);
        }
    });
    let s = app.global::<TrackInfoState>();
    let weak = app.as_weak();
    s.on_rating_set(move |n| {
        let Some(cur) = SHOWN.with_borrow(|t| t.as_ref().map(|t| t.rating)) else {
            return;
        };
        let n = if i32::from(cur) == n {
            0
        } else {
            n.clamp(0, 5) as u8
        }; // the current star again clears
        edit(&weak.unwrap(), |s, k| s.set_rating(k, Some(n)));
    });
    let weak = app.as_weak();
    s.on_comment_edited(move |text| {
        let text = text.trim().to_owned();
        edit(&weak.unwrap(), |s, k| s.set_comment(k, Some(&text)));
    });
    let weak = app.as_weak();
    s.on_color_set(move |c| {
        let app = weak.unwrap();
        let cur = app.global::<TrackInfoState>().get_color();
        let new = (c != cur).then_some(c as u8); // the set colour again clears it
        edit(&app, |s, k| s.set_color(k, new));
    });
    s.set_color_palette(ModelRc::new(VecModel::from(
        store::TRACK_COLORS
            .iter()
            .map(|&(_, [r, g, b])| Color::from_rgb_u8(r, g, b))
            .collect::<Vec<_>>(),
    )));
}

/// Shows `t`; a cover that is missing or fails to decode is shown as "no art". Where the library has no BPM or key, ours is shown.
pub fn show(app: &App, t: &Track) {
    if !WIRED.replace(true) {
        wire(app);
    }
    // The browser's list may predate an edit made here: what the store holds wins.
    let edits = store::global().edits(&store::key(t)).unwrap_or_default();
    let mut t = t.clone();
    t.rating = edits.rating.unwrap_or(t.rating);
    t.comment = edits.comment.unwrap_or(t.comment);
    let t = &t;
    SHOWN.with_borrow_mut(|s| *s = Some(t.clone()));
    let s = app.global::<TrackInfoState>();
    s.set_color(edits.color.map_or(0, i32::from));
    let (ours, notation) = (
        found(t).unwrap_or_default(),
        prefs::with(|p| p.key_notation),
    );
    s.set_detected(detected_text(t, &ours, notation).into());
    let cover = t
        .artwork
        .as_deref()
        .and_then(|p| Image::load_from_path(p).ok());
    s.set_has_cover(cover.is_some());
    s.set_cover(cover.unwrap_or_default());
    s.set_has_track(true);
    s.set_title(t.title.as_str().into());
    s.set_artist(t.artist.as_str().into());
    s.set_genre(t.genre.as_str().into());
    let key = if t.key.is_empty() { &ours.1 } else { &t.key };
    s.set_key(display(key, notation).into());
    let bpm = if t.bpm_x100 > 0 { t.bpm_x100 } else { ours.0 };
    s.set_bpm(format!("{:.2}", bpm as f64 / 100.0).into());
    s.set_length(t.length_text().into());
    let details = [
        (t.bitrate > 0).then(|| format!("{} kbps", t.bitrate)),
        (t.sample_rate > 0).then(|| format!("{} kHz", t.sample_rate as f64 / 1000.0)),
        (!t.date_added.is_empty()).then(|| format!("added {}", t.date_added)),
    ];
    s.set_details(
        details
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("  ·  ")
            .into(),
    );
    s.set_comment(t.comment.as_str().into());
    s.set_phrases(phrases(t, edits.phrases).into());
    s.set_rating(t.rating.into());
}

/// "Phrases: High · 9" (mood, section count): the stored phrase edit, else the phrases the library's analysis of the track
/// holds, else empty. An edit that deleted every phrase shows "Phrases: none".
// ponytail: asks the library for the whole analysis (waveforms included) on every selection just to reach the phrases;
// fine for a click, cache or add a phrases-only op if arrow-key browsing lags.
fn phrases(t: &Track, edit: Option<openfader::analysis::Phrases>) -> String {
    let own = || LIBRARY.with_borrow(|l| l.as_ref()?.analysis(t)?.phrases);
    match edit.or_else(own) {
        Some(p) if p.list.is_empty() => "Phrases: none".into(),
        Some(p) => format!("Phrases: {:?} · {}", p.mood, p.list.len()),
        None => String::new(),
    }
}
