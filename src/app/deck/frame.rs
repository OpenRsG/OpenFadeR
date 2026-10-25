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
                self.set_live(|r| r.detail = image);
                l.strip = Some(Strip {
                    key,
                    start_ms,
                    span,
                    px_per_ms,
                });
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
        self.scroll(&app, &mut l, pos);
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

    fn rig(count: usize) -> (App, Decks) {
        use openfader::{analysis::{Analysis, Beat, Cue}, library::{Library, own::Own}, store::Store};
        use crate::app::deck::Loaded;
        crate::app::headless::init();
        let app = App::new().unwrap();
        let lib: Rc<dyn Library> = Rc::new(Own::new(std::sync::Arc::new(Store::in_memory().unwrap())));
        let decks = crate::app::deck::wire(&app, lib, Default::default());
        let a = Analysis {
            beats: (0..360).map(|i| Beat { time_ms: i * 500, bar_pos: (i % 4 + 1) as u16, bpm_x100: 12000 }).collect(),
            cues: vec![Cue { time_ms: 24000, hot_cue: 1, loop_end_ms: None, color: None, comment: None }],
            three_band_detail: (0..27000).map(|i| [(i % 128) as u8, ((i * 7) % 128) as u8, ((i * 13) % 128) as u8]).collect(),
            three_band_preview: (0..400).map(|i| [(i % 128) as u8, ((i * 7) % 128) as u8, ((i * 13) % 128) as u8]).collect(),
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
        use slint::{PhysicalSize, Rgb8Pixel};
        use openfader::audio::{Pcm, Track16};
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
                let keys = || decks[..count].iter().map(|d| { let l = d.loaded.borrow(); (l.strip.as_ref().map(|s| s.start_ms.to_bits()), l.overview_key) }).collect::<Vec<_>>();
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
