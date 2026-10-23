//! Selected-track details panel (`TrackInfoState` in ui/trackinfo.slint): cover, tags, rating, comment, phrase mood,
//! our detected BPM/key where it differs from the library's, and the bulk-analysis progress ([`super::analyze`]).

use super::{App, TrackInfoState, analyze, camelot::display, prefs};
use openfader::{
    library::{Library, Track},
    store,
};
use slint::{Color, ComponentHandle, ModelRc, VecModel};
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
    /// A browser editor pins its target even if a controller changes the selection behind it.
    static EDITING: RefCell<Option<Track>> = const { RefCell::new(None) };
    static WIRED: Cell<bool> = const { Cell::new(false) };
    static LISTENER: RefCell<Option<Box<dyn Fn(&Track)>>> = const { RefCell::new(None) };
    /// The library the panel reads the selected track's phrases from.
    static LIBRARY: RefCell<Option<Rc<dyn Library>>> = const { RefCell::new(None) };
}

/// The browser's library: where the panel finds the analysis of the track it shows.
pub fn use_library(lib: Rc<dyn Library>) {
    LIBRARY.with_borrow_mut(|l| *l = Some(lib));
}

/// The shown track as the library lists it (the store's edits are read again on every [`show`]).
fn shown() -> Option<Track> {
    SHOWN.with_borrow(|t| t.clone())
}

fn target() -> Option<Track> {
    EDITING.with_borrow(|t| t.clone()).or_else(shown)
}

/// Opens the existing details editor for the browser action's track, not a loaded deck.
pub fn open_editor(app: &App, t: &Track) {
    show(app, t);
    EDITING.with_borrow_mut(|e| *e = Some(t.clone()));
    display_track(app, t);
    let s = app.global::<TrackInfoState>();
    let (t, _) = edited(t);
    s.set_metadata_writable(t.file.is_some());
    s.set_edit_title(t.title.into());
    s.set_edit_artist(t.artist.into());
    s.set_edit_album(t.album.into());
    s.set_edit_genre(t.genre.into());
    s.set_edit_label(t.label.into());
    s.set_edit_status("".into());
    s.set_editor_open(true);
}

/// Re-shows the shown track as the library lists it now (after an undo or redo changed its row or its edits).
pub fn reshow(app: &App) {
    let Some(t) = shown() else {
        return;
    };
    show(app, &super::browser::fresh_track(&t).unwrap_or(t));
}

/// `t` with the store's metadata overrides, and every edit of it.
fn edited(t: &Track) -> (Track, store::Edits) {
    let edits = store::global().edits(&store::key(t)).unwrap_or_default();
    let mut t = t.clone();
    edits.apply_metadata(&mut t);
    (t, edits)
}

/// Copies only editable fields; origin, identity, technical data and analysis references stay put.
pub(in crate::app) fn copy_metadata(to: &mut Track, from: &Track) {
    to.title.clone_from(&from.title);
    to.artist.clone_from(&from.artist);
    to.album.clone_from(&from.album);
    to.genre.clone_from(&from.genre);
    to.label.clone_from(&from.label);
    to.comment.clone_from(&from.comment);
    to.rating = from.rating;
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
    analyze::detected().lookup(t)
}

/// "detected 125.00 BPM (library 128.00)" / "detected 9A (library 8A)" for each value that exists on both sides and differs.
fn detected_text(t: &Track, (bpm, key): &analyze::Found, notation: prefs::KeyNotation) -> String {
    let mut parts = Vec::new();
    if t.bpm_x100 > 0 && *bpm > 0 && (bpm - t.bpm_x100).abs() as f64 / 100.0 > BPM_TOLERANCE {
        let digits = prefs::with(|p| p.view.tempo_decimals.digits());
        parts.push(tr!(
            "detected {} BPM (library {})",
            format!("{:.*}", digits, *bpm as f64 / 100.0),
            format!("{:.*}", digits, t.bpm_x100 as f64 / 100.0)
        ));
    }
    if !t.key.is_empty() && !key.is_empty() && analyze::keys_differ(&t.key, key) {
        parts.push(tr!(
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

/// Persists the same edits from the details panel and browser editor. A failure leaves the draft intact.
fn edit(app: &App, save: impl FnOnce(&store::Store, &str) -> Result<(), String>) {
    let Some(t) = target() else {
        return;
    };
    let s = app.global::<TrackInfoState>();
    let draft = s.get_comment();
    let old_comment = edited(&t).0.comment;
    if let Err(e) = save(&store::global(), &store::key(&t)) {
        let message = tr!("not saved: {}", e);
        s.set_edit_status(message.clone().into());
        return set_progress(app, &message);
    }
    display_track(app, &t);
    let (t, _) = edited(&t);
    // Rating and colour edits must not discard a comment still being typed.
    if t.comment == old_comment {
        s.set_comment(draft);
    }
    SHOWN.with_borrow_mut(|shown| {
        if shown
            .as_ref()
            .is_some_and(|s| store::key(s) == store::key(&t))
        {
            *shown = Some(t.clone());
        }
    });
    s.set_edit_status(tr!("Saved in OpenFadeR").into());
    LISTENER.with_borrow(|l| l.iter().for_each(|f| f(&t)));
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
    s.on_editor_closed(move || {
        let app = weak.unwrap();
        EDITING.with_borrow_mut(|e| *e = None);
        app.global::<TrackInfoState>().set_editor_open(false);
        reshow(&app);
    });
    let weak = app.as_weak();
    s.on_rating_set(move |n| {
        let Some(cur) = target().map(|t| edited(&t).0.rating) else {
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
    s.on_metadata_edited(move |title, artist, album, genre, label| {
        let app = weak.unwrap();
        if target().is_none_or(|t| t.file.is_none()) {
            app.global::<TrackInfoState>().set_edit_status(
                tr!("Import a local file before editing descriptive fields").into(),
            );
            return;
        }
        let comment = app.global::<TrackInfoState>().get_comment();
        edit(&app, |s, k| {
            s.set_metadata(
                k,
                title.as_str(),
                artist.as_str(),
                album.as_str(),
                genre.as_str(),
                label.as_str(),
                comment.trim(),
            )
        });
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
    SHOWN.with_borrow_mut(|s| *s = Some(t.clone()));
    if EDITING.with_borrow(Option::is_none) {
        display_track(app, t);
    }
}

fn display_track(app: &App, t: &Track) {
    let (t, edits) = edited(t);
    let t = &t;
    let s = app.global::<TrackInfoState>();
    s.set_color(edits.color.map_or(0, i32::from));
    let (ours, notation) = (
        found(t).unwrap_or_default(),
        prefs::with(|p| p.key_notation),
    );
    s.set_detected(detected_text(t, &ours, notation).into());
    let cover = super::artwork::cover(&store::global(), t, 160);
    s.set_has_cover(cover.is_some());
    s.set_cover(cover.unwrap_or_default());
    s.set_has_track(true);
    s.set_title(t.title.as_str().into());
    s.set_artist(t.artist.as_str().into());
    s.set_genre(t.genre.as_str().into());
    let key = if t.key.is_empty() { &ours.1 } else { &t.key };
    s.set_key(display(key, notation).into());
    let bpm = if t.bpm_x100 > 0 { t.bpm_x100 } else { ours.0 };
    let digits = prefs::with(|p| p.view.tempo_decimals.digits());
    s.set_bpm(format!("{:.*}", digits, bpm as f64 / 100.0).into());
    s.set_length(t.length_text().into());
    let details = [
        (t.bitrate > 0).then(|| format!("{} kbps", t.bitrate)),
        (t.sample_rate > 0).then(|| format!("{} kHz", t.sample_rate as f64 / 1000.0)),
        (!t.date_added.is_empty()).then(|| tr!("added {}", t.date_added)),
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
    s.set_stems(stems(t).into());
    s.set_rating(t.rating.into());
}

/// "Phrases: High · 9" (mood, section count): the stored phrase edit, else the phrases the library's analysis of the track
/// holds, else empty. An edit that deleted every phrase shows "Phrases: none".
// ponytail: asks the library for the whole analysis (waveforms included) on every selection just to reach the phrases;
// fine for a click, cache or add a phrases-only op if arrow-key browsing lags.
fn phrases(t: &Track, edit: Option<openfader::analysis::Phrases>) -> String {
    let own = || LIBRARY.with_borrow(|l| l.as_ref()?.analysis(t)?.phrases);
    match edit.or_else(own) {
        Some(p) if p.list.is_empty() => tr!("Phrases: none").into(),
        Some(p) => tr!("Phrases: {} · {}", format!("{:?}", p.mood), p.list.len()),
        None => String::new(),
    }
}

/// "Stem file: Drums · Bass · Melody · Vocals" when the track's file names its four parts, else empty.
fn stems(t: &Track) -> String {
    t.file
        .as_deref()
        .and_then(openfader::audio::stemfile::read_info)
        .map_or_else(String::new, |i| tr!("Stem file: {}", i.label()).into())
}
