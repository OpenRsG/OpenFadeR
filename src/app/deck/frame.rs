//! What moves every frame, drawn in step with the display. The enlarged waveform is a still picture three views wide
//! ([`Strip`], `render::detail::render_strip`) that slides under a fixed playhead (`ui/deck-wave.slint`); it is drawn
//! again only when the playhead nears its edge or what it shows changes (zoom, size, cues, loop, settings: `Loaded::invalidate`).
//! The platter, the slip ghost, the beat counter and the phase meter move with it ([`DeckScroll`]).
//!
//! [`wire`] hooks the window's rendering: right before each frame every deck works out the position heard *now* and
//! places the strip ([`Deck::scroll`]), so the steps between frames are even whatever the timers do; while a deck plays
//! the next frame is asked for straight away, no sooner than VIEW > Waveform Drawing Rate allows. While paused (or when
//! the renderer has no frame notifier), the deck tick does the same.

use super::{Deck, Decks, Loaded, active, render, tick::jog_angle};
use crate::app::{App, DeckScroll, prefs};
use slint::{ComponentHandle, Model, RenderingState, Timer, TimerMode};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

/// Views of track the strip holds: one before the visible one and one after it.
const VIEWS: f64 = 3.0;
/// Tolerated zoom mismatch before the strip is drawn again (a pitch fader in motion would otherwise redraw it every frame).
const SPAN_SLACK: f64 = 0.005;

/// The picture the detail view slides: `w` physical pixels from `start_ms` at `px_per_ms`.
pub(super) struct Strip {
    key: Key,
    start_ms: f64,
    span: f64,
    px_per_ms: f64,
}

/// What the picture depends on besides the position: view size (physical), loop, cue point, GRID edit.
type Key = ((usize, usize), Option<(u32, u32)>, u32, bool);

impl Strip {
    /// Still covers the view around `pos` at zoom `span` with view key `key`.
    fn fits(&self, key: &Key, pos: f64, span: f64) -> bool {
        self.key == *key
            && (span / self.span - 1.0).abs() <= SPAN_SLACK
            && pos >= self.start_ms + self.span / 2.0
            && pos <= self.start_ms + (VIEWS - 0.5) * self.span
    }
}

impl Deck {
    /// Places the strip for the audible position `pos` (drawing it again when it no longer covers the view) and moves the
    /// playhead-bound parts. True when the strip was drawn again: the caller pushes the `DeckView` row (`detail`).
    pub(super) fn scroll(&self, app: &App, l: &mut Loaded, pos: f64) -> bool {
        let scale = app.window().scale_factor();
        let (lw, lh) = self.detail_size.get();
        let (w, h) = ((lw * scale).round() as usize, (lh * scale).round() as usize);
        let mut drawn = false;
        let mut row = DeckScroll {
            slip_x: -1.0,
            jog_angle: jog_angle(pos),
            overview_slip_x: -1.0,
            ..Default::default()
        };
        if render::backend::gpu() {
            let ow = (self.overview_size.get().0 * scale).round() as usize;
            if ow > 0 && l.duration_ms > 0 {
                row.overview_x =
                    (render::overview::playhead_x(ow, pos, l.duration_ms) - 1) as f32 / scale;
                if let Some(slip) = self.with_player(|p| p.slip_position()).flatten() {
                    row.overview_slip_x =
                        render::overview::playhead_x(ow, slip * 1000.0, l.duration_ms) as f32
                            / scale;
                }
            }
        }
        if w > 0 && h > 0 {
            // A fixed stretch of *playing* time: at rate r the view covers r × the zoom in track time, so synced decks at
            // different pitches draw their beats the same distance apart.
            let span = f64::from(self.span_ms.get()) * self.rate().abs().max(0.1);
            let grid_edit = self.view.borrow().grid_edit;
            let key = ((w, h), l.loop_, l.cue_ms, grid_edit);
            if !l.strip.as_ref().is_some_and(|s| s.fits(&key, pos, span)) {
                let start_ms = pos - span;
                let px_per_ms = w as f64 / span;
                let image = render::detail::render_strip(
                    l.detail.as_ref(),
                    &l.analysis,
                    start_ms,
                    px_per_ms,
                    l.cue_ms,
                    l.loop_,
                    (w as f64 * VIEWS) as usize,
                    h,
                    grid_edit,
                );
                self.view.borrow_mut().detail = image;
                l.strip = Some(Strip {
                    key,
                    start_ms,
                    span,
                    px_per_ms,
                });
                drawn = true;
            }
            let s = l.strip.as_ref().expect("drawn above");
            let half = w as f64 / 2.0;
            // Whole physical pixels: the picture is shown 1:1, never resampled.
            let x = |ms: f64| ((ms - s.start_ms) * s.px_per_ms).round();
            row.strip_x = ((half - x(pos)) / f64::from(scale)) as f32;
            row.strip_w = (w as f64 * VIEWS / f64::from(scale)) as f32;
            if let Some(slip) = self.with_player(|p| p.slip_position()).flatten() {
                row.slip_x = ((half + x(slip * 1000.0) - x(pos)) / f64::from(scale)) as f32;
            }
            if let Some(r) = super::ruler::ruler(&l.analysis.beats, pos, span, f64::from(lw)) {
                row.ruler_bar = r.first;
                row.ruler_x = r.x;
                row.ruler_w = r.pitch;
                row.ruler_step = r.step;
                row.ruler_last = r.last;
            }
            if h >= 40 {
                let mode = prefs::with(|p| p.view.beat_count);
                row.beat_text =
                    render::detail::beat_count_text(mode, &l.analysis.beats, &l.analysis.cues, pos)
                        .unwrap_or_default()
                        .into();
            }
        }
        if let Some((own, reference)) = self.phase_meter(&l.analysis.beats, pos) {
            row.phase_show = true;
            (row.phase_own, row.phase_ref) = (own as f32, reference as f32);
            row.phase_off = super::tick::phase_offset(own, reference, f64::from(super::grid::per_bar(&l.analysis.beats))) as f32;
        }
        if self.scroll.row_data(self.index).as_ref() != Some(&row) {
            self.scroll.set_row_data(self.index, row);
        }
        drawn
    }

    /// One frame of this deck: true while its picture moves (playing), so the next frame is wanted.
    fn frame(&self) -> bool {
        let (Some(app), Ok(mut l)) = (self.app.upgrade(), self.loaded.try_borrow_mut()) else {
            return false;
        };
        if !l.ready {
            return false;
        }
        let (pos, playing) = self.clock(&l);
        if self.scroll(&app, &mut l, pos) {
            drop(l);
            let v = self.view.borrow().clone();
            self.model.set_row_data(self.index, v);
        }
        playing || self.with_player(|p| p.is_scratching()).unwrap_or(false)
    }
}

thread_local! {
    /// Shortest time between two frames asked for while a deck plays (VIEW > Waveform Drawing Rate, with room for jitter).
    static MIN_FRAME: Cell<Duration> = const { Cell::new(Duration::from_millis(14)) };
    /// A registered rendering notifier updates moving decks; their timer need not do the same work between frames.
    static FRAME_DRIVEN: Cell<bool> = const { Cell::new(false) };
}

/// VIEW > Waveform Drawing Rate as the frame pacing.
pub(super) fn set_rate(period: Duration) {
    MIN_FRAME.set(period.mul_f32(0.9));
}

/// The timer places paused decks, and moving decks only when the rendering notifier is unavailable.
pub(super) fn tick_scroll(playing: bool) -> bool {
    !playing || !FRAME_DRIVEN.get()
}

/// Hooks the decks to the window's frames (see the module doc).
pub(super) fn wire(app: &App, decks: &Decks) {
    let decks = decks.clone();
    let weak = app.as_weak();
    let started = Cell::new(Instant::now());
    let moving = Cell::new(false);
    let timer = Rc::new(Timer::default());
    let set = app
        .window()
        .set_rendering_notifier(move |state, api| match state {
            RenderingState::RenderingSetup => {
                crate::app::diag::graphics(api);
                if let Some(app) = weak.upgrade() {
                    render::backend::setup(api, &app);
                }
            }
            RenderingState::RenderingTeardown => render::backend::teardown(),
            RenderingState::BeforeRendering => {
                started.set(Instant::now());
                moving.set(decks[..active()].iter().fold(false, |m, d| d.frame() | m));
            }
            RenderingState::AfterRendering => {
                if let Some(app) = weak.upgrade() {
                    crate::app::log_first_frame(&app);
                }
                if !moving.get() {
                    return;
                }
                let due = started.get() + MIN_FRAME.get();
                let now = Instant::now();
                let w = weak.clone();
                let redraw = move || {
                    if let Some(app) = w.upgrade() {
                        app.window().request_redraw();
                    }
                };
                if now >= due {
                    redraw();
                } else {
                    timer.start(TimerMode::SingleShot, due - now, redraw);
                }
            }
            _ => {}
        });
    FRAME_DRIVEN.set(set.is_ok());
    if let Err(e) = set {
        eprintln!(
            "Slint renderer: software / no graphics notifier ({e:?}); waveform renderer: CPU"
        );
        crate::app::diag::software(&format!("{e:?}"));
        let weak = app.as_weak();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(app) = weak.upgrade() {
                crate::app::log_first_frame(&app);
            }
        });
    }
    let weak = app.as_weak();
    prefs::on_change(move |_| {
        if let Some(app) = weak.upgrade() {
            render::backend::apply(&app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tick_only_leaves_playback_scrolling_to_a_registered_frame_notifier() {
        FRAME_DRIVEN.set(false);
        assert!(tick_scroll(false));
        assert!(tick_scroll(true));
        FRAME_DRIVEN.set(true);
        assert!(tick_scroll(false));
        assert!(!tick_scroll(true));
        FRAME_DRIVEN.set(false);
    }

    #[test]
    fn the_strip_is_drawn_again_only_near_its_edges_or_when_what_it_shows_changes() {
        let key: Key = ((1000, 100), None, 0, false);
        // Drawn at 10 s with an 8 s view: it holds 2 s to 26 s, the view needs pos ± 4 s.
        let s = Strip {
            key,
            start_ms: 2000.0,
            span: 8000.0,
            px_per_ms: 0.125,
        };
        assert!(s.fits(&key, 10_000.0, 8000.0));
        assert!(
            s.fits(&key, 22_000.0, 8000.0),
            "the view's right edge is the strip's"
        );
        assert!(
            !s.fits(&key, 22_001.0, 8000.0),
            "past it the view would show beyond the picture"
        );
        assert!(!s.fits(&key, 5_999.0, 8000.0), "same going back");
        assert!(
            s.fits(&key, 10_000.0, 8000.0 * 1.004),
            "a pitch fader in motion keeps the picture"
        );
        assert!(!s.fits(&key, 10_000.0, 8000.0 * 1.01));
        assert!(
            !s.fits(&((1000, 100), Some((1, 2)), 0, false), 10_000.0, 8000.0),
            "a new loop is drawn"
        );
    }
}
