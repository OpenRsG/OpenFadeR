//! What the OS audio devices offer: names, channel counts, supported sample rates and buffer sizes, for the
//! Preferences dropdowns. Enumerating hardware can take a while (ALSA opens every PCM), so the answers are cached for
//! [`TTL`]; the lists are rebuilt after every click of the dialog. Children: [`card`] (the raw cpal names grouped into
//! cards with named ports), [`volume`] (volume and mute of the device in use), and `sys` (what `pactl`, `/proc/asound`,
//! udev and the HDA jack controls say, shared by both).

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{SupportedBufferSize, SupportedStreamConfigRange};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

pub mod card;
mod sys;
pub mod volume;

use sys::Facts;

/// Rates offered (those the device supports), Hz.
pub const RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// Buffer sizes offered (those inside the device's range), frames.
pub const BUFFERS: [u32; 6] = [64, 128, 256, 512, 1024, 2048];
/// How long a device list or capability answer is reused.
const TTL: Duration = Duration::from_secs(5);

/// Stop alsa-lib printing to stderr. Listing devices opens every PCM (dmix, dsnoop, OSS, surround maps), and each
/// one that does not apply prints an `ALSA lib … [error.pcm]` line although cpal handles the failure. Real errors
/// still reach us as cpal `Result`s.
pub fn quiet_alsa() {
    use std::ffi::{c_char, c_int};
    type Handler =
        unsafe extern "C" fn(*const c_char, c_int, *const c_char, c_int, *const c_char, ...);
    unsafe extern "C" {
        fn snd_lib_error_set_handler(handler: Option<Handler>) -> c_int;
    }
    unsafe extern "C" fn silent(
        _: *const c_char,
        _: c_int,
        _: *const c_char,
        _: c_int,
        _: *const c_char,
    ) {
    }
    // SAFETY: a callee that ignores its variadic tail is call-compatible on the SysV x86-64 and AArch64 Linux ABIs
    // (the caller owns the argument area); stable Rust cannot define a C-variadic function itself.
    unsafe {
        let silent: Handler = std::mem::transmute(silent as unsafe extern "C" fn(_, _, _, _, _));
        snd_lib_error_set_handler(Some(silent));
    }
}

/// What one device supports (over all its configurations).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Caps {
    pub name: String,
    /// Most channels of any configuration.
    pub channels: usize,
    /// [`RATES`] inside some configuration's range.
    pub rates: Vec<u32>,
    /// [`BUFFERS`] inside the reported range; empty when the driver reports none.
    pub buffers: Vec<u32>,
}

/// The [`RATES`] covered by some `(min, max)` range.
fn rates_of(ranges: &[(u32, u32)]) -> Vec<u32> {
    RATES
        .into_iter()
        .filter(|r| ranges.iter().any(|&(a, b)| (a..=b).contains(r)))
        .collect()
}

/// The [`BUFFERS`] inside the widest buffer range, `None` = the driver does not say.
fn buffers_of(range: Option<(u32, u32)>) -> Vec<u32> {
    range.map_or_else(Vec::new, |(a, b)| {
        BUFFERS
            .into_iter()
            .filter(|s| (a..=b).contains(s))
            .collect()
    })
}

/// Output delay of one buffer in ms (the driver may add more: a lower bound, not a measurement).
pub fn latency_ms(frames: u32, rate: u32) -> f32 {
    if rate == 0 {
        0.0
    } else {
        frames as f32 * 1000.0 / rate as f32
    }
}

fn caps_of(name: &str, configs: impl Iterator<Item = SupportedStreamConfigRange>) -> Caps {
    let (mut channels, mut ranges, mut buf): (usize, Vec<(u32, u32)>, Option<(u32, u32)>) =
        (0, Vec::new(), None);
    for c in configs {
        channels = channels.max(c.channels() as usize);
        ranges.push((c.min_sample_rate().0, c.max_sample_rate().0));
        if let SupportedBufferSize::Range { min, max } = *c.buffer_size() {
            buf = Some(buf.map_or((min, max), |(a, b)| (a.min(min), b.max(max))));
        }
    }
    Caps {
        name: name.to_owned(),
        channels,
        rates: rates_of(&ranges),
        buffers: buffers_of(buf),
    }
}

#[derive(Default)]
struct Cache {
    /// Device names, `[output, input]`.
    lists: [Option<(Instant, Vec<String>)>; 2],
    caps: HashMap<(bool, String), (Instant, Option<Caps>)>,
}

/// Shared by every thread: the Preferences' background refresh keeps it warm so a click on the UI thread rarely pays for an
/// enumeration (ALSA opens every PCM: about 0.25 s here).
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

fn fresh(t: &Instant) -> bool {
    t.elapsed() < TTL
}

/// Every output name cpal has listed since the start (kept across [`refresh`]): the names of a PCM that is in use now.
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The devices as [`card::Card`]s (the system default first): friendly titles from the sound server and the udev database,
/// ports named in plain words, HDMI connection state. `show_all` adds the plugins and converters as extra ports; `saved` is
/// the name in the settings, kept selectable (see [`card::group`]). cpal lists only the PCMs it can open, so a PCM that is
/// in use (by the sound server, or by our own stream) drops out of its list: [`card::unlisted`] puts those back from the names
/// seen before, and a card left with nothing listed is [`card::Card::busy`]. The cpal lists (duplicates dropped) and the
/// system facts are cached for [`TTL`]. Input cards are titled from the udev model, not from an output sink.
pub fn cards(input: bool, show_all: bool, saved: &str) -> Vec<card::Card> {
    let mut names = names(input);
    let mut facts = sys::facts();
    let busy = if input {
        Vec::new()
    } else {
        card::unlisted(&names, &SEEN.lock()[..], &facts)
    };
    names.extend(busy.iter().cloned());
    let mut cards = card::group(&names, show_all, saved);
    for c in &mut cards {
        c.busy = c.kind == card::Kind::Card
            && !c.ports.is_empty()
            && c.ports.iter().all(|p| busy.contains(&p.name));
    }
    if input {
        facts.sinks.clear();
        facts.default_sink = None;
    }
    card::enrich(&mut cards, &facts);
    cards
}

/// Forgets every cached list and answer: the next call sees devices plugged since (the Preferences call this when the
/// AUDIO page opens).
pub fn refresh() {
    *CACHE.lock() = Cache::default();
    sys::forget();
}

fn names(input: bool) -> Vec<String> {
    if let Some((_, v)) = CACHE.lock().lists[input as usize]
        .as_ref()
        .filter(|(t, _)| fresh(t))
    {
        return v.clone();
    }
    let mut v: Vec<String> = Vec::new();
    for n in devices(input).filter_map(|d| d.name().ok()) {
        if !v.contains(&n) {
            v.push(n);
        }
    }
    CACHE.lock().lists[input as usize] = Some((Instant::now(), v.clone()));
    if !input {
        let mut seen = SEEN.lock();
        for n in &v {
            if !seen.contains(n) {
                seen.push(n.clone());
            }
        }
    }
    v
}

/// Output device named exactly `name`.
pub fn output_named(name: &str) -> Option<cpal::Device> {
    named(false, name)
}

fn named(input: bool, name: &str) -> Option<cpal::Device> {
    devices(input).find(|d| d.name().is_ok_and(|n| n == name))
}

/// The host's input or output devices; none when it cannot list them.
fn devices(input: bool) -> impl Iterator<Item = cpal::Device> {
    let host = cpal::default_host();
    if input {
        host.input_devices()
    } else {
        host.output_devices()
    }
    .into_iter()
    .flatten()
}

/// The system default output device and its name.
pub fn default_output() -> Result<(String, cpal::Device), String> {
    let d = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output device")?;
    Ok((d.name().unwrap_or_default(), d))
}

/// Capabilities of the output device `name` (empty = the system default); `None` when it is gone or cannot be queried.
pub fn output_caps(name: &str) -> Option<Caps> {
    caps(false, name)
}

/// Capabilities of the input device `name` (empty = the system default).
pub fn input_caps(name: &str) -> Option<Caps> {
    caps(true, name)
}

fn caps(input: bool, name: &str) -> Option<Caps> {
    let key = (input, name.to_owned());
    if let Some(c) = CACHE
        .lock()
        .caps
        .get(&key)
        .filter(|(t, _)| fresh(t))
        .map(|(_, v)| v.clone())
    {
        return c;
    }
    let host = cpal::default_host();
    let found = match (name.is_empty(), input) {
        (true, false) => host.default_output_device(),
        (true, true) => host.default_input_device(),
        (false, _) => named(input, name),
    };
    let result = found.and_then(|d| {
        let n = d.name().unwrap_or_else(|_| name.to_owned());
        Some(if input {
            caps_of(&n, d.supported_input_configs().ok()?)
        } else {
            caps_of(&n, d.supported_output_configs().ok()?)
        })
    });
    CACHE
        .lock()
        .caps
        .insert(key, (Instant::now(), result.clone()));
    result
}

/// Most channels `dev` can open an output with (2 when it cannot say).
pub fn max_channels(dev: &cpal::Device) -> usize {
    dev.supported_output_configs()
        .ok()
        .and_then(|c| c.map(|c| c.channels() as usize).max())
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_keep_only_what_some_range_covers() {
        assert_eq!(rates_of(&[(8_000, 48_000)]), [44_100, 48_000]);
        assert_eq!(
            rates_of(&[(44_100, 44_100), (96_000, 192_000)]),
            [44_100, 96_000, 176_400, 192_000]
        );
        assert_eq!(rates_of(&[(22_050, 22_050)]), Vec::<u32>::new());
        assert!(rates_of(&[]).is_empty());
    }

    #[test]
    fn buffer_sizes_stay_inside_the_reported_range() {
        assert_eq!(buffers_of(Some((128, 1024))), [128, 256, 512, 1024]);
        assert_eq!(buffers_of(Some((100, 100))), Vec::<u32>::new());
        assert!(
            buffers_of(None).is_empty(),
            "an unknown range offers only Auto"
        );
    }

    #[test]
    fn latency_is_frames_over_rate() {
        assert_eq!(latency_ms(480, 48_000), 10.0);
        assert!((latency_ms(512, 44_100) - 11.609).abs() < 0.001);
        assert_eq!(latency_ms(512, 0), 0.0);
    }

    #[test]
    fn caps_merge_the_configurations() {
        let cfg = |ch, lo, hi, b: SupportedBufferSize| {
            SupportedStreamConfigRange::new(
                ch,
                cpal::SampleRate(lo),
                cpal::SampleRate(hi),
                b,
                cpal::SampleFormat::F32,
            )
        };
        let c = caps_of(
            "x",
            [
                cfg(
                    2,
                    44_100,
                    48_000,
                    SupportedBufferSize::Range { min: 64, max: 512 },
                ),
                cfg(
                    6,
                    96_000,
                    96_000,
                    SupportedBufferSize::Range {
                        min: 256,
                        max: 4096,
                    },
                ),
                cfg(1, 48_000, 48_000, SupportedBufferSize::Unknown),
            ]
            .into_iter(),
        );
        assert_eq!(
            (c.channels, c.rates, c.buffers),
            (
                6,
                vec![44_100, 48_000, 96_000],
                vec![64, 128, 256, 512, 1024, 2048]
            )
        );
    }
}
