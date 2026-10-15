//! VIDEO mode ↔ ui/video.slint (`VideoState`, `VideoWindow`). Each deck whose track is a video file gets a
//! [`Stream`] (an ffmpeg decode worker, see `src/video/`); a 30 Hz timer, running only while the VIDEO view or the output
//! window is up, tells each stream the frame under its deck's audible playhead, mixes the frames by the crossfader
//! ([`mix::weights`] over [`mixer::sides`]: FADE dissolves, CUT switches at the middle, optionally dimmed by the channel
//! faders) and shows the result in the view and in the separate output window. Text / image overlays are drawn by the UI on
//! top of that picture. The video list is the library's mp4 / mov / m4v tracks with a LOAD button per deck.
//! With both views closed the streams are dropped (their ffmpeg processes end) and come back on the next look.
//!
//! Children, all `impl Video` except `slot`: `slot` (per-deck source and frame), `list` (library list, LOAD), `overlay`
//! (image overlay), `window` (output window), `tick` (the 30 Hz mix).

mod list;
mod overlay;
mod slot;
mod tick;
mod window;

use super::{
    App, MixerState, VideoDeckView, VideoItem, VideoState, VideoWindow,
    deck::{DECKS, Decks},
    mixer, prefs,
};
use openfader::library::{Library, Track};
use openfader::video::{HEIGHT, WIDTH, mix};
use slint::{
    ComponentHandle, Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel,
};
use slot::Slot;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

struct Video {
    app: slint::Weak<App>,
    decks: Decks,
    lib: Rc<dyn Library>,
    slots: RefCell<[Slot; DECKS]>,
    rows: Rc<VecModel<VideoDeckView>>,
    list: RefCell<Vec<Track>>,
    window: RefCell<Option<VideoWindow>>,
    /// The mixed picture and what it was mixed from (frame addresses, weights): remixed only when one changes.
    out: RefCell<Image>,
    mixed_from: Cell<[(usize, u32); DECKS]>,
    /// Result of the last action (overlay image, library read), shown until the next one.
    note: RefCell<String>,
    _timer: Timer,
}

/// A zeroed frame-sized buffer.
fn frame_buffer() -> SharedPixelBuffer<Rgb8Pixel> {
    SharedPixelBuffer::new(WIDTH as u32, HEIGHT as u32)
}

pub fn wire(app: &App, lib: Rc<dyn Library>, decks: Decks) -> impl Sized {
    let rows = Rc::new(VecModel::default());
    let state = app.global::<VideoState>();
    state.set_decks(ModelRc::from(rows.clone()));
    let v = Rc::new_cyclic(|me: &std::rc::Weak<Video>| {
        let weak = me.clone();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
            if let Some(v) = weak.upgrade() {
                v.tick();
            }
        });
        Video {
            app: app.as_weak(),
            decks,
            lib,
            slots: RefCell::default(),
            rows,
            list: RefCell::default(),
            window: RefCell::default(),
            out: RefCell::new(Image::default()),
            mixed_from: Cell::new([(0, 0); DECKS]),
            note: RefCell::default(),
            _timer: timer,
        }
    });
    on!(state, weak v, on_opened, |v| v.refresh_list());
    on!(state, weak v, on_load, |v, row, deck| v.load(row as usize, deck as usize));
    on!(state, weak v, on_image_chosen, |v, path| v.choose_image(path.as_str()));
    on!(state, weak v, on_clear_overlay, |v| v.clear_overlay());
    on!(state, weak v, on_window_toggled, |v| v.toggle_window());
    on!(state, weak v, on_full_screen, |v| v.full_screen());
    let w = Rc::downgrade(&v);
    prefs::on_change(move |p| {
        if let Some(v) = w.upgrade() {
            v.set_enabled(p.extensions.video);
        }
    });
    v.set_enabled(prefs::with(|p| p.extensions.video));
    v
}

impl Video {
    /// EXTENSIONS > Video: off takes the VIDEO view and the output window away (and the global bar offers no VIDEO button).
    fn set_enabled(&self, on: bool) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let st = app.global::<VideoState>();
        st.set_enabled(on);
        if !on {
            st.set_visible(false);
            if self.window_open() {
                self.toggle_window();
            }
        }
    }
}
