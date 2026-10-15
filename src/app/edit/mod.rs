//! EDIT mode ↔ ui/edit.slint (global `EditState`): the selected track as a timeline you cut up. The model is
//! [`openfader::edit`] (an edit-decision list over the decoded audio, undo / redo, beat snapping, preview,
//! WAV render); this side owns the view (zoom, scroll, selection, playhead), the pointer and key callbacks
//! and the hand-over to the decks. Children: [`open`] (load the browser's selected track on a worker),
//! [`ops`] (the edit buttons, preview, render, to-deck), [`nav`] (pointer, zoom, scroll, playhead follow),
//! [`text`] (labels and flags) and [`view`] (the timeline picture, drawn at the element's physical size like
//! the deck waveforms).
//!
//! The mode is shown while `EditState.visible` (set by the mode chips in ui/app.slint); leaving it stops the
//! preview. Pointer: a click puts the playhead (snapped), a drag selects a range (snapped to beats / bars);
//! the wheel scrolls, Ctrl + wheel zooms around the pointer.

mod nav;
mod open;
mod ops;
mod text;
mod view;

use super::{
    App, EditState,
    deck::Decks,
    render::{Axis, Canvas, Layers, three_band},
};
use openfader::{
    analysis::{self, Beat},
    edit::{
        Edit, Edl,
        load::Loaded,
        play::Play,
        snap::{self, Line, Snap},
    },
    library::{Library, Track},
};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::mpsc::Receiver,
    time::Duration,
};

/// Closest zoom, physical px per ms.
const MAX_PPM: f64 = 1.0;
/// A press that moves less than this many logical px is a click, not a selection.
const CLICK_PX: f32 = 3.0;

/// The track being edited.
struct Open {
    edit: Edit,
    track: Track,
    beats: Vec<Beat>,
    detail: Vec<[u8; 3]>,
    /// The 3-band waveform and the beat grid as the arrangement plays them (rebuilt by [`Open::refresh`]).
    layers: Vec<Layers>,
    lines: Vec<Line>,
}

impl Open {
    fn new(track: Track, l: Loaded) -> Open {
        let mut o = Open {
            edit: Edit::new(l.pcm),
            track,
            beats: l.beats,
            detail: l.detail,
            layers: vec![],
            lines: vec![],
        };
        o.refresh();
        o
    }

    /// The arrangement changed: redo the waveform and grid derived from it.
    fn refresh(&mut self) {
        let rate = self.edit.rate();
        self.lines = snap::lines(self.edit.edl(), &self.beats, rate);
        self.layers = three_band(&self.edit.edl().remap(
            &self.detail,
            analysis::DETAIL_ENTRIES_PER_SEC as f64 / rate as f64,
        ));
    }

    fn ms(&self, frame: usize) -> f64 {
        frame as f64 * 1000.0 / self.edit.rate() as f64
    }

    /// Four beats at the grid's tempo (a second without a grid), in frames.
    fn bar_frames(&self) -> usize {
        let rate = self.edit.rate() as f64;
        self.beats
            .first()
            .filter(|b| b.bpm_x100 > 0)
            .map_or(rate, |b| 4.0 * 60.0 * 100.0 / b.bpm_x100 as f64 * rate) as usize
    }
}

/// How the arrangement is laid on the picture right now.
struct Geo {
    axis: Axis,
    /// Physical size of the picture and physical px per logical px.
    w: usize,
    h: usize,
    k: usize,
    scale: f64,
}

struct State {
    app: slint::Weak<App>,
    lib: Rc<dyn Library>,
    decks: Decks,
    selected: Rc<RefCell<Option<Track>>>,
    open: RefCell<Option<Open>>,
    loading: RefCell<Option<Receiver<Result<(Track, Loaded), String>>>>,
    /// Logical size of the picture element, reported by the layout.
    size: Cell<(f32, f32)>,
    /// View: left edge in ms and zoom in physical px per ms (`None` = the whole arrangement).
    start_ms: Cell<f64>,
    ppm: Cell<Option<f64>>,
    sel: RefCell<Option<Range<usize>>>,
    cursor: Cell<usize>,
    snap: Cell<Snap>,
    /// The press in progress: arrangement frame and logical x it started at; whether it became a drag.
    press: Cell<Option<(usize, f32)>>,
    moved: Cell<bool>,
    play: RefCell<Option<Play>>,
    /// The arrangement last rendered to a file and where (what TO DECK loads).
    rendered: RefCell<Option<(Edl, PathBuf)>>,
    /// OPEN SELECTED was pressed once over unrendered edits.
    discard: Cell<bool>,
    /// The static picture, dropped whenever anything it shows changes.
    base: RefCell<Option<Canvas>>,
    dirty: Cell<bool>,
    status: RefCell<String>,
    timer: Timer,
}

impl State {
    fn say(&self, text: impl Into<String>) {
        *self.status.borrow_mut() = text.into();
        self.dirty.set(true);
    }

    fn sel(&self) -> Option<Range<usize>> {
        self.sel.borrow().clone()
    }

    /// The picture's geometry; `None` without a track or before the layout has sized the element.
    fn geo(&self, o: &Open) -> Option<Geo> {
        let app = self.app.upgrade()?;
        let scale = app.window().scale_factor() as f64;
        let (w, h) = (
            self.size.get().0 as f64 * scale,
            self.size.get().1 as f64 * scale,
        );
        let len_ms = o.ms(o.edit.len());
        if w < 1.0 || h < 1.0 || len_ms <= 0.0 {
            return None;
        }
        let fit = w / len_ms;
        let ppm = self
            .ppm
            .get()
            .map_or(fit, |p| p.clamp(fit, MAX_PPM.max(fit)));
        let start_ms = self.start_ms.get().clamp(0.0, (len_ms - w / ppm).max(0.0));
        Some(Geo {
            axis: Axis {
                start_ms,
                px_per_ms: ppm,
            },
            w: w.round() as usize,
            h: h.round() as usize,
            k: scale.round().max(1.0) as usize,
            scale,
        })
    }

    /// Something the picture shows changed.
    fn invalidate(&self) {
        *self.base.borrow_mut() = None;
        self.dirty.set(true);
    }

    fn redraw(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let open = self.open.borrow();
        let Some((o, g)) = open.as_ref().and_then(|o| Some((o, self.geo(o)?))) else {
            app.global::<EditState>().set_timeline(Default::default());
            return;
        };
        let mut base = self.base.borrow_mut();
        let base = base.get_or_insert_with(|| {
            view::base(&view::Scene {
                axis: g.axis,
                w: g.w,
                h: g.h,
                k: g.k,
                layers: &o.layers,
                lines: &o.lines,
                segs: o.edit.edl().segs(),
                rate: o.edit.rate(),
                sel: self.sel(),
                snap_on: self.snap.get() != Snap::Off,
            })
        });
        let x = g.axis.x(o.ms(self.cursor.get())).round() as i64;
        app.global::<EditState>()
            .set_timeline(view::with_playhead(base, x));
    }

    /// 30 ms: picks up a finished load, moves the playhead while previewing, redraws what changed.
    fn tick(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if !app.global::<EditState>().get_visible() {
            self.stop();
            return;
        }
        self.poll_loading();
        if let Some(p) = &*self.play.borrow() {
            self.cursor.set(
                p.pos()
                    .min(self.open.borrow().as_ref().map_or(0, |o| o.edit.len())),
            );
            self.dirty.set(true);
        }
        if self.play.borrow().as_ref().is_some_and(|p| p.ended()) {
            self.stop();
        }
        self.follow();
        if self.dirty.take() {
            self.push();
            self.redraw();
        }
    }
}

/// Connects `EditState` to the browser's selection, the decks and the audio output.
pub fn wire(app: &App, lib: Rc<dyn Library>, decks: Decks, selected: Rc<RefCell<Option<Track>>>) {
    let state = Rc::new(State {
        app: app.as_weak(),
        lib,
        decks,
        selected,
        open: RefCell::default(),
        loading: RefCell::default(),
        size: Cell::new((0.0, 0.0)),
        start_ms: Cell::new(0.0),
        ppm: Cell::new(None),
        sel: RefCell::default(),
        cursor: Cell::new(0),
        snap: Cell::new(Snap::Bar),
        press: Cell::new(None),
        moved: Cell::new(false),
        play: RefCell::default(),
        rendered: RefCell::default(),
        discard: Cell::new(false),
        base: RefCell::default(),
        dirty: Cell::new(true),
        status: RefCell::new("EDIT: OPEN SELECTED loads the track selected in the library".into()),
        timer: Timer::default(),
    });
    let g = app.global::<EditState>();
    on!(g, state, on_opened, |s| s.opened());
    on!(g, state, on_open_selected, |s| s.open_selected());
    on!(g, state, on_resized, |s, w, h| {
        s.size.set((w, h));
        s.invalidate();
    });
    on!(g, state, on_press, |s, x| s.press(x));
    on!(g, state, on_drag, |s, x| s.drag(x));
    on!(g, state, on_release, |s, x| s.release(x));
    on!(g, state, on_scroll, |s, dx, dy, x, ctrl| s
        .scroll(dx, dy, x, ctrl));
    on!(g, state, on_zoom, |s, d| s.zoom(d));
    on!(g, state, on_set_snap, |s, m| s.set_snap(m));
    on!(g, state, on_copy, |s| s.copy());
    on!(g, state, on_cut, |s| s.cut());
    on!(g, state, on_paste, |s| s.paste());
    on!(g, state, on_insert, |s| s.insert());
    on!(g, state, on_delete, |s| s.delete());
    on!(g, state, on_clear, |s| s.clear());
    on!(g, state, on_silence, |s| s.silence());
    on!(g, state, on_duplicate, |s| s.duplicate());
    on!(g, state, on_select_all, |s| s.select_all());
    on!(g, state, on_undo, |s| s.history(true));
    on!(g, state, on_redo, |s| s.history(false));
    on!(g, state, on_play_toggle, |s| s.play_toggle());
    on!(g, state, on_rewind, |s| s.rewind());
    on!(g, state, on_render, |s| s.render());
    on!(g, state, on_to_deck, |s, i| s.to_deck(i as usize));
    let weak = Rc::downgrade(&state);
    state
        .timer
        .start(TimerMode::Repeated, Duration::from_millis(30), move || {
            if let Some(s) = weak.upgrade() {
                s.tick();
            }
        });
}
