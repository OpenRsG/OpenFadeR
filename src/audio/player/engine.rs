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
use std::sync::atomic::Ordering::Relaxed;

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
) -> Result<Stream, cpal::BuildStreamError> {
    let (rate, ch) = (cfg.sample_rate.0, cfg.channels as usize);
    dev.build_output_stream(
        cfg,
        move |o: &mut [T], i| callback(&mut mix, rate, ch, o, i),
        |e| eprintln!("audio: {e}"),
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

fn build(out: &Output, cfg: &cpal::StreamConfig, mix: Mix) -> Result<Stream, String> {
    let stream = match out.fmt {
        cpal::SampleFormat::F32 => open::<f32>(&out.dev, cfg, mix),
        cpal::SampleFormat::I16 => open::<i16>(&out.dev, cfg, mix),
        cpal::SampleFormat::U16 => open::<u16>(&out.dev, cfg, mix),
        f => return Err(format!("unsupported sample format {f:?}")),
    };
    stream.map_err(|e| e.to_string())
}

/// A running stream on `out` rendering the channels `shared` through `bus`. A fixed buffer size the driver
/// refuses is dropped (automatic buffer) and noted in `out.status`.
fn start(
    out: &mut Output,
    shared: &[Arc<Shared>; SLOTS],
    bus: &Arc<Bus>,
) -> Result<Stream, String> {
    for s in shared {
        s.raw.store(out.plan.external, Relaxed);
    }
    let cfg = out.cfg.clone();
    let stream = match build(out, &cfg, mix_for(out, shared, bus)) {
        Err(e) if matches!(cfg.buffer_size, cpal::BufferSize::Fixed(_)) => {
            out.cfg.buffer_size = cpal::BufferSize::Default;
            out.status.notes.push(format!(
                "the driver refused the buffer size ({e}): automatic"
            ));
            out.status.buffer = None;
            build(out, &out.cfg, mix_for(out, shared, bus))?
        }
        r => r?,
    };
    stream.play().map_err(|e| e.to_string())?; // runs for the engine's lifetime; silence while paused
    Ok(stream)
}

/// The one output stream and the channels it carries.
pub(super) struct Engine {
    pub(super) shared: [Arc<Shared>; SLOTS],
    pub(super) bus: Arc<Bus>,
    /// Next unclaimed slot. ponytail: slots are never recycled; a dropped `Player` leaves its channel silent.
    pub(super) claimed: Cell<usize>,
    /// `None` only inside [`Engine::reopen`] (and after a failed one that could not restore the old device).
    stream: RefCell<Option<Stream>>,
    /// The headphone device's stream, kept alive with the main one.
    cue: RefCell<Option<cue::Cue>>,
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
        let stream = start(&mut out, shared, &bus)?;
        output::publish(out.status.clone());
        Ok(Engine {
            shared: shared.clone(),
            bus,
            claimed: Cell::new(0),
            stream: RefCell::new(Some(stream)),
            cue: RefCell::new(out.cue.take()),
        })
    }

    /// Moves the channels to the device [`choose`] now picks, keeping their state and the bus knobs; returns its name.
    /// The old stream stops first: two callbacks must never render the same channels. On an error the engine has no stream.
    fn reopen(&self) -> Result<String, String> {
        drop(self.stream.borrow_mut().take());
        drop(self.cue.borrow_mut().take());
        let mut out = choose()?;
        let stream = start(&mut out, &self.shared, &self.bus)?;
        self.bus
            .route(out.cfg.sample_rate.0, out.headphones.clone());
        output::publish(out.status.clone());
        *self.stream.borrow_mut() = Some(stream);
        *self.cue.borrow_mut() = out.cue.take();
        Ok(out.status.device)
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

/// A device lookup ended (the waker of [`output::set_waker`] runs this on the UI thread): moves the stream to the device the
/// requests so far asked for, and tells [`when_settled`]. Nothing to do when no request waits, or a newer one waits for
/// another lookup.
pub fn settle() {
    let Some(p) = PENDING.take() else { return };
    if let Some(name) = output::unresolved() {
        output::set_pending(Some(name));
        return PENDING.set(Some(p));
    }
    match finish(p) {
        Ok(dev) => eprintln!("audio: output moved to {dev}"),
        Err(e) => eprintln!("audio: output not moved: {e}"),
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
            .and_then(|mut d| d.find_map(|d| d.name().ok().filter(|n| n == "pipewire" || n == "pulse")))
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
    /// Two decks playing for `OF_XRUN_SECS` (60) on the `pipewire` PCM at `OF_XRUN_FRAMES` frames, to be watched from outside:
    /// cpal's ALSA host recovers an underrun silently, so the count is the ERR column of `pw-top` for this process.
    /// `PIPEWIRE_NODE=<null sink> OF_XRUN_FRAMES=256 cargo test --release --lib -- --ignored --nocapture bench_xruns`
    #[test]
    #[ignore = "plays for a minute into a sound server"]
    fn bench_xruns() {
        let env = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let (frames, secs) = (env("OF_XRUN_FRAMES", 0), env("OF_XRUN_SECS", 60));
        configure(Settings { device: "pipewire".into(), buffer: frames, ..Settings::default() }).unwrap();
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
        eprintln!("xrun bench: {} frames asked, buffer {:?}, device {}, pid {}", frames, st.buffer, st.device, std::process::id());
        std::thread::sleep(std::time::Duration::from_secs(secs.into()));
    }
}
