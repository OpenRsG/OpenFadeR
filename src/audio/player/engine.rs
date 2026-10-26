//! The one cpal stream the `Player`s share and its lifecycle.
//!
//! The first `Player::new` opens the stream, later ones take the next free slot of the mixer ([`SLOTS`]: decks A to D).
//! The callback is in [`super::mix`]. The device, rate, buffer and layout come from [`super::output`]; [`configure`]
//! moves the running stream when the Preferences change them, [`set_output`] when a controller asks for its own
//! interface. A device is found by name on a worker thread ([`output::set_waker`]: listing ALSA devices takes up to a second):
//! the stream plays on where it is, [`settle`] moves it when the device is found and [`when_settled`] tells the caller.
//! `Player::bus` gives the shared knobs.

use super::super::bus::{self, Bus};
use super::super::cue;
use super::super::route::{self, EXTERNAL, Settings};
use super::Shared;
use super::mix::{Mix, callback};
use super::output::{self, Output, choose};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SizedSample, Stream};
use std::cell::{Cell, RefCell};
use std::rc::Weak;
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU64,
    Ordering::{AcqRel, Relaxed},
};
use std::time::{Duration, Instant};

const RUNNING: u8 = 0;
const DEVICE_GONE: u8 = 1;
const BACKEND_ERROR: u8 = 2;
const STALLED: u8 = 3;
const RECOVERING: u8 = 4;
const RETRY: Duration = Duration::from_secs(1);
const STALL: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Health {
    state: AtomicU8,
    callbacks: AtomicU64,
    generation: AtomicU64,
    posted: AtomicBool,
    cue_failed: parking_lot::Mutex<Option<Arc<AtomicBool>>>,
}

impl Health {
    fn arm(&self) {
        self.generation.fetch_add(1, Relaxed);
        self.state.store(RUNNING, Relaxed);
    }

    fn fail(&self, reason: u8) {
        let _ = self
            .state
            .compare_exchange(RUNNING, reason, AcqRel, Relaxed);
    }

    fn failed(&self) -> bool {
        self.state.load(Relaxed) != RUNNING
    }
}

struct Watchdog {
    callbacks: u64,
    generation: u64,
    last_callback: Instant,
    notified: Option<Instant>,
    running: bool,
}

impl Watchdog {
    fn new(now: Instant) -> Self {
        Self {
            callbacks: 0,
            generation: 0,
            last_callback: now,
            notified: None,
            running: false,
        }
    }

    fn poll(&mut self, h: &Health, now: Instant) -> bool {
        if h.cue_failed
            .lock()
            .as_ref()
            .is_some_and(|failed| failed.load(Relaxed))
        {
            h.fail(BACKEND_ERROR);
        }
        let callbacks = h.callbacks.load(Relaxed);
        let running = !h.failed();
        let generation = h.generation.load(Relaxed);
        if callbacks != self.callbacks
            || generation != self.generation
            || (running && !self.running)
        {
            self.last_callback = now;
            self.callbacks = callbacks;
            self.generation = generation;
        }
        self.running = running;
        if running && now.duration_since(self.last_callback) >= STALL {
            h.fail(STALLED);
        }
        if !h.failed() {
            self.notified = None;
            return false;
        }
        if self
            .notified
            .is_none_or(|at| now.duration_since(at) >= RETRY)
            && !h.posted.swap(true, AcqRel)
        {
            self.notified = Some(now);
            return true;
        }
        false
    }
}

fn watch(health: &Arc<Health>) {
    let health = Arc::downgrade(health);
    std::thread::spawn(move || {
        let mut watchdog = Watchdog::new(Instant::now());
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let Some(health) = health.upgrade() else {
                break;
            };
            if watchdog.poll(&health, Instant::now()) && !output::wake() {
                health.posted.store(false, Relaxed);
            }
        }
    });
}

/// Mixer channels one stream carries.
pub(super) const SLOTS: usize = bus::CHANNELS;
const _: () = assert!(
    EXTERNAL == SLOTS + 2,
    "External outputs = the decks, the sampler and the preview"
);

fn open<T: SizedSample + FromSample<f32> + Send + 'static>(
    dev: &cpal::Device,
    cfg: &cpal::StreamConfig,
    mut mix: Mix,
    health: &Arc<Health>,
) -> Result<Stream, cpal::BuildStreamError> {
    let (rate, ch) = (cfg.sample_rate.0, cfg.channels as usize);
    let (data, error) = (health.clone(), health.clone());
    dev.build_output_stream(
        cfg,
        move |o: &mut [T], i| {
            if data.failed() {
                o.fill(T::from_sample(0.0));
                return;
            }
            data.callbacks.fetch_add(1, Relaxed);
            callback(&mut mix, rate, ch, o, i);
        },
        move |e| {
            error.fail(match e {
                cpal::StreamError::DeviceNotAvailable => DEVICE_GONE,
                _ => BACKEND_ERROR,
            })
        },
        None,
    )
}

/// The audio thread's state for a stream on `out`.
fn mix_for(out: &Output, shared: &[Arc<Shared>; SLOTS], bus: &Arc<Bus>) -> Mix {
    let cue = out.cue.as_ref().map(|c| c.ring.clone());
    Mix::new(
        out.cfg.sample_rate.0,
        out.cfg.channels as usize,
        out.plan.clone(),
        cue,
        shared,
        bus,
    )
}

fn build(
    out: &Output,
    cfg: &cpal::StreamConfig,
    mix: Mix,
    health: &Arc<Health>,
) -> Result<Stream, String> {
    let stream = match out.fmt {
        cpal::SampleFormat::F32 => open::<f32>(&out.dev, cfg, mix, health),
        cpal::SampleFormat::I16 => open::<i16>(&out.dev, cfg, mix, health),
        cpal::SampleFormat::U16 => open::<u16>(&out.dev, cfg, mix, health),
        f => return Err(format!("unsupported sample format {f:?}")),
    };
    stream.map_err(|e| e.to_string())
}

/// A running stream on `out` rendering the channels `shared` through `bus`. A fixed buffer size the driver
/// refuses is dropped (driver default buffer) and noted in `out.status`.
fn start(
    out: &mut Output,
    shared: &[Arc<Shared>; SLOTS],
    bus: &Arc<Bus>,
    health: &Arc<Health>,
) -> Result<Stream, String> {
    for s in shared {
        s.raw.store(out.plan.external, Relaxed);
    }
    *health.cue_failed.lock() = out.cue.as_ref().map(|cue| cue.failed.clone());
    health.arm();
    let cfg = out.cfg.clone();
    let stream = match build(out, &cfg, mix_for(out, shared, bus), health) {
        Err(e) if matches!(cfg.buffer_size, cpal::BufferSize::Fixed(_)) => {
            out.cfg.buffer_size = cpal::BufferSize::Default;
            out.status.notes.push(format!(
                "the driver refused the buffer size ({e}): driver default"
            ));
            out.status.buffer = None;
            health.arm();
            build(out, &out.cfg, mix_for(out, shared, bus), health)
        }
        r => r,
    }
    .inspect_err(|_| health.state.store(RECOVERING, Relaxed))?;
    if let Err(e) = stream.play() {
        health.state.store(RECOVERING, Relaxed);
        return Err(e.to_string());
    }
    Ok(stream)
}

/// The one output stream and the channels it carries.
pub(super) struct Engine {
    pub(super) shared: [Arc<Shared>; SLOTS],
    pub(super) bus: Arc<Bus>,
    /// Next unclaimed slot. ponytail: slots are never recycled; a dropped `Player` leaves its channel silent.
    pub(super) claimed: Cell<usize>,
    /// Absent while an output is unavailable; the same channel state survives every retry.
    stream: RefCell<Option<Stream>>,
    /// The headphone device's stream, kept alive with the main one.
    cue: RefCell<Option<cue::Cue>>,
    health: Arc<Health>,
    recovery: RefCell<Option<Recovery>>,
}

struct Recovery {
    name: String,
    source: &'static str,
    retry_at: Instant,
}

thread_local! {
    /// The stream every `Player` of this (UI) thread shares; gone when the last `Player` is dropped.
    pub(super) static ENGINE: RefCell<Weak<Engine>> = RefCell::new(Weak::new());
}

impl Engine {
    /// Opens the stream the settings ask for; when that fails and the settings are not the defaults, the defaults
    /// play instead and the status says what failed (a saved device that no longer works must not silence the app).
    pub(super) fn open() -> Result<Engine, String> {
        let shared = [(); SLOTS].map(|_| Arc::new(Shared::new()));
        match Engine::build(&shared) {
            Ok(e) => Ok(e),
            Err(first) if !output::settings().same_stream(&Settings::default()) => {
                let keep = output::settings();
                output::replace_settings(Settings {
                    device: String::new(),
                    rate: 0,
                    buffer: 0,
                    mixer: route::Mixer::Internal,
                    master: 1,
                    phones: 0,
                    booth: 0,
                    external: Settings::default().external,
                    ..keep
                });
                let e = Engine::build(&shared).map_err(|again| {
                    format!("{first}; and the default output failed too: {again}")
                })?;
                output::note_error(first);
                Ok(e)
            }
            Err(e) => Err(e),
        }
    }

    fn build(shared: &[Arc<Shared>; SLOTS]) -> Result<Engine, String> {
        let mut out = choose()?;
        if out.status.pending.is_some() {
            PENDING.with_borrow_mut(|p| _ = p.get_or_insert_with(Pending::default)); // `settle` moves the stream when it is found
        }
        let bus = Arc::new(Bus::new(out.cfg.sample_rate.0, out.headphones.clone()));
        bus.apply(&output::settings());
        let health = Arc::new(Health::default());
        let stream = start(&mut out, shared, &bus, &health)?;
        watch(&health);
        output::publish(out.status.clone());
        Ok(Engine {
            shared: shared.clone(),
            bus,
            claimed: Cell::new(0),
            stream: RefCell::new(Some(stream)),
            cue: RefCell::new(out.cue.take()),
            health,
            recovery: RefCell::new(None),
        })
    }

    /// Moves the channels to the device [`choose`] now picks, keeping their state and the bus knobs; returns its name.
    /// The old stream stops first: two callbacks must never render the same channels. On an error the engine has no stream.
    fn reopen(&self) -> Result<String, String> {
        self.stop();
        self.install(choose()?)
    }

    fn stop(&self) {
        self.health.state.store(RECOVERING, Relaxed);
        drop(self.stream.borrow_mut().take());
        drop(self.cue.borrow_mut().take());
        for shared in &self.shared {
            shared
                .clock
                .lock()
                .freeze(f64::from_bits(shared.pos.load(Relaxed)));
        }
    }

    fn install(&self, mut out: Output) -> Result<String, String> {
        let stream = start(&mut out, &self.shared, &self.bus, &self.health)?;
        self.bus
            .route(out.cfg.sample_rate.0, out.headphones.clone());
        output::publish(out.status.clone());
        *self.stream.borrow_mut() = Some(stream);
        *self.cue.borrow_mut() = out.cue.take();
        self.recovery.borrow_mut().take();
        Ok(out.status.device)
    }

    pub(super) fn available(&self) -> bool {
        !self.health.failed()
    }

    fn recover(&self, now: Instant) {
        self.health.posted.store(false, Relaxed);
        if self.available() {
            return;
        }
        let mut recovery = self.recovery.borrow_mut().take().unwrap_or_else(|| {
            let status = output::status();
            let reason = match self.health.state.load(Relaxed) {
                DEVICE_GONE => "device disconnected",
                STALLED => "no audio callbacks for two seconds",
                _ => "audio backend stopped",
            };
            self.stop();
            output::note_error(format!("{}: {reason}; reconnecting", status.device));
            output::forget_recovery(&status.device);
            Recovery {
                name: status.device,
                source: status.source,
                retry_at: now,
            }
        });
        if now < recovery.retry_at {
            self.recovery.replace(Some(recovery));
            return;
        }
        let Some(found) = output::recover(&recovery.name, recovery.source) else {
            self.recovery.replace(Some(recovery));
            return;
        };
        match found.and_then(|out| self.install(out)) {
            Ok(_) => {}
            Err(e) => {
                self.health.state.store(RECOVERING, Relaxed);
                output::note_error(format!("{}: {e}; reconnecting", recovery.name));
                output::forget_recovery(&recovery.name);
                recovery.retry_at = now + RETRY;
                self.recovery.replace(Some(recovery));
            }
        }
    }
}

/// How a request to move the stream ended for now.
#[derive(Debug, PartialEq)]
pub enum Switch {
    /// The stream plays on this device now.
    Done(String),
    /// The device is being looked up on a worker thread (ALSA opens every PCM to list them: far too long for the UI thread).
    /// The stream plays on where it was and moves when the lookup ends ([`settle`]); [`when_settled`] tells the caller then.
    Later,
}

/// A request that waits for its device lookup.
#[derive(Default)]
struct Pending {
    /// Puts the request state back to that of the stream that plays now: the first request's, later ones build on it.
    undo: Option<Box<dyn FnOnce()>>,
    /// A failure is also noted in the status (a Preferences change; a controller's request only reports it).
    note: bool,
    waiters: Vec<Box<dyn FnOnce(Result<String, String>)>>,
}

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

/// Moves the stream to the device [`choose`] now picks (or, for no stream yet, just checks it), puts the request back with
/// `p.undo` when that fails, and tells the waiters.
fn finish(p: Pending) -> Result<String, String> {
    output::set_pending(None);
    let undo = || p.undo.into_iter().for_each(|u| u());
    let r = match ENGINE.with_borrow(Weak::upgrade) {
        None => choose().map(|o| o.status.device).inspect_err(|_| undo()),
        Some(engine) => engine.reopen().or_else(|e| {
            undo();
            let back = engine.reopen();
            if p.note {
                output::note_error(e.clone());
            }
            match back {
                Ok(was) => Err(format!("{e}; staying on {was}")),
                Err(again) => Err(format!("{e}; and the previous output failed too: {again}")),
            }
        }),
    };
    p.waiters.into_iter().for_each(|w| w(r.clone()));
    r
}

/// Moves the stream to what the request state now asks, at once when the device is known, else once a worker has found it.
fn request(undo: impl FnOnce() + 'static, note: bool) -> Result<Switch, String> {
    let mut p = PENDING.take().unwrap_or_default();
    if p.undo.is_none() {
        (p.undo, p.note) = (Some(Box::new(undo) as Box<dyn FnOnce()>), note);
    }
    if let Some(name) = output::unresolved() {
        output::set_pending(Some(name));
        PENDING.set(Some(p));
        return Ok(Switch::Later);
    }
    finish(p).map(Switch::Done)
}

/// Handles completed device lookups and coalesced stream-health notifications on the stream-owning UI thread.
pub fn settle() {
    if let Some(p) = PENDING.take() {
        if let Some(name) = output::unresolved() {
            output::set_pending(Some(name));
            PENDING.set(Some(p));
        } else {
            match finish(p) {
                Ok(dev) => eprintln!("audio: output moved to {dev}"),
                Err(e) => eprintln!("audio: output not moved: {e}"),
            }
        }
    }
    if let Some(engine) = ENGINE.with_borrow(Weak::upgrade) {
        engine.recover(Instant::now());
    }
}

/// Calls `done` with the outcome (the device now in use, or why not) once the device [`Switch::Later`] / [`configure`] waits
/// for is there and the stream has moved. Does nothing when no request is waiting.
pub fn when_settled(done: impl FnOnce(Result<String, String>) + 'static) {
    PENDING.with_borrow_mut(|p| {
        if let Some(p) = p {
            p.waiters.push(Box::new(done));
        }
    });
}

/// Asks for the output device named like `want` (a controller's name, matched without punctuation or case) or, with `None`, for the
/// original one (`OPENFADER_OUTPUT_DEVICE`, else the Preferences' device, else the system default). The stream moves there at
/// once, without a restart, when the device is known; else it plays on while a worker looks for it ([`Switch::Later`]). An error
/// leaves the previous device playing; `OPENFADER_OUTPUT_DEVICE` set keeps its device whatever is asked.
pub fn set_output(want: Option<&str>) -> Result<Switch, String> {
    if std::env::var_os(bus::OUTPUT_ENV).is_some_and(|v| !v.is_empty()) {
        return choose().map(|o| Switch::Done(o.status.device)); // the env device is already the one playing
    }
    let previous = output::replace_wanted(want.map(str::to_owned));
    request(move || output::set_wanted(previous), false)
}

/// Applies the Preferences' audio settings ([`Settings`]): the live knobs at once ([`Bus::apply`]), the device, rate, buffer
/// and channel layout by moving the running stream (decks keep their state; there is a short gap; a device not looked up yet
/// is waited for first, [`when_settled`]). With no stream yet the first `Player` opens it with these. An error leaves the
/// previous stream playing (what failed is in `status().error`).
pub fn configure(s: Settings) -> Result<(), String> {
    let old = output::replace_settings(s.clone());
    let Some(engine) = ENGINE.with_borrow(Weak::upgrade) else {
        let _ = output::unresolved(); // starts the lookup while the window is built
        return Ok(());
    };
    engine.bus.apply(&s);
    if s.same_stream(&old) {
        return Ok(());
    }
    request(move || _ = output::replace_settings(old), true).map(drop)
}

/// The bus of the running stream (`None` before the first `Player` opened it): the knobs, the preview player
/// (`Bus::preview`) and the recorder tap, for code that has no `Player` at hand.
pub fn live_bus() -> Option<Arc<Bus>> {
    ENGINE.with_borrow(Weak::upgrade).map(|e| e.bus.clone())
}

#[cfg(test)]
mod tests {
    use super::super::super::route::Settings;
    use super::super::{status, Player};
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    static LOOKED_UP: AtomicBool = AtomicBool::new(false);

    #[test]
    fn an_error_storm_posts_one_wake_and_retries_at_most_once_a_second() {
        let (h, t0) = (Health::default(), Instant::now());
        let mut watchdog = Watchdog::new(t0);
        h.fail(DEVICE_GONE);
        for _ in 0..10_000 {
            h.fail(BACKEND_ERROR);
        }
        assert_eq!(h.state.load(Relaxed), DEVICE_GONE);
        assert!(watchdog.poll(&h, t0));
        assert!(
            !watchdog.poll(&h, t0 + RETRY * 2),
            "one notification can remain outstanding"
        );
        h.posted.store(false, Relaxed);
        assert!(watchdog.poll(&h, t0 + RETRY * 2));
        h.posted.store(false, Relaxed);
        assert!(!watchdog.poll(&h, t0 + RETRY * 2 + RETRY / 2));
        assert!(watchdog.poll(&h, t0 + RETRY * 3));
    }

    #[test]
    fn missing_callbacks_trip_the_watchdog_but_a_new_stream_gets_a_fresh_deadline() {
        let (h, t0) = (Health::default(), Instant::now());
        let mut watchdog = Watchdog::new(t0);
        h.arm();
        assert!(!watchdog.poll(&h, t0));
        h.callbacks.fetch_add(1, Relaxed);
        assert!(!watchdog.poll(&h, t0 + STALL));
        assert!(!watchdog.poll(&h, t0 + STALL * 2 - Duration::from_millis(1)));
        assert!(watchdog.poll(&h, t0 + STALL * 2));
        assert_eq!(h.state.load(Relaxed), STALLED);
        h.posted.store(false, Relaxed);
        // A reconnect can finish entirely between two watchdog turns.
        h.state.store(RECOVERING, Relaxed);
        h.arm();
        assert!(!watchdog.poll(&h, t0 + STALL * 10));
        assert!(!h.failed());
        assert!(watchdog.poll(&h, t0 + STALL * 11));
    }

    #[test]
    fn a_headphone_stream_fault_reconnects_through_the_same_latch() {
        let (h, t0) = (Health::default(), Instant::now());
        let failed = Arc::new(AtomicBool::new(false));
        *h.cue_failed.lock() = Some(failed.clone());
        let mut watchdog = Watchdog::new(t0);
        assert!(!watchdog.poll(&h, t0));
        failed.store(true, Relaxed);
        assert!(watchdog.poll(&h, t0 + Duration::from_millis(100)));
        assert_eq!(h.state.load(Relaxed), BACKEND_ERROR);
        *h.cue_failed.lock() = None;
        h.arm();
        assert!(!watchdog.poll(&h, t0 + RETRY * 5));
    }

    #[test]
    fn stopping_a_failed_stream_keeps_the_loaded_audio_and_pending_transport_commands() {
        use super::super::{Anchor, Pcm, Track16, Voice, heard_frames, pack, render};
        let shared = [(); SLOTS].map(|_| Arc::new(Shared::new()));
        let track = Arc::new(Track16::from(Pcm {
            rate: 44100,
            samples: vec![0.25; 44100 * 2],
        }));
        *shared[0].pcm.lock() = track.clone();
        shared[0].playing.store(true, Relaxed);
        shared[0].rate.store(1.08f64.to_bits(), Relaxed);
        let lp = pack(Some((100.0, 500.0)));
        shared[0].lp.store(lp, Relaxed);
        shared[0].pos.store(8000f64.to_bits(), Relaxed);
        shared[0].seek_to(12000.0);
        shared[0].clock.lock().push(
            Anchor {
                at: Instant::now() - STALL,
                pos: 7500.0,
                latency: Duration::from_millis(40),
                fps: 44100.0,
                lp: None,
            },
            true,
            true,
        );
        let engine = Engine {
            shared,
            bus: Arc::new(Bus::new(44100, bus::Headphones::Split)),
            claimed: Cell::new(1),
            stream: RefCell::new(None),
            cue: RefCell::new(None),
            health: Arc::new(Health::default()),
            recovery: RefCell::new(None),
        };
        let engine = std::rc::Rc::new(engine);
        let mut player = Player {
            shared: engine.shared[0].clone(),
            engine: engine.clone(),
            src_rate: 44100,
            frames: 44100,
            has_stems: false,
        };
        engine.health.fail(BACKEND_ERROR);
        let pending = player.shared.seek.swap(super::super::NO_SEEK, Relaxed);
        assert_eq!(player.position_ms(), 8000.0 * 1000.0 / 44100.0);
        assert!(
            !player.settled(),
            "a stale hardware anchor is not audible output"
        );
        player.shared.seek.store(pending, Relaxed);
        player.pause();
        assert!(!player.is_playing());
        player.play();
        assert!(player.is_playing());
        engine.stop();
        let sh = &engine.shared[0];
        assert!(Arc::ptr_eq(&track, &sh.pcm.lock()));
        assert_eq!(sh.seek.load(Relaxed), 12000f64.to_bits());
        assert!(sh.playing.load(Relaxed));
        assert_eq!(sh.lp.load(Relaxed), lp);
        assert_eq!(sh.rate.load(Relaxed), 1.08f64.to_bits());
        assert_eq!(
            heard_frames(&sh.clock.lock(), Instant::now() + STALL * 10, 44100.0),
            8000.0
        );
        engine.health.arm();
        let mut voice = Voice::new(44100);
        let mut peak = 0.0f32;
        let rendered = render(sh, &mut voice, 44100, 256, |_, m, _| {
            peak = peak.max(m[0].abs())
        });
        assert!(rendered.seeked);
        assert!(
            peak > 0.0,
            "the replacement stream can render the retained track"
        );
        assert!(f64::from_bits(sh.pos.load(Relaxed)) > 12000.0);
    }

    /// Plays the UI thread: waits for the worker's waker, then runs what the app posts (`settle`).
    fn settled() {
        let t = Instant::now();
        while !LOOKED_UP.swap(false, Ordering::AcqRel) {
            assert!(t.elapsed() < Duration::from_secs(20), "the lookup never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        settle();
    }

    /// Listing ALSA devices takes up to a second: the Preferences' device is looked up on a worker thread, the stream plays on the
    /// default meanwhile and moves when the device is found; a device that is not there leaves the default with a note.
    #[test]
    fn a_preferences_device_is_looked_up_off_the_calling_thread() {
        let Some(name) = cpal::default_host()
            .output_devices()
            .ok()
            .and_then(|mut d| {
                d.find_map(|d| d.name().ok().filter(|n| n == "pipewire" || n == "pulse"))
            })
        else {
            return; // no sound server: no device to find that is safe to open
        };
        output::set_waker(|| LOOKED_UP.store(true, Ordering::Release));
        let Ok(_player) = Player::new() else { return };
        let (before, name_) = (status().device, name.clone());
        let device = |d: &str| Settings { device: d.into(), ..Settings::default() };

        let t = Instant::now();
        configure(device(&name)).unwrap();
        let took = t.elapsed();
        eprintln!("configure returned after {took:?}");
        assert!(took < Duration::from_millis(5), "the caller waited {took:?}");
        assert_eq!((status().device, status().pending), (before, Some(name_)));
        settled();
        let st = status();
        assert_eq!((st.device.as_str(), st.source, st.pending), (name.as_str(), "settings", None));

        configure(device("no such pcm")).unwrap();
        settled();
        let st = status();
        assert_eq!((st.source, st.pending), ("system default", None));
        assert!(st.notes.iter().any(|n| n.contains("no such pcm")), "{:?}", st.notes);
    }
    /// Two decks on a private PipeWire server; inspect this process's ERR column with `pw-top` on that same server.
    /// Requires `OPENFADER_OUTPUT_DEVICE=pipewire`, a non-default `PIPEWIRE_REMOTE` and a separate `PIPEWIRE_RUNTIME_DIR`.
    #[test]
    #[ignore = "requires a private PipeWire daemon with only a null sink"]
    fn bench_xruns() {
        let remote = std::env::var("PIPEWIRE_REMOTE")
            .expect("use a private PipeWire daemon, never the desktop server");
        assert!(
            !remote.is_empty() && remote != "pipewire-0",
            "the desktop PipeWire server is not a test sink"
        );
        let runtime = std::env::var_os("PIPEWIRE_RUNTIME_DIR")
            .expect("set a private PipeWire runtime directory");
        assert_ne!(
            Some(runtime),
            std::env::var_os("XDG_RUNTIME_DIR"),
            "do not use the desktop audio runtime"
        );
        assert_eq!(
            std::env::var(bus::OUTPUT_ENV).as_deref(),
            Ok("pipewire"),
            "pin the output before opening any stream"
        );
        let env = |k: &str, d: u32| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let (frames, secs) = (env("OF_XRUN_FRAMES", 0), env("OF_XRUN_SECS", 60));
        configure(Settings {
            device: "pipewire".into(),
            buffer: frames,
            driver_buffer: std::env::var_os("OF_XRUN_DRIVER").is_some(),
            ..Settings::default()
        })
        .unwrap();
        let mut decks = [Player::new().unwrap(), Player::new().unwrap()];
        for (k, d) in decks.iter_mut().enumerate() {
            let hz = 220.0 * (k + 1) as f32;
            let samples = (0..44100 * (secs as usize + 10))
                .flat_map(|i| {
                    let v = 0.2 * (i as f32 * hz * std::f32::consts::TAU / 44100.0).sin();
                    [v, v]
                })
                .collect();
            d.load(crate::audio::Pcm { rate: 44100, samples });
            d.play();
        }
        let st = status();
        eprintln!(
            "xrun bench: {} frames asked, buffer {:?}, device {}, pid {}",
            frames,
            st.buffer,
            st.device,
            std::process::id()
        );
        let mut latencies = Vec::with_capacity(secs as usize);
        for _ in 0..secs {
            std::thread::sleep(std::time::Duration::from_secs(1));
            latencies.push(decks[0].output_latency_ms());
        }
        latencies.sort_by(f64::total_cmp);
        eprintln!(
            "xrun bench: ALSA callback-to-playback delay min/median/max {:.3}/{:.3}/{:.3} ms",
            latencies[0],
            latencies[latencies.len() / 2],
            latencies[latencies.len() - 1]
        );
        eprintln!(
            "xrun bench: callback {} frames, positions {} {} ms",
            decks[0].bus().buffer_frames(),
            decks[0].position_ms(),
            decks[1].position_ms()
        );
        assert!(
            decks.iter().all(|d| d.position_ms() > secs as f64 * 800.0),
            "both decks must really play for the measurement"
        );
    }
}
