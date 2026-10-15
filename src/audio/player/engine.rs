//! The one cpal stream the `Player`s share and its lifecycle.
//!
//! The first `Player::new` opens the stream, later ones take the next free slot of the mixer ([`SLOTS`]: decks A to D).
//! The callback is in [`super::mix`]. The device, rate, buffer and layout come from [`super::output`]; [`configure`]
//! moves the running stream when the Preferences change them, [`set_output`] when a controller asks for its own
//! interface. `Player::bus` gives the shared knobs.

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

/// Asks for the output device named like `want` (a controller's name, matched without punctuation or case) or, with `None`, for the
/// original one (`OPENFADER_OUTPUT_DEVICE`, else the Preferences' device, else the system default); the stream moves there at once,
/// without a restart. Returns the device name now in use. An error leaves the previous device playing; `OPENFADER_OUTPUT_DEVICE` set
/// keeps its device whatever is asked.
pub fn set_output(want: Option<&str>) -> Result<String, String> {
    if std::env::var_os(bus::OUTPUT_ENV).is_some_and(|v| !v.is_empty()) {
        return choose().map(|o| o.status.device); // the env device is already the one playing
    }
    let previous = output::replace_wanted(want.map(str::to_owned));
    let Some(engine) = ENGINE.with_borrow(Weak::upgrade) else {
        return choose()
            .map(|o| o.status.device)
            .inspect_err(|_| _ = output::replace_wanted(previous)); // no stream yet: the first `Player` opens it on this device
    };
    engine.reopen().or_else(|e| {
        output::set_wanted(previous);
        match engine.reopen() {
            Ok(was) => Err(format!("{e}; staying on {was}")),
            Err(again) => Err(format!("{e}; and the previous device failed too: {again}")),
        }
    })
}

/// Applies the Preferences' audio settings ([`Settings`]): the live knobs at once ([`Bus::apply`]), the device, rate, buffer
/// and channel layout by moving the running stream (decks keep their state; there is a short gap). With no stream yet the first
/// `Player` opens it with these. An error leaves the previous stream playing (what failed is in `status().error`).
pub fn configure(s: Settings) -> Result<(), String> {
    let old = output::replace_settings(s.clone());
    let Some(engine) = ENGINE.with_borrow(Weak::upgrade) else {
        return Ok(());
    };
    engine.bus.apply(&s);
    if s.same_stream(&old) {
        return Ok(());
    }
    engine.reopen().map(drop).or_else(|e| {
        output::replace_settings(old);
        let back = engine.reopen();
        output::note_error(e.clone());
        match back {
            Ok(was) => Err(format!("{e}; staying on {was}")),
            Err(again) => Err(format!("{e}; and the previous output failed too: {again}")),
        }
    })
}

/// The bus of the running stream (`None` before the first `Player` opened it): the knobs, the preview player
/// (`Bus::preview`) and the recorder tap, for code that has no `Player` at hand.
pub fn live_bus() -> Option<Arc<Bus>> {
    ENGINE.with_borrow(Weak::upgrade).map(|e| e.bus.clone())
}
