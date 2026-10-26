//! Which output the next stream gets: device, sample rate, buffer size and channel layout.
//!
//! Device precedence (first that applies): `OPENFADER_OUTPUT_DEVICE` (environment, always wins) > the connected
//! controller's own audio interface (`set_output(Some(name))` on connect, PREFS "Use controller audio when connected";
//! a device picked in Preferences afterwards ends it, [`release_controller`]) > the
//! device chosen in Preferences > Audio ([`Settings::device`]) > the system default. A chosen device that is not
//! there falls back to the system default and says so in [`Status::notes`]. A sample rate or buffer size the device
//! does not support falls back to its default the same way. Headphones: `OPENFADER_CUE_DEVICE` / `OPENFADER_HP_CHANNELS`
//! > the Preferences headphone pair > a controller interface with four outputs / a card listed in `audio-cards.json` > SPLIT on the master pair.
//! The stream is opened with exactly the channels the layout needs ([`Plan::channels`]).
//!
//! A device is found by name, and finding it is slow (cpal's ALSA host opens every PCM to list them: 0.1 to 0.7 s), so with
//! a waker set ([`set_waker`]) the lookup runs on a worker thread and the answer is cached: [`choose`] plays the system
//! default meanwhile and [`Status::pending`] names the device being looked for; the engine moves the stream when the worker
//! calls the waker (`super::engine::settle`). Without a waker (tests, tools) the lookup blocks the caller.

use super::super::bus::{self, Headphones};
use super::super::route::{self, Mixer, Plan, Settings};
use super::super::{cue, device, find_output};
use cpal::traits::DeviceTrait;
use cpal::{BufferSize, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfig};
use parking_lot::Mutex;
use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// What the last stream open decided, for the Preferences page (see [`status`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// Device in use.
    pub device: String,
    /// Where the choice came from: "OPENFADER_OUTPUT_DEVICE", "controller", "settings" or "system default".
    pub source: &'static str,
    pub rate: u32,
    pub channels: usize,
    /// The buffer size requested from the driver (`None` = the driver's own).
    pub buffer: Option<u32>,
    /// Why a setting was not honoured (missing device, unsupported rate, buffer outside the range, outputs the device lacks).
    pub notes: Vec<String>,
    /// The asked layout or device could not be had (the previous or default one plays instead).
    pub error: Option<String>,
    /// The device being looked for on a worker thread: the stream keeps playing where it is and moves there when found.
    pub pending: Option<String>,
}

/// What [`choose`] decided: the device, its stream config, the channel layout, and the second headphone device if one was asked for.
pub(super) struct Output {
    pub(super) dev: cpal::Device,
    pub(super) cfg: StreamConfig,
    pub(super) fmt: SampleFormat,
    pub(super) plan: Plan,
    pub(super) cue: Option<cue::Cue>,
    pub(super) headphones: Headphones,
    pub(super) status: Status,
}

thread_local! {
    /// The output device the app asked for at run time ([`super::set_output`]: the connected controller's audio interface); the env var wins.
    static WANTED: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The Preferences' audio settings the next stream is opened with ([`super::configure`]).
    static SETTINGS: RefCell<Settings> = RefCell::new(Settings::default());
    static STATUS: RefCell<Status> = RefCell::new(Status::default());
}

/// The settings the next stream is opened with.
pub(super) fn settings() -> Settings {
    SETTINGS.with_borrow(Clone::clone)
}

/// Swaps in new settings; returns the previous ones.
pub(super) fn replace_settings(s: Settings) -> Settings {
    SETTINGS.replace(s)
}

/// The controller's interface asked for with `set_output`, if any.
pub(super) fn replace_wanted(w: Option<String>) -> Option<String> {
    WANTED.replace(w)
}

pub(super) fn set_wanted(w: Option<String>) {
    WANTED.set(w);
}

/// The controller whose audio interface the app was asked to use (`set_output(Some(name))`, `None` = none connected / the
/// switch is off), whether or not that device was found.
pub fn controller() -> Option<String> {
    WANTED.with_borrow(Clone::clone)
}

/// A device was picked in Preferences while a controller's interface played: the pick wins until the next connect. The
/// stream moves on the next [`super::configure`] / [`super::set_output`].
pub fn release_controller() {
    WANTED.set(None);
}

/// What the last stream open decided (empty before the first one).
pub fn status() -> Status {
    STATUS.with_borrow(Clone::clone)
}

/// Shows (or clears) the device being looked for in the status of the stream that plays meanwhile.
pub(super) fn set_pending(name: Option<String>) {
    STATUS.with_borrow_mut(|s| s.pending = name);
}

pub(super) fn publish(s: Status) {
    STATUS.set(s);
}

/// Records a failure of the asked settings next to the status of the stream that plays instead.
pub(super) fn note_error(e: String) {
    STATUS.with_borrow_mut(|s| s.error = Some(e));
}

/// Names compare without punctuation and case, so a controller's name ("Model-1") finds its ALSA card ("front:CARD=Model1,DEV=0").
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn name_matches(device: &str, want: &str) -> bool {
    let w = norm(want);
    !w.is_empty() && norm(device).contains(&w)
}

/// The device the app asked for: of the ones named like `want`, the first with a 4-channel config (master 1/2, headphones 3/4), else the first.
fn find_wanted(want: &str) -> Result<(String, cpal::Device), String> {
    use cpal::traits::HostTrait;
    let devs: Vec<(String, cpal::Device)> = cpal::default_host()
        .output_devices()
        .map_err(|e| format!("cannot list output devices: {e}"))?
        .filter_map(|d| Some((d.name().ok()?, d)))
        .filter(|(n, _)| name_matches(n, want))
        .collect();
    let pick = devs
        .iter()
        .position(|(n, d)| config_for(d, n, 4, None).is_ok())
        .unwrap_or(0);
    devs.into_iter()
        .nth(pick)
        .ok_or_else(|| format!("no output device named like {want:?}"))
}

/// A device to open by name; the first of these that applies is the one asked for (the module doc's precedence).
#[derive(Clone, Debug, PartialEq)]
enum Lookup {
    /// `OPENFADER_OUTPUT_DEVICE`: a part of the name ([`find_output`]).
    Env(String),
    /// The connected controller's interface ([`find_wanted`]).
    Controller(String),
    /// The Preferences' device: the exact cpal name.
    Settings(String),
    /// Reconnect the actual output without changing the saved preference or controller override.
    Recovery(String),
    RecoveryCue(String),
}

/// What a lookup found: the device and its name, or why not.
type Found = Result<(String, cpal::Device), String>;

impl Lookup {
    fn name(&self) -> &str {
        let (Lookup::Env(n)
        | Lookup::Controller(n)
        | Lookup::Settings(n)
        | Lookup::Recovery(n)
        | Lookup::RecoveryCue(n)) = self;
        n
    }

    /// The slow part: lists the host's devices.
    fn find(&self) -> Found {
        match self {
            Lookup::Env(want) => find_output(want, bus::OUTPUT_ENV),
            Lookup::RecoveryCue(want) => find_output(want, cue::ENV),
            Lookup::Recovery(name) => device::output_named(name)
                .map(|d| (name.clone(), d))
                .ok_or_else(|| format!("{name:?} is not available")),
            Lookup::Controller(want) => find_wanted(want),
            Lookup::Settings(name) => {
                // An HDMI port without a screen opens fine and plays silence: the default plays until something is plugged in.
                if device::unplugged(name) {
                    return Err(format!(
                        "nothing is plugged into {name:?} (no screen or receiver): the system default plays"
                    ));
                }
                device::output_named(name)
                    .map(|d| (name.clone(), d))
                    .ok_or_else(|| format!("{name:?} is not available: the system default plays"))
            }
        }
    }
}

/// How long an answer is reused. It holds an open PCM until a stream takes it, so it must not outlive the moment it is wanted.
const FRESH: Duration = Duration::from_secs(5);
static FOUND: Mutex<Vec<(Lookup, Instant, Found)>> = Mutex::new(Vec::new());
static RUNNING: Mutex<Vec<Lookup>> = Mutex::new(Vec::new());
static WAKE: OnceLock<fn()> = OnceLock::new();

/// Runs device lookups and output-health notifications off-thread; `wake` must post `super::settle` to the
/// stream-owning UI thread. Install before opening players for automatic recovery. Without it ordinary lookups block.
pub fn set_waker(wake: fn()) {
    let _ = WAKE.set(wake);
}

pub(super) fn wake() -> bool {
    let Some(wake) = WAKE.get() else { return false };
    wake();
    true
}

pub(super) fn forget_recovery(name: &str) {
    let key = Lookup::Recovery(name.to_owned());
    FOUND
        .lock()
        .retain(|(k, _, _)| k != &key && !matches!(k, Lookup::RecoveryCue(_)));
}

/// Reconnecting never substitutes the system default or enumerates devices on the stream owner.
pub(super) fn recover(name: &str, source: &'static str) -> Option<Result<Output, String>> {
    let main = resolve(&Lookup::Recovery(name.to_owned()));
    let cue = match std::env::var(cue::ENV).ok().filter(|v| !v.is_empty()) {
        Some(name) => Some(resolve(&Lookup::RecoveryCue(name))?),
        None => None,
    };
    main.map(|found| {
        found.and_then(|(name, dev)| prepare(name, dev, source, Vec::new(), None, cue))
    })
}

fn cached(l: &Lookup) -> Option<Found> {
    FOUND
        .lock()
        .iter()
        .find(|(k, at, _)| k == l && at.elapsed() < FRESH)
        .map(|(.., f)| f.clone())
}

fn store(l: &Lookup, f: &Found) {
    let mut v = FOUND.lock();
    v.retain(|(k, at, _)| k != l && at.elapsed() < FRESH);
    v.push((l.clone(), Instant::now(), f.clone()));
}

/// The answer for `l`; `None` = a worker is looking (started here when none is yet). The environment's device is looked up at
/// once: it is a developer's override, and its error lists the devices.
fn resolve(l: &Lookup) -> Option<Found> {
    if let Some(f) = cached(l) {
        return Some(f);
    }
    if !matches!(l, Lookup::Recovery(_) | Lookup::RecoveryCue(_))
        && (WAKE.get().is_none() || matches!(l, Lookup::Env(_)))
    {
        let f = l.find();
        store(l, &f);
        return Some(f);
    }
    let mut running = RUNNING.lock();
    if !running.contains(l) {
        running.push(l.clone());
        let l = l.clone();
        std::thread::spawn(move || {
            store(&l, &l.find());
            RUNNING.lock().retain(|k| k != &l);
            wake();
        });
    }
    None
}

/// The device the next stream is asked to open by name, if any.
fn lookup_now() -> Option<Lookup> {
    if let Some(want) = std::env::var(bus::OUTPUT_ENV).ok().filter(|v| !v.is_empty()) {
        return Some(Lookup::Env(want));
    }
    if let Some(want) = WANTED.with_borrow(Clone::clone) {
        return Some(Lookup::Controller(want));
    }
    SETTINGS.with_borrow(|s| (!s.device.is_empty()).then(|| Lookup::Settings(s.device.clone())))
}

/// The name of the device the next stream wants when a worker is still looking for it (the lookup is running by now).
pub(super) fn unresolved() -> Option<String> {
    let l = lookup_now()?;
    resolve(&l).is_none().then(|| l.name().to_owned())
}

/// Device, config and layout for the next stream (see the module doc for the precedence). A device not found yet is
/// [`Status::pending`]: the system default is what this returns, the stream moves when the answer is there.
pub(super) fn choose() -> Result<Output, String> {
    let mut notes = Vec::new();
    let mut pending = None;
    let look = lookup_now();
    let (name, dev, source) = match (&look, look.as_ref().map(resolve)) {
        (Some(Lookup::Env(_)), Some(Some(f))) => {
            let (n, d) = f?;
            (n, d, bus::OUTPUT_ENV)
        }
        (Some(Lookup::Controller(_)), Some(Some(f))) => {
            let (n, d) = f?;
            (n, d, "controller")
        }
        (_, Some(Some(Ok((n, d))))) => (n, d, "settings"),
        (look, found) => {
            match found {
                Some(Some(Err(note))) => notes.push(note),
                Some(None) => pending = look.as_ref().map(|l| l.name().to_owned()),
                _ => {}
            }
            let (n, d) = device::default_output()?;
            (n, d, "system default")
        }
    };
    prepare(name, dev, source, notes, pending, None)
}

fn prepare(
    name: String,
    dev: cpal::Device,
    source: &'static str,
    mut notes: Vec<String>,
    pending: Option<String>,
    cue_device: Option<Found>,
) -> Result<Output, String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let set = settings();
    let rate_want = (set.rate > 0).then_some(set.rate);
    let external = set.mixer == Mixer::External;
    let (cue_want, hp_env) = (env(cue::ENV), env(bus::HP_ENV));
    // Headphones on this device's own channels, unless a second device was asked for. A failure is kept for the UI, never papered over.
    let mut multi = if external {
        Ok(None)
    } else {
        match (&cue_want, &hp_env) {
            (Some(_), Some(_)) => Err(format!(
                "{} and {} are both set: pick one headphone route",
                cue::ENV,
                bus::HP_ENV
            )),
            (Some(_), None) => Ok(None),
            (None, None) if set.phones > 0 => Ok(route::pair(set.phones)),
            _ => bus::phones_channels(hp_env.as_deref(), &name),
        }
    };
    // A controller's interface with four outputs is a master + headphones device (1/2 and 3/4); one without is plain stereo, not a failure.
    if matches!(multi, Ok(None))
        && !external
        && cue_want.is_none()
        && env(bus::OUTPUT_ENV).is_none()
        && source == "controller"
        && config_for(&dev, &name, 4, None).is_ok()
    {
        multi = Ok(Some([2, 3]));
    }
    let (phones, mut failed) = match multi {
        Ok(p) => (p, None),
        Err(e) => (None, Some(e)),
    };
    let planned = Plan::new(&set, phones, device::max_channels(&dev)).and_then(|(plan, n)| {
        let (cfg, rate_note) = config_for(&dev, &name, plan.channels(), rate_want)?;
        Ok((plan, cfg, n, rate_note))
    });
    let mut error = None;
    let (plan, sup) = match planned {
        Ok((plan, sup, n, rate_note)) => {
            notes.extend(n);
            notes.extend(rate_note);
            (plan, sup)
        }
        Err(e) => {
            if phones.is_some() {
                failed.get_or_insert_with(|| e.clone()); // the headphone pair was the reason: the mixer says so
            }
            error = Some(e);
            let (sup, rate_note) = config_for(&dev, &name, 2, rate_want).or_else(|_| {
                dev.default_output_config()
                    .map(|c| (c, None))
                    .map_err(|e| e.to_string())
            })?;
            notes.extend(rate_note);
            (Plan::stereo(), sup)
        }
    };
    let fmt = sup.sample_format();
    let mut cfg: StreamConfig = sup.config();
    let mut buffer = None;
    if let Some(frames) = set.requested_buffer() {
        match *sup.buffer_size() {
            SupportedBufferSize::Range { min, max } if (min..=max).contains(&frames) => {
                cfg.buffer_size = BufferSize::Fixed(frames);
                buffer = Some(frames);
            }
            _ => notes.push(format!(
                "a buffer of {frames} frames is outside what the device supports: driver default"
            )),
        }
    }
    let rate = cfg.sample_rate.0;
    let (cue, headphones) = if external {
        (
            None,
            Headphones::Device(
                "none: External mixer mode, every deck has its own output".to_owned(),
            ),
        )
    } else {
        match (failed, cue_want, plan.phones) {
            (Some(e), ..) => (None, Headphones::Failed(e)),
            (None, None, None) => (None, Headphones::Split),
            (None, None, Some([a, b])) => (
                None,
                Headphones::Device(format!("{name} ch {}/{}", a + 1, b + 1)),
            ),
            (None, Some(want), _) => match cue_device.map_or_else(
                || cue::open(&want, rate),
                |found| found.and_then(|(name, dev)| cue::open_device(name, dev, rate)),
            ) {
                Ok(c) => {
                    let name = c.name.clone();
                    (Some(c), Headphones::Device(name))
                }
                Err(e) => (None, Headphones::Failed(e)),
            },
        }
    };
    let status = Status {
        device: name,
        source,
        rate,
        channels: cfg.channels as usize,
        buffer,
        notes,
        error,
        pending,
    };
    Ok(Output {
        dev,
        cfg,
        fmt,
        plan,
        cue,
        headphones,
        status,
    })
}

/// Output config of `dev` with at least `min` channels (fewest first, f32 preferred), at `rate` when the device has it
/// there, else at its default rate (with a note saying so).
pub(super) fn config_for(
    dev: &cpal::Device,
    name: &str,
    min: usize,
    rate: Option<u32>,
) -> Result<(SupportedStreamConfig, Option<String>), String> {
    let default = dev
        .default_output_config()
        .map_err(|e| format!("{name}: {e}"))?
        .sample_rate();
    let ranges: Vec<_> = dev
        .supported_output_configs()
        .map_err(|e| format!("{name}: {e}"))?
        .filter(|c| c.channels() as usize >= min)
        .filter(|c| {
            matches!(
                c.sample_format(),
                SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16
            )
        }) // the formats `start` plays
        .collect();
    let at = |r: cpal::SampleRate| {
        ranges
            .iter()
            .filter(|c| (c.min_sample_rate()..=c.max_sample_rate()).contains(&r))
            .min_by_key(|c| (c.channels(), c.sample_format() != SampleFormat::F32))
            .map(|c| (*c).with_sample_rate(r))
    };
    let wanted = rate.map(cpal::SampleRate);
    if let Some(c) = wanted.and_then(at) {
        return Ok((c, None));
    }
    let c = at(default)
        .ok_or_else(|| format!("{name}: no output with {min} channels at {} Hz", default.0))?;
    let note = wanted.map(|r| {
        format!(
            "{} Hz is not available with {min} channels on {name}: {} Hz plays",
            r.0, default.0
        )
    });
    Ok((c, note))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_forgets_only_recovery_handles_without_changing_output_intent() {
        let name = "unit-test-disconnected-output";
        let recovery = Lookup::Recovery(name.into());
        let preference = Lookup::Settings(name.into());
        let missing: Found = Err("disconnected".into());
        let old_settings = replace_settings(Settings {
            device: name.into(),
            ..settings()
        });
        let old_controller = replace_wanted(Some("unit-test-controller".into()));
        store(&recovery, &missing);
        store(&preference, &missing);
        assert!(cached(&recovery).unwrap().is_err());
        forget_recovery(name);
        assert!(cached(&recovery).is_none());
        assert!(cached(&preference).unwrap().is_err());
        assert_eq!(settings().device, name);
        assert_eq!(controller().as_deref(), Some("unit-test-controller"));
        replace_settings(old_settings);
        replace_wanted(old_controller);
        FOUND.lock().retain(|(key, _, _)| key != &preference);
    }

    #[test]
    fn a_controller_name_finds_its_alsa_card_without_punctuation_or_case() {
        assert!(name_matches("front:CARD=CTRL4X,DEV=0", "CTRL-4X"));
        assert!(name_matches("CTRL-4X Output", "ctrl 4x"));
        assert!(!name_matches("hdmi:CARD=NVidia,DEV=2", "CTRL-4X"));
        assert!(
            !name_matches("anything", "--"),
            "a name without letters or digits matches nothing"
        );
    }
}
