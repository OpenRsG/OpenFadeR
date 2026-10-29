//! What moves every frame, drawn in step with the display. The enlarged waveform is a still picture three views wide
//! ([`Strip`], `render::detail::render_strip`) that slides under a fixed playhead (`ui/deck-wave.slint`); it is drawn
//! again only when the playhead nears its edge or what it shows changes (zoom, size, cues, loop, settings: `Loaded::invalidate`).
//! The platter, the slip ghost, the beat counter and the phase meter move with it ([`DeckScroll`]).
//!
//! [`wire`] prepares each moving deck's next frame before requesting a redraw, so the renderer sees its new position when
//! it prepares the scene and dirty regions. The rendering notifier paces these requests to the display, no sooner than
//! VIEW > Waveform Drawing Rate allows. While paused (or when the renderer has no frame notifier), the deck tick does the
//! same; transport returns also place their final frame immediately.

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
    /// Places the strip for the audible position `pos` (drawing it again, into `DeckLive.detail`, when it no longer covers
    /// the view) and moves the playhead-bound parts.
    pub(super) fn scroll(&self, app: &App, l: &mut Loaded, pos: f64) {
        let scale = app.window().scale_factor();
        let (lw, lh) = self.detail_size.get();
        let (w, h) = ((lw * scale).round() as usize, (lh * scale).round() as usize);
        let opts = super::tick::opts();
        let mut row = DeckScroll {
            slip_x: -1.0,
            jog_angle: jog_angle(pos),
            jog_laps: opts
                .countdown
                .then(|| super::tick::laps_to_hot_cue(&l.analysis.cues, pos))
                .flatten()
                .unwrap_or(-1.0),
            overview_slip_x: -1.0,
            ..Default::default()
        };
        let ow = (self.overview_size.get().0 * scale).round() as usize;
        if ow > 0 && l.duration_ms > 0 {
            row.overview_x =
                (render::overview::playhead_x(ow, pos, l.duration_ms) - 1) as f32 / scale;
            if let Some(slip) = self.with_player(|p| p.slip_position()).flatten() {
                row.overview_slip_x =
                    render::overview::playhead_x(ow, slip * 1000.0, l.duration_ms) as f32 / scale;
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
                self.set_live(|r| r.detail = image);
                l.strip = Some(Strip {
                    key,
                    start_ms,
                    span,
                    px_per_ms,
                });
            }
            let s = l.strip.as_ref().expect("drawn above");
            let half = (w / 2) as f64;
            // Use the same containing column as the waveform and its cue/grid markers.
            let axis = render::Axis {
                start_ms: s.start_ms,
                px_per_ms: s.px_per_ms,
            };
            let x = |ms: f64| axis.col(ms) as f64;
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
                let value = render::detail::beat_count_value(
                    mode,
                    &l.analysis.beats,
                    &l.analysis.cues,
                    pos,
                );
                if l.beat_count != value {
                    l.beat_count = value;
                    l.beat_text = value.map(|v| v.text().into()).unwrap_or_default();
                }
                row.beat_text = l.beat_text.clone();
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
    }

    /// Prepares the deck's next frame before the renderer prepares its scene.
    fn frame(&self) {
        let (Some(app), Ok(mut l)) = (self.app.upgrade(), self.loaded.try_borrow_mut()) else {
            return;
        };
        if !l.ready {
            return;
        }
        let (pos, _) = self.clock(&l);
        self.scroll(&app, &mut l, pos);
    }

    fn moving(&self) -> bool {
        self.with_player(|p| p.is_playing() || p.is_scratching())
            .unwrap_or(false)
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
            }
            RenderingState::AfterRendering => {
                if let Some(app) = weak.upgrade() {
                    crate::app::log_first_frame(&app);
                }
                // Unrelated UI repaints must not postpone an already scheduled waveform frame.
                if timer.running() || !decks[..active()].iter().any(|d| d.moving()) {
                    return;
                }
                let due = started.get() + MIN_FRAME.get();
                let now = Instant::now();
                let w = weak.clone();
                let decks = decks.clone();
                let redraw = move || {
                    if let Some(app) = w.upgrade() {
                        for d in &decks[..active()] {
                            d.frame();
                        }
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

    fn rig(count: usize) -> (App, Decks) {
        use crate::app::deck::Loaded;
        use openfader::{
            analysis::{Analysis, Beat, Cue},
            library::{Library, own::Own},
            store::Store,
        };
        crate::app::headless::init();
        let app = App::new().unwrap();
        let lib: Rc<dyn Library> = Rc::new(Own::new(std::sync::Arc::new(Store::in_memory().unwrap())));
        let decks = crate::app::deck::wire(&app, lib, Default::default());
        let a = Analysis {
            beats: (0..360)
                .map(|i| Beat {
                    time_ms: i * 500,
                    bar_pos: (i % 4 + 1) as u16,
                    bpm_x100: 12000,
                })
                .collect(),
            cues: vec![Cue {
                time_ms: 24000,
                hot_cue: 1,
                loop_end_ms: None,
                color: None,
                comment: None,
            }],
            three_band_detail: (0..27000)
                .map(|i| {
                    [
                        (i % 128) as u8,
                        ((i * 7) % 128) as u8,
                        ((i * 13) % 128) as u8,
                    ]
                })
                .collect(),
            three_band_preview: (0..400)
                .map(|i| {
                    [
                        (i % 128) as u8,
                        ((i * 7) % 128) as u8,
                        ((i * 13) % 128) as u8,
                    ]
                })
                .collect(),
            ..Default::default()
        };
        for d in &decks {
            d._timer.stop();
        }
        for d in &decks[..count] {
            let colors = render::deck_palette(d.index);
            *d.loaded.borrow_mut() = Loaded {
                ready: true, duration_ms: 180000,
                detail: render::detail::wave(&colors, &a, None),
                overview: render::overview::wave(&colors, &a, 180000),
                analysis: a.clone(), ..Default::default()
            };
            d.show(|v| { v.loaded = true; v.has_grid = true; });
        }
        (app, decks)
    }

    /// CPU time of this thread: what the frame costs, whatever else the machine runs.
    #[cfg(unix)]
    fn cpu() -> Duration {
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: valid out-pointer; CLOCK_THREAD_CPUTIME_ID exists on Linux.
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
        Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "10 s paced real-deck software rendering per case; --release --ignored --nocapture"]
    fn bench_waveform_ui_frames() {
        use openfader::audio::{Pcm, Track16};
        use slint::{PhysicalSize, Rgb8Pixel};
        crate::app::headless::repaint_changes_only();
        let pcm = std::sync::Arc::new(Track16::from(Pcm { rate: 8000, samples: vec![0.0; 180 * 8000 * 2] }));
        let (app, decks) = rig(4);
        app.show().unwrap();
        for count in [2, 4] {
            prefs::set_for_test(|p| p.decks = count);
            app.global::<crate::app::DeckState>().set_count(count as i32);
            let window = crate::app::headless::last_window();
            window.set_size(PhysicalSize::new(1920, 1080));
            let mut pixels = vec![Rgb8Pixel::default(); 1920 * 1080];
            // Repainted physical pixels of the frame: what the partial renderer found dirty.
            let draw = |pixels: &mut [Rgb8Pixel]| {
                app.window().request_redraw();
                let mut area = 0;
                window.draw_if_needed(|r| {
                    area = r.render(pixels, 1920).iter().map(|(_, s)| s.width as usize * s.height as usize).sum();
                });
                area
            };
            draw(&mut pixels);
            for d in &decks[..count] {
                assert!(d.detail_size.get().0 > 0.0, "detail must be visible: {:?}", d.detail_size.get());
                assert!(d.overview_size.get().0 > 0.0, "overview must be visible");
                let mut player = d.player.as_ref().expect("benchmark requires an audio output (silent PCM)").borrow_mut();
                player.load_shared(pcm.clone());
            }
            for fps in [60u32, 120] {
                FRAME_DRIVEN.set(true);
                for d in &decks[..count] {
                    let mut player = d.player.as_ref().unwrap().borrow_mut();
                    player.seek(20000);
                    player.play();
                }
                // Warm layout/image caches and let the audio-clock anchor become audible.
                for _ in 0..20 {
                    slint::platform::update_timers_and_animations();
                    for d in &decks[..count] { d.tick(); d.frame(); }
                    draw(&mut pixels);
                    std::thread::sleep(Duration::from_millis(10));
                }
                // The strip drawn at 20 s holds up to 32 s: the measured 10 s include one natural redraw per deck.
                for d in &decks[..count] { d.player.as_ref().unwrap().borrow_mut().seek(28000); }
                let budget = Duration::from_secs_f64(1.0 / f64::from(fps));
                let start = Instant::now();
                let mut times = Vec::with_capacity(10 * fps as usize);
                let mut updates = Vec::with_capacity(times.capacity());
                let keys = || {
                    decks[..count]
                        .iter()
                        .map(|d| {
                            let l = d.loaded.borrow();
                            (
                                l.strip.as_ref().map(|s| s.start_ms.to_bits()),
                                l.overview_key,
                            )
                        })
                        .collect::<Vec<_>>()
                };
                let (mut strips, mut overviews) = (0, 0);
                let mut areas = Vec::with_capacity(times.capacity());
                for frame in 0..10 * fps {
                    let due = start + budget.mul_f64(f64::from(frame));
                    std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    let before = keys();
                    let at = cpu();
                    slint::platform::update_timers_and_animations();
                    for d in &decks[..count] { d.tick(); d.frame(); }
                    updates.push(cpu() - at);
                    for (b, a) in before.iter().zip(keys()) {
                        strips += usize::from(b.0 != a.0);
                        overviews += usize::from(b.1 != a.1);
                    }
                    let area = draw(&mut pixels);
                    times.push(cpu() - at);
                    areas.push(area);
                }
                let dropped = times.iter().filter(|&&t| t > budget).count();
                times.sort_unstable();
                updates.sort_unstable();
                let n = times.len();
                eprintln!("WAVEFRAME decks={count} fps={fps} renderer=software-reused 1920x1080 thread-cpu frames={n} p50={:.3}ms p99={:.3}ms max={:.3}ms over_budget={dropped} repaint_px_mean={} repaint_px_max={} strip_redraws={strips} overview_redraws={overviews} update_p50={:.3}ms update_p99={:.3}ms sizes={:?}", times[n/2].as_secs_f64()*1000.0, times[n*99/100].as_secs_f64()*1000.0, times[n-1].as_secs_f64()*1000.0, areas.iter().sum::<usize>() / n, areas.iter().max().unwrap(), updates[n/2].as_secs_f64()*1000.0, updates[n*99/100].as_secs_f64()*1000.0, decks[..count].iter().map(|d| (d.detail_size.get(), d.overview_size.get())).collect::<Vec<_>>());
                for d in &decks[..count] { d.player.as_ref().unwrap().borrow_mut().pause(); }
                draw(&mut pixels);
                let same_view = { for d in &decks[..count] { d.model.set_row_data(d.index, d.view.borrow().clone()); } draw(&mut pixels) };
                let same_scroll = { for d in &decks[..count] { d.scroll.set_row_data(d.index, d.scroll.row_data(d.index).unwrap()); } draw(&mut pixels) };
                let same_live = { for d in &decks[..count] { d.live.set_row_data(d.index, d.live.row_data(d.index).unwrap()); } draw(&mut pixels) };
                eprintln!("WAVEFRAME decks={count} unchanged rows pushed again repaint: DeckView {same_view} px, DeckScroll {same_scroll} px, DeckLive {same_live} px");
            }
            FRAME_DRIVEN.set(false);
        }
    }

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

    /// Pins the gain of the `DeckLive` split (src/app/deck/{mod,tick,frame}.rs, `ui/deck-state.slint`): a frame whose
    /// playhead stays inside the strip's 3-view window moves the picture (`DeckScroll.strip-x`) without redrawing it
    /// (same `Strip` cache key, no `set_live`), and a frame with no state change at all repaints nothing.
    #[test]
    fn a_scroll_only_frame_keeps_the_strip_and_an_unchanged_frame_repaints_nothing() {
        use slint::{PhysicalSize, Rgb8Pixel};
        crate::app::headless::repaint_changes_only();
        let (app, decks) = rig(1);
        app.show().unwrap();
        decks[0].detail_size.set((400.0, 50.0));
        let window = crate::app::headless::last_window();
        window.set_size(PhysicalSize::new(400, 300));
        let mut pixels = vec![Rgb8Pixel::default(); 400 * 300];
        let draw = |pixels: &mut [Rgb8Pixel]| {
            app.window().request_redraw();
            let mut area = 0;
            window.draw_if_needed(|r| {
                area = r
                    .render(pixels, 400)
                    .iter()
                    .map(|(_, s)| s.width as usize * s.height as usize)
                    .sum();
            });
            area
        };
        draw(&mut pixels); // first paint, warms the layout and the strip cache

        let mut l = decks[0].loaded.borrow_mut();
        decks[0].scroll(&app, &mut l, 24_000.0);
        drop(l);
        draw(&mut pixels);
        let key_before = decks[0].loaded.borrow().strip.as_ref().map(|s| s.start_ms);

        let mut l = decks[0].loaded.borrow_mut();
        decks[0].scroll(&app, &mut l, 24_050.0); // 50 ms on: well inside the same strip, no edge, no zoom/cue/loop change
        drop(l);
        let moved = draw(&mut pixels);
        let key_after = decks[0].loaded.borrow().strip.as_ref().map(|s| s.start_ms);
        assert_eq!(
            key_before, key_after,
            "the strip picture is not redrawn for a small scroll inside its view"
        );
        assert!(moved > 0, "the playhead still moves something on screen");

        let still = draw(&mut pixels);
        assert_eq!(still, 0, "a frame with no state change repaints nothing");
    }
    #[test]
    fn returning_cue_ignores_key_repeat_until_release_and_play_still_latches() {
        use super::super::cue::Preview;
        let (_app, decks) = rig(1);
        let deck = &decks[0];
        deck.cue_state.set(Preview::Returned);
        for _ in 0..64 {
            deck.cue_press();
            assert_eq!(deck.cue_state.get(), Preview::Returned);
            assert!(
                !deck.cue_latch(),
                "PLAY after a return is ordinary playback"
            );
        }
        deck.cue_release();
        assert_eq!(deck.cue_state.get(), Preview::Off);
        deck.cue_state.set(Preview::Held);
        for _ in 0..64 {
            deck.cue_press();
            assert_eq!(deck.cue_state.get(), Preview::Held);
        }
        assert!(deck.cue_latch());
        deck.cue_release();
        assert_eq!(deck.cue_state.get(), Preview::Off);
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

    mod pixels {
        use slint::{ComponentHandle, Model, Rgb8Pixel, VecModel};
        use std::rc::Rc;

        slint::slint! {
            import { WaveStrip, DeckDetail } from "../../../ui/deck-wave.slint";
            import { Chip, RingButton } from "../../../ui/widgets.slint";
            import { StageButton } from "../../../ui/stage/controls.slint";
            import { BigButton } from "../../../ui/studio/parts.slint";
            import { DeckDetailV } from "../../../ui/deck-wave-v.slint";
            import { StageWave, StagePads } from "../../../ui/stage/deck-parts.slint";
            import { DeckState } from "../../../ui/deck-state.slint";
            export { DeckState, DeckView, DeckScroll, DeckLive } from "../../../ui/deck-state.slint";
            import { PadGrid } from "../../../ui/pads.slint";
            export { PadView } from "../../../ui/pads.slint";
            export { Theme } from "../../../ui/theme.slint";
            export component StripPixels inherits Window {
                in property <length> view-width;
                in property <length> view-height;
                min-width: 0px;
                min-height: 0px;
                background: #000;
                WaveStrip { index: 0; width: root.view-width; height: root.view-height; }
            }
            export component CueButtons inherits Window {
                min-width: 0px;
                min-height: 0px;
                callback down;
                callback up;
                callback clicked;
                Chip { x: 0px; width: 80px; height: 80px; pressed => { root.down(); } released => { root.up(); } clicked => { root.clicked(); } }
                RingButton { x: 80px; width: 80px; height: 80px; pressed => { root.down(); } released => { root.up(); } clicked => { root.clicked(); } }
                StageButton { x: 160px; width: 80px; height: 80px; pressed => { root.down(); } released => { root.up(); } clicked => { root.clicked(); } }
                BigButton { x: 240px; width: 80px; height: 80px; pressed => { root.down(); } released => { root.up(); } clicked => { root.clicked(); } }
            }
            export component CueWaves inherits Window {
                in property <int> index;
                min-width: 0px;
                min-height: 0px;
                DeckDetail { index: root.index; x: 0px; y: 0px; width: 320px; height: 100px; }
                DeckDetailV { index: root.index; y: 100px; width: 320px; height: 100px; }
                StageWave { index: root.index; y: 200px; width: 320px; height: 100px; }
            }
            export component PadGestures inherits Window {
                in property <int> index;
                min-width: 0px;
                min-height: 0px;
                PadGrid {
                    x: 0px; y: 0px;
                    index: root.index; width: 320px; height: 120px; mode: DeckState.decks[root.index].pad-mode; enabled: true;
                    pads: DeckState.decks[root.index].pads;
                    pad(d, p) => { DeckState.pad(d, p); }
                    pad-released(d, p, m) => { DeckState.pad-released-at(d, p, m); }
                }
                StagePads { index: root.index; y: 120px; width: 320px; height: 120px; tone: #11dd47; }
            }
        }

        #[test]
        fn performance_pad_cancel_releases_the_original_deck_and_pad_once() {
            use slint::{
                LogicalPosition, PhysicalSize,
                platform::{PointerEventButton as Button, WindowEvent as Event},
            };
            use std::cell::RefCell;
            crate::app::headless::init();
            let ui = PadGestures::new().unwrap();
            ui.global::<Theme>().set_t_fast(0);
            ui.show().unwrap();
            crate::app::headless::last_window().set_size(PhysicalSize::new(320, 240));
            let model = Rc::new(VecModel::from(
                (0..4)
                    .map(|d| DeckView {
                        loaded: true,
                        pad_mode: if d == 2 { 4 } else { 9 },
                        pads: Rc::new(VecModel::from(vec![
                            PadView {
                                color: slint::Color::from_rgb_u8(17, 221, 71),
                                ..Default::default()
                            };
                            8
                        ]))
                        .into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            ));
            let state = ui.global::<DeckState>();
            state.set_decks(model.clone().into());
            let events = Rc::new(RefCell::new(Vec::new()));
            let (seen, feedback) = (events.clone(), model.clone());
            state.on_pad(move |d, p| {
                let mut row = feedback.row_data(d as usize).unwrap();
                seen.borrow_mut().push((true, d, p, row.pad_mode));
                let mut pads: Vec<_> = row.pads.iter().collect();
                pads[p as usize].set = !pads[p as usize].set;
                if let Some(next) = crate::app::update_rows(&row.pads, pads) {
                    row.pads = next;
                    feedback.set_row_data(d as usize, row);
                }
            });
            let seen = events.clone();
            state.on_pad_released_at(move |d, p, m| seen.borrow_mut().push((false, d, p, m)));
            let window = crate::app::headless::last_window();
            let mut pixels = vec![Rgb8Pixel::default(); 320 * 240];
            ui.window().request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, 320);
            });
            for y in [11.0, 160.0] {
                let position = LogicalPosition::new(40.0, y);
                ui.set_index(0);
                let before = model.row_data(0).unwrap().pads.row_data(0).unwrap().set;
                ui.window().dispatch_event(Event::PointerPressed {
                    position,
                    button: Button::Left,
                });
                assert_ne!(
                    model.row_data(0).unwrap().pads.row_data(0).unwrap().set,
                    before,
                    "pad press at y={y}, events={:?}",
                    events.borrow()
                );
                ui.window().request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, 320);
                });
                assert_eq!(
                    pixels[y as usize * 320 + 40],
                    Rgb8Pixel {
                        r: 17,
                        g: 221,
                        b: 71
                    },
                    "rendered feedback follows the new row model at y={y}"
                );
                assert_eq!(
                    *events.borrow(),
                    [(true, 0, 0, 9)],
                    "rendering feedback must not release the held control"
                );
                ui.window().dispatch_event(Event::PointerReleased {
                    position,
                    button: Button::Left,
                });
                assert_eq!(
                    *events.borrow(),
                    [(true, 0, 0, 9), (false, 0, 0, 9)],
                    "feedback preserves the held tile"
                );
                events.borrow_mut().clear();
                ui.window().dispatch_event(Event::PointerPressed {
                    position,
                    button: Button::Left,
                });
                ui.set_index(2);
                ui.window().dispatch_event(Event::PointerExited);
                ui.window().dispatch_event(Event::PointerReleased {
                    position,
                    button: Button::Left,
                });
                assert_eq!(*events.borrow(), [(true, 0, 0, 9), (false, 0, 0, 9)]);
                events.borrow_mut().clear();
                ui.window().dispatch_event(Event::PointerPressed {
                    position,
                    button: Button::Right,
                });
                ui.window().dispatch_event(Event::PointerExited);
                ui.window().dispatch_event(Event::PointerReleased {
                    position,
                    button: Button::Right,
                });
                assert!(events.borrow().is_empty());
            }
        }

        #[test]
        fn waveform_gestures_keep_the_pressed_deck_through_layer_changes_and_cancel() {
            use slint::{
                LogicalPosition, PhysicalSize,
                platform::{PointerEventButton as Button, WindowEvent as Event},
            };
            use std::cell::RefCell;
            crate::app::headless::init();
            let ui = CueWaves::new().unwrap();
            ui.show().unwrap();
            crate::app::headless::last_window().set_size(PhysicalSize::new(320, 300));
            let state = ui.global::<DeckState>();
            state.set_decks(
                Rc::new(VecModel::from(vec![
                    DeckView {
                        loaded: true,
                        ..Default::default()
                    };
                    4
                ]))
                .into(),
            );
            state.set_click_wave(true);
            let events = Rc::new(RefCell::new(Vec::new()));
            let seen = events.clone();
            state.on_cue(move |d| seen.borrow_mut().push(("cue", d)));
            let seen = events.clone();
            state.on_cue_released(move |d| seen.borrow_mut().push(("cue-up", d)));
            let seen = events.clone();
            state.on_scratch_begin(move |d| seen.borrow_mut().push(("scratch", d)));
            let seen = events.clone();
            state.on_scratch_move(move |d, _| seen.borrow_mut().push(("move", d)));
            let seen = events.clone();
            state.on_scratch_end(move |d| seen.borrow_mut().push(("scratch-up", d)));
            let seen = events.clone();
            state.on_play_pause(move |d| seen.borrow_mut().push(("play", d)));
            for y in [50.0, 150.0, 250.0] {
                let position = LogicalPosition::new(160.0, y);
                ui.set_index(0);
                ui.window().dispatch_event(Event::PointerPressed {
                    position,
                    button: Button::Right,
                });
                ui.set_index(2);
                ui.window().dispatch_event(Event::PointerExited);
                ui.window().dispatch_event(Event::PointerReleased {
                    position,
                    button: Button::Right,
                });
                assert_eq!(*events.borrow(), [("cue", 0), ("cue-up", 0)]);
                events.borrow_mut().clear();
                ui.set_index(0);
                ui.window().dispatch_event(Event::PointerPressed {
                    position,
                    button: Button::Left,
                });
                ui.set_index(2);
                ui.window().dispatch_event(Event::PointerMoved {
                    position: LogicalPosition::new(180.0, y),
                });
                ui.window().dispatch_event(Event::PointerExited);
                ui.window().dispatch_event(Event::PointerReleased {
                    position,
                    button: Button::Left,
                });
                assert_eq!(
                    *events.borrow(),
                    [("scratch", 0), ("move", 0), ("scratch-up", 0)],
                    "cancel never clicks PLAY"
                );
                events.borrow_mut().clear();
                ui.set_index(0);
                ui.window().dispatch_event(Event::PointerPressed {
                    position,
                    button: Button::Left,
                });
                ui.set_index(2);
                ui.window().dispatch_event(Event::PointerReleased {
                    position,
                    button: Button::Left,
                });
                assert_eq!(
                    *events.borrow(),
                    [("scratch", 0), ("scratch-up", 0), ("play", 0)]
                );
                events.borrow_mut().clear();
            }
        }

        #[test]
        fn cue_buttons_release_on_pointer_cancel_without_clicking_or_releasing_twice() {
            use slint::{
                LogicalPosition, PhysicalSize,
                platform::{PointerEventButton, WindowEvent},
            };
            use std::cell::Cell;
            crate::app::headless::init();
            let ui = CueButtons::new().unwrap();
            ui.show().unwrap();
            let window = crate::app::headless::last_window();
            window.set_size(PhysicalSize::new(320, 80));
            let counts = Rc::new(Cell::new((0, 0, 0)));
            let c = counts.clone();
            ui.on_down(move || {
                let (d, u, k) = c.get();
                c.set((d + 1, u, k));
            });
            let c = counts.clone();
            ui.on_up(move || {
                let (d, u, k) = c.get();
                c.set((d, u + 1, k));
            });
            let c = counts.clone();
            ui.on_clicked(move || {
                let (d, u, k) = c.get();
                c.set((d, u, k + 1));
            });
            for (n, x) in [40.0, 120.0, 200.0, 280.0].into_iter().enumerate() {
                let position = LogicalPosition::new(x, 40.0);
                let button = PointerEventButton::Left;
                ui.window()
                    .dispatch_event(WindowEvent::PointerPressed { position, button });
                assert_eq!(counts.get(), (2 * n + 1, 2 * n, n));
                ui.window().dispatch_event(WindowEvent::PointerExited);
                assert_eq!(counts.get(), (2 * n + 1, 2 * n + 1, n));
                ui.window()
                    .dispatch_event(WindowEvent::PointerReleased { position, button });
                assert_eq!(counts.get(), (2 * n + 1, 2 * n + 1, n));
                ui.window()
                    .dispatch_event(WindowEvent::PointerPressed { position, button });
                ui.window()
                    .dispatch_event(WindowEvent::PointerReleased { position, button });
                assert_eq!(counts.get(), (2 * n + 2, 2 * n + 2, n + 1));
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Right,
                });
                ui.window().dispatch_event(WindowEvent::PointerExited);
                assert_eq!(
                    counts.get(),
                    (2 * n + 2, 2 * n + 2, n + 1),
                    "unmatched right cancellation is not a CUE release"
                );
            }
        }
        #[test]
        fn waveform_cue_pointer_cancel_releases_the_original_press() {
            use slint::{
                LogicalPosition, PhysicalSize,
                platform::{PointerEventButton, WindowEvent},
            };
            use std::cell::Cell;
            crate::app::headless::init();
            let ui = CueWaves::new().unwrap();
            ui.show().unwrap();
            let window = crate::app::headless::last_window();
            window.set_size(PhysicalSize::new(320, 300));
            let state = ui.global::<DeckState>();
            state.set_decks(
                Rc::new(VecModel::from(vec![DeckView {
                    loaded: true,
                    ..Default::default()
                }]))
                .into(),
            );
            let counts = Rc::new(Cell::new((0, 0)));
            let c = counts.clone();
            state.on_cue(move |_| {
                let (d, u) = c.get();
                c.set((d + 1, u));
            });
            let c = counts.clone();
            state.on_cue_released(move |_| {
                let (d, u) = c.get();
                c.set((d, u + 1));
            });
            for (n, y) in [50.0, 150.0, 250.0].into_iter().enumerate() {
                let position = LogicalPosition::new(160.0, y);
                state.set_click_wave(true);
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Right,
                });
                assert_eq!(counts.get(), (2 * n + 1, 2 * n));
                ui.window().dispatch_event(WindowEvent::PointerExited);
                assert_eq!(counts.get(), (2 * n + 1, 2 * n + 1));
                ui.window().dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Right,
                });
                assert_eq!(counts.get(), (2 * n + 1, 2 * n + 1));
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Right,
                });
                state.set_click_wave(false);
                ui.window().dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Right,
                });
                assert_eq!(
                    counts.get(),
                    (2 * n + 2, 2 * n + 2),
                    "changing the click preference cannot lose a held CUE release"
                );
            }
        }

        #[test]
        fn cue_transient_and_playhead_share_pixels_in_cached_strips() {
            use crate::app::{headless, render};
            use openfader::analysis::Analysis;
            use slint::platform::WindowEvent;

            headless::repaint_changes_only();
            let (app, decks) = super::rig(1);
            let deck = &decks[0];
            deck.span_ms.set(2000);
            // A real PCM impulse at 510 ms, inside detail entry 76 (506.67..513.33 ms).
            let mut samples = vec![0.0; 2 * 44100];
            samples[2 * 22491] = 1.0;
            {
                let mut l = deck.loaded.borrow_mut();
                l.analysis = Analysis::default();
                l.cue_ms = 510;
                l.detail = render::detail::audio_wave(&samples, 44100);
            }
            let ui = StripPixels::new().unwrap();
            ui.show().unwrap();
            let window = headless::last_window();
            let state = ui.global::<DeckState>();
            let scroll = Rc::new(VecModel::from(vec![DeckScroll::default()]));
            let live = Rc::new(VecModel::from(vec![DeckLive::default()]));
            state.set_scroll(scroll.clone().into());
            state.set_live(live.clone().into());
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                app.window()
                    .dispatch_event(WindowEvent::ScaleFactorChanged {
                        scale_factor: scale,
                    });
                ui.window().dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                });
                for lw in [256.0f32, 257.0, 257.2] {
                    let lh = 44.2f32;
                    let (w, h) = ((lw * scale).round() as usize, (lh * scale).round() as usize);
                    let (sw, sh) = ((lw * scale).ceil() as usize, (lh * scale).ceil() as usize);
                    window.set_size(slint::PhysicalSize::new(sw as u32, sh as u32));
                    ui.set_view_width(lw);
                    ui.set_view_height(lh);
                    deck.detail_size.set((lw, lh));
                    let start = {
                        let mut l = deck.loaded.borrow_mut();
                        l.strip = None;
                        deck.scroll(&app, &mut l, 409.75);
                        l.strip.as_ref().unwrap().start_ms
                    };
                    let mut pixels = vec![Rgb8Pixel::default(); sw * sh];
                    for pos in [510.0, 523.1, 535.5, 508.5, 510.0] {
                        let mut l = deck.loaded.borrow_mut();
                        deck.scroll(&app, &mut l, pos);
                        let strip = l.strip.as_ref().unwrap();
                        assert_eq!(
                            strip.start_ms, start,
                            "motion must reuse the cached picture"
                        );
                        let axis = render::Axis {
                            start_ms: strip.start_ms,
                            px_per_ms: strip.px_per_ms,
                        };
                        let cue_col = axis.col(510.0) as usize;
                        let row = deck.scroll.row_data(0).unwrap();
                        let cue_x = f64::from(row.strip_x * scale) + cue_col as f64;
                        let expected =
                            (w / 2) as f64 + axis.col(510.0) as f64 - axis.col(pos) as f64;
                        assert!(
                            (cue_x - expected).abs() < 0.0001,
                            "scale={scale}, width={lw}, pos={pos}: cue={cue_x}, expected={expected}"
                        );
                        let wave = render::detail::render_strip(
                            l.detail.as_ref(),
                            &l.analysis,
                            strip.start_ms,
                            strip.px_per_ms,
                            u32::MAX,
                            None,
                            w * 3,
                            h,
                            false,
                        )
                        .to_rgba8()
                        .unwrap();
                        let peak = wave.as_slice()[h / 2 * w * 3 + cue_col];
                        let bg = render::palette().bg;
                        assert_ne!(
                            [peak.r, peak.g, peak.b],
                            bg,
                            "the PCM transient must occupy its cue column"
                        );
                        let marked = deck.live.row_data(0).unwrap().detail.to_rgba8().unwrap();
                        let cue = marked.as_slice()[h / 2 * w * 3 + cue_col];
                        assert_eq!(
                            [cue.r, cue.g, cue.b],
                            render::palette().cue,
                            "the cue and PCM transient must share the source column"
                        );
                        drop(l);
                        live.set_row_data(
                            0,
                            DeckLive {
                                detail: deck.live.row_data(0).unwrap().detail,
                                ..Default::default()
                            },
                        );
                        scroll.set_row_data(
                            0,
                            DeckScroll {
                                strip_x: row.strip_x,
                                strip_w: row.strip_w,
                                slip_x: -1.0,
                                ..Default::default()
                            },
                        );
                        ui.window().request_redraw();
                        window.draw_if_needed(|renderer| {
                            renderer.render(&mut pixels, sw);
                        });
                        // Away from the overlaid playhead, the real UI must show the cue on its exact source column.
                        if (expected - (w / 2) as f64).abs() > f64::from(scale) + 1.0 {
                            let pixel = pixels[h / 2 * sw + expected as usize];
                            assert_eq!(
                                [pixel.r, pixel.g, pixel.b],
                                render::palette().cue,
                                "visible cue source column: scale={scale}, width={lw}, pos={pos}, x={expected}"
                            );
                        }
                        if pos == 510.0 {
                            let head = ui.global::<Theme>().get_wave_playhead();
                            let pixel = pixels[h / 2 * sw + w / 2];
                            assert_eq!(
                                [pixel.r, pixel.g, pixel.b],
                                [head.red(), head.green(), head.blue()]
                            );
                        }
                    }
                    let resting = pixels.clone();
                    let resting_x = deck.scroll.row_data(0).unwrap().strip_x;
                    for n in 0..48 {
                        // Some held previews move through several frames; others end before a frame can be drawn.
                        for pos in [523.1, 535.5, 508.5] {
                            deck.scroll(&app, &mut deck.loaded.borrow_mut(), pos);
                            let row = deck.scroll.row_data(0).unwrap();
                            scroll.set_row_data(
                                0,
                                DeckScroll {
                                    strip_x: row.strip_x,
                                    strip_w: row.strip_w,
                                    slip_x: -1.0,
                                    ..Default::default()
                                },
                            );
                            if n % 3 != 0 {
                                ui.window().request_redraw();
                                window.draw_if_needed(|renderer| {
                                    renderer.render(&mut pixels, sw);
                                });
                            }
                        }
                        deck.cue_state.set(super::super::super::cue::Preview::Held);
                        deck.cue_release();
                        assert_eq!(
                            deck.scroll.row_data(0).unwrap().strip_x,
                            resting_x,
                            "release publishes the final source column without a tick"
                        );
                        assert_eq!(
                            deck.loaded.borrow().strip.as_ref().unwrap().start_ms,
                            start,
                            "a rapid return must reuse its strip"
                        );
                        let row = deck.scroll.row_data(0).unwrap();
                        scroll.set_row_data(
                            0,
                            DeckScroll {
                                strip_x: row.strip_x,
                                strip_w: row.strip_w,
                                slip_x: -1.0,
                                ..Default::default()
                            },
                        );
                        ui.window().request_redraw();
                        window.draw_if_needed(|renderer| {
                            renderer.render(&mut pixels, sw);
                        });
                        assert_eq!(
                            pixels, resting,
                            "every stopped frame must contain the resting cue pixels: burst={n}, scale={scale}, width={lw}"
                        );
                    }
                }
            }
        }
    }
}
