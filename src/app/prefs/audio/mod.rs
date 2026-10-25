//! AUDIO category (Preferences > Audio: Configuration and Input/Output tabs; values and defaults are
//! our choice). Typed section [`Audio`], the page rows ([`page`]), and [`wire`], which hands the settings to the audio engine.
//!
//! What each row does:
//! * **Audio** (`output_device`): the output device, shown as cards with named ports (`cards`, `device::card`); the stored
//!   value stays the raw cpal name. Applied live by reopening the stream ([`audio::configure`]). Precedence:
//!   `OPENFADER_OUTPUT_DEVICE` > the connected controller's own interface (CONTROLLER page "Controller audio"; taken on
//!   connect, given up when a card is picked here while it plays) > this row
//!   > the system default. A saved device that is gone falls back to the default; its card says so. Volume and mute of a
//!   card act on the device through `device::volume`.
//! * **Sample Rate** / **Buffer size**: only what the chosen device supports ([`audio::device`]); AUTO = the
//!   device's default. Applied when the stream is (re)opened; the whole engine runs at that rate (every unit takes the
//!   stream's rate, tracks are resampled to it). The latency shown is one buffer, `frames / rate` (a lower bound).
//! * **Metronome** (tone, volume): the click on every beat of a deck's grid, mixed into the master; the per-deck on/off
//!   is `Player::set_metronome`, the grid `Player::set_beats` (the grid-edit UI calls them).
//! * **Preview Volume**: the level of the browser preview player (`audio::live_bus()` → `Bus::preview`).
//! * **Control Device Information**: read-only line: the connected controller and the audio output it got.
//! * **Mixer Mode**: Internal (our mixer) or External (Decks 1 to 4, Sampler and Preview raw on their own pairs; needs a
//!   device with four or more channels, otherwise the option is shown disabled).
//! * **Input channels**: the input device and channel pair, and whether REC records the master or this input
//!   ([`Audio::rec_input`], [`audio::input::open`]).
//! * **Output channels**: Internal: Master / Headphones / Booth pairs (+ booth level); External: the pair of each of the
//!   six outputs. Pair 1 = device channels 1/2, 2 = 3/4, … Headphones AUTO = `OPENFADER_HP_CHANNELS`, a four-output controller
//!   interface, else SPLIT on the master pair.

mod cards;
mod page;
#[cfg(test)]
mod tests;

pub use page::rows;

use super::{Prefs, choice, section};
use openfader::audio::{
    self,
    device::{self, card, Caps},
    metronome::TONES,
    mic,
    route::{self, Mixer, Settings},
    Status,
};
use std::{cell::RefCell, rc::Rc};

choice!(
    /// Mixer Mode: our mixer, or per-deck outputs for an external mixer.
    MixerMode { #[default] Internal = "internal", External = "external" }
);
choice!(
    /// What REC records: the master mix, or the chosen audio input pair.
    RecSource { #[default] Master = "master", Input = "input" }
);
choice!(
    /// Talkover Mode: Advanced ducks only the mid range of the other channels, Normal all of them.
    TalkMode { #[default] Advanced = "advanced", Normal = "normal" }
);

section!(
    /// The AUDIO settings; every field is stored under its own name.
    Audio {
        /// Output device name; empty = the system default.
        output_device: String = String::new();
        /// "Show all ALSA devices": list the plugins, converters and raw names too (the output cards and the input list).
        output_show_all: bool = false;
        /// Sample rate in Hz and buffer size in frames; 0 = automatic.
        sample_rate: u32 = 0;
        buffer_size: u32 = 0;
        /// Metronome timbre index (0..3) and volume percent.
        metronome_tone: u8 = 0;
        metronome_volume: u8 = 50;
        /// Preview volume in dB, -99 = muted.
        preview_volume: i32 = 0;
        /// Mixer Mode.
        mixer_mode: MixerMode = MixerMode::Internal;
        /// Output pairs (1 = channels 1/2, …). Internal: master, headphones (0 = automatic), booth (0 = off) and the booth level in dB.
        out_master: u8 = 1;
        out_phones: u8 = 0;
        out_booth: u8 = 0;
        booth_level: i32 = 0;
        /// External: the pair of Deck 1 to 4, Sampler, Preview (0 = not routed).
        out_deck1: u8 = 1;
        out_deck2: u8 = 2;
        out_deck3: u8 = 3;
        out_deck4: u8 = 4;
        out_sampler: u8 = 5;
        out_preview: u8 = 6;
        /// Input device (empty = the system default) and channel pair for REC, and what REC records.
        input_device: String = String::new();
        input_pair: u8 = 1;
        rec_source: RecSource = RecSource::Master;
        /// Microphone input (Controller > Mixer > Microphone Input): on, the pair of the input device, level (dB),
        /// Talkover Mode and the talkover level in dB (0 = no ducking).
        mic_on: bool = false;
        mic_pair: u8 = 1;
        mic_level: i32 = 0;
        talkover_mode: TalkMode = TalkMode::Advanced;
        talkover_level: i32 = -18;
        /// AUX input: the AUX strip shows in the mixer (its ON opens the input), the input device (empty = the one above) and pair,
        /// the talkover-style ducking of the decks in dB (0 = off) and their release in ms.
        aux_on: bool = false;
        aux_device: String = String::new();
        aux_pair: u8 = 1;
        aux_duck: i32 = 0;
        aux_release: u32 = 400;
    }
);

/// Preview Volume steps in dB (-99 = mute).
const PREVIEW_DB: [i32; 10] = [-99, -24, -18, -12, -9, -6, -3, 0, 3, 6];
/// Booth level steps in dB.
const BOOTH_DB: [i32; 7] = [-12, -9, -6, -3, 0, 3, 6];
/// Mic level steps and talkover levels (the DJM list, 0 = off), dB.
const MIC_DB: [i32; 8] = [-24, -18, -12, -6, -3, 0, 3, 6];
const TALK_DB: [i32; 5] = [0, -6, -12, -18, -24];
/// Release times of the AUX ducking, ms.
const AUX_RELEASE_MS: [u32; 5] = [200, 400, 800, 1500, 3000];
/// Metronome volume step, percent.
const VOLUME_STEP: u8 = 10;
/// Most output/input pairs offered (cpal reports at most 32 channels).
const MAX_PAIRS: usize = 16;

/// Linear gain of `db` (≤ -96 = silence).
fn db_gain(db: i32) -> f32 {
    if db <= -96 {
        0.0
    } else {
        10f32.powf(db as f32 / 20.0)
    }
}

fn db_label(db: i32) -> String {
    if db <= -96 {
        tr!("MUTE").to_owned()
    } else {
        format!("{db:+} dB").replace("+0", "0")
    }
}

/// Index of the step of `steps` closest to `v`.
fn nearest(steps: &[i32], v: i32) -> usize {
    (0..steps.len())
        .min_by_key(|&i| (steps[i] - v).abs())
        .unwrap_or(0)
}

impl Audio {
    /// The pair of each External output, in `route::EXTERNAL_NAMES` order.
    fn externals(&self) -> [u8; 6] {
        [
            self.out_deck1,
            self.out_deck2,
            self.out_deck3,
            self.out_deck4,
            self.out_sampler,
            self.out_preview,
        ]
    }

    fn externals_mut(&mut self) -> [&mut u8; 6] {
        [
            &mut self.out_deck1,
            &mut self.out_deck2,
            &mut self.out_deck3,
            &mut self.out_deck4,
            &mut self.out_sampler,
            &mut self.out_preview,
        ]
    }

    /// What the audio engine is told ([`audio::configure`]).
    pub fn settings(&self) -> Settings {
        Settings {
            device: self.output_device.clone(),
            rate: self.sample_rate,
            buffer: self.buffer_size,
            driver_buffer: false,
            mixer: if self.mixer_mode == MixerMode::External {
                Mixer::External
            } else {
                Mixer::Internal
            },
            master: self.out_master,
            phones: self.out_phones,
            booth: self.out_booth,
            external: self.externals(),
            booth_gain: db_gain(self.booth_level),
            metronome_tone: self.metronome_tone as usize,
            metronome_volume: self.metronome_volume as f32 / 100.0,
            preview_gain: db_gain(self.preview_volume),
            mic_gain: if self.mic_on {
                db_gain(self.mic_level)
            } else {
                0.0
            },
            talkover_advanced: self.talkover_mode == TalkMode::Advanced,
            talkover_gain: if self.mic_on {
                db_gain(self.talkover_level)
            } else {
                1.0
            },
            aux_duck: db_gain(self.aux_duck),
            aux_release: self.aux_release as f32 / 1000.0,
        }
    }

    /// REC records the chosen audio input instead of the master: open it with `audio::input::open(&a.input_device, a.input_pair)`
    /// and record its `tap()` (`Recorder::start`).
    pub fn rec_input(&self) -> bool {
        self.rec_source == RecSource::Input
    }

    /// The AUX strip's input: the device (the INPUT DEVICE when no other is picked) and the pair.
    pub fn aux_source(&self) -> (&str, u8) {
        match self.aux_device.as_str() {
            "" => (&self.input_device, self.aux_pair),
            d => (d, self.aux_pair),
        }
    }
}

/// The device whose capabilities the page shows: the one actually playing when the environment or a controller overrides the row.
fn caps_of(p: &Prefs, st: &Status) -> Option<Caps> {
    let name = match st.source {
        "OPENFADER_OUTPUT_DEVICE" | "controller" => st.device.as_str(),
        _ => p.audio.output_device.as_str(),
    };
    device::output_caps(name)
}

/// The value behind option `i` of [`page::value_options`]: 0 = AUTO; `None` for the "(not supported)" entry or past the end.
fn value_at(supported: &[u32], i: usize) -> Option<u32> {
    match i {
        0 => Some(0),
        i => supported.get(i - 1).copied(),
    }
}

/// External mixer mode needs a device with at least two pairs.
fn external_ok(channels: usize) -> bool {
    channels >= 4
}

fn pairs_of(caps: Option<&Caps>) -> (usize, usize) {
    let ch = caps.map_or(2, |c| c.channels.max(1));
    (ch, route::pairs(ch).min(MAX_PAIRS))
}

fn buffer_in_use(_p: &Prefs, st: &Status) -> u32 {
    // A callback contains a period, not the full ALSA buffer. Unknown driver buffering must not be labelled as its size.
    st.buffer.unwrap_or(0)
}

fn effective_settings(p: &Prefs) -> Settings {
    Settings { driver_buffer: super::lowpower::on(p), ..p.audio.settings() }
}

pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    let st = audio::status();
    let caps = caps_of(p, &st);
    let a = &mut p.audio;
    match key {
        k if k.starts_with("out:") => {
            let cards = device::cards(false, a.output_show_all, &a.output_device);
            if let Some(n) = cards::pick(&cards, &k[4..], i) {
                if st.source == "controller" {
                    // The user's pick beats the controller's interface. A changed device moves the stream in `wire`'s
                    // `configure`; the same one would not change the settings, so it is reopened here.
                    audio::release_controller();
                    if n == a.output_device {
                        let _ = audio::set_output(None);
                        refresh_when_settled();
                    }
                }
                a.output_device = n;
            }
        }
        "sample_rate" => {
            if let Some(v) = value_at(caps.as_ref().map_or(&[][..], |c| &c.rates[..]), i) {
                a.sample_rate = v;
            }
        }
        "buffer_size" => {
            if let Some(v) = value_at(caps.as_ref().map_or(&[][..], |c| &c.buffers[..]), i) {
                a.buffer_size = v;
            }
        }
        "metronome_tone" => a.metronome_tone = i.min(TONES - 1) as u8,
        "metronome_volume" => a.metronome_volume = (i as u8).saturating_mul(VOLUME_STEP).min(100),
        "preview_volume" => a.preview_volume = PREVIEW_DB[i.min(PREVIEW_DB.len() - 1)],
        "booth_level" => a.booth_level = BOOTH_DB[i.min(BOOTH_DB.len() - 1)],
        "mixer_mode" => {
            let (channels, _) = pairs_of(caps.as_ref());
            if i == 0 || external_ok(channels) {
                a.mixer_mode = if i == 0 {
                    MixerMode::Internal
                } else {
                    MixerMode::External
                };
            }
        }
        "out_master" => a.out_master = (i + 1).min(MAX_PAIRS) as u8,
        "out_phones" => a.out_phones = i.min(MAX_PAIRS) as u8,
        "out_booth" => a.out_booth = i.min(MAX_PAIRS) as u8,
        "input_device" => {
            let ch = card::choices(&device::cards(true, a.output_show_all, &a.input_device));
            if let Some((_, n)) = ch.get(i) {
                a.input_device = n.clone();
            }
        }
        "input_pair" => a.input_pair = (i + 1).min(MAX_PAIRS) as u8,
        "mic_pair" => a.mic_pair = (i + 1).min(MAX_PAIRS) as u8,
        "mic_level" => a.mic_level = MIC_DB[i.min(MIC_DB.len() - 1)],
        "talkover_level" => a.talkover_level = TALK_DB[i.min(TALK_DB.len() - 1)],
        "aux_device" => {
            if let Some((_, n)) = aux_choices(a).get(i) {
                a.aux_device = n.clone();
            }
        }
        "aux_pair" => a.aux_pair = (i + 1).min(MAX_PAIRS) as u8,
        "aux_duck" => a.aux_duck = TALK_DB[i.min(TALK_DB.len() - 1)],
        "aux_release" => a.aux_release = AUX_RELEASE_MS[i.min(AUX_RELEASE_MS.len() - 1)],
        _ => match key
            .strip_prefix("ext_")
            .and_then(|k| k.parse::<usize>().ok())
            .and_then(|k| a.externals_mut().into_iter().nth(k))
        {
            Some(v) => *v = i.min(MAX_PAIRS) as u8,
            None => return a.pick(key, i),
        },
    }
    true
}

/// The AUX device drop-down: "same as the input device" (empty name), then every input device that is not the system default.
fn aux_choices(a: &Audio) -> Vec<(String, String)> {
    let mut out = vec![(tr!("SAME AS INPUT DEVICE").to_owned(), String::new())];
    out.extend(
        card::choices(&device::cards(true, a.output_show_all, &a.aux_device))
            .into_iter()
            .filter(|(_, n)| !n.is_empty()),
    );
    out
}

/// The status (device, rate, notes) changes again when a device being looked for is found and the stream moves: redraw then.
fn refresh_when_settled() {
    audio::when_settled(|_| {
        super::refresh();
        cards::sync();
    });
}

/// Hands the settings to the audio engine now and after every change ([`audio::configure`]: live knobs at once, the device /
/// rate / buffer / layout by moving the stream). Call after `prefs::wire` has loaded the file and before the decks are created
/// (the first `Player` opens the stream with these settings).
pub fn wire() {
    // A device is looked up on a worker thread; the UI thread moves the stream when it is found.
    audio::set_waker(|| _ = slint::invoke_from_event_loop(audio::settle));
    device::never_block_here(); // the page reads the lists the background refresh keeps (cards::sync)
    let ins = super::get().audio;
    mic::open(ins.mic_on, &ins.input_device, ins.mic_pair);
    let last = Rc::new(RefCell::new(effective_settings(&super::get())));
    if let Err(e) = audio::configure(last.borrow().clone()) {
        eprintln!("prefs: audio: {e}");
    }
    super::on_change(move |p| {
        mic::open(p.audio.mic_on, &p.audio.input_device, p.audio.mic_pair); // device / pair are not stream knobs
        let s = effective_settings(p);
        if s == *last.borrow() {
            return;
        }
        *last.borrow_mut() = s.clone();
        if let Err(e) = audio::configure(s) {
            eprintln!("prefs: audio: {e}");
        }
        refresh_when_settled();
        super::refresh(); // the status (device, rate, latency, notes) changed with the stream
    });
    super::on_change(|_| cards::sync()); // after the closure above: the cards show the status it produced
    cards::wire();
}
