//! Master / headphone bus: pure routing math ([`out`]) and the knob values the audio thread reads ([`Bus`]).
//!
//! Two headphone schemes. With one stereo device, headphones use "split" (SPLIT): with split on, the left output carries the mono headphone feed and the right
//! output the mono master; with split off the master plays as stereo and the cue feed is not heard.
//! With a second device ([`super::cue`], `OPENFADER_CUE_DEVICE`) the main device always plays the
//! master stereo and the second one the stereo headphone feed ([`Out::phones`]); SPLIT then has no
//! effect. A third scheme puts both on ONE multichannel device (`OPENFADER_OUTPUT_DEVICE`, [`phones_channels`]):
//! master on channels 1/2, the headphone feed on 3/4 (or `OPENFADER_HP_CHANNELS`), other channels silent; SPLIT has no
//! effect there either. Headphone feed = equal-power blend of cue and master by the MIX knob (0 = cue only, 1 =
//! master only; the default 0 gives "left = cue" exactly) × the headphone level. Cue is the channels'
//! post-EQ, post-filter, pre-fader signal (the usual hardware-mixer PFL wiring). The bus also owns the recorder [`Tap`], the booth level, the metronome's tone and volume, the
//! preview player and the buffer size in use. Which device channel pairs carry master / headphones / booth (or the
//! External mixer's six outputs) is [`super::route`]'s; the environment variables above still override the Preferences.

use super::{clamped, load_f32, mono, store_clamped, Tap};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

/// Env: substring of the output device to open instead of the system default.
pub const OUTPUT_ENV: &str = "OPENFADER_OUTPUT_DEVICE";
/// Env: `a,b` (1-based, both above the master's 1/2) = the device channels carrying the headphone feed.
pub const HP_ENV: &str = "OPENFADER_HP_CHANNELS";

/// Name fragments (lowercase) of cards whose outputs 3/4 carry the headphones, from `audio-cards.json`
/// (`{"headphones_3_4": ["fragment", …]}`) in the first mapping folder that has one; none when absent.
fn cards() -> &'static [String] {
    static CARDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
        crate::paths::search_path("OPENFADER_MAPPINGS", "mappings")
            .iter()
            .find_map(|d| std::fs::read(d.join("audio-cards.json")).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| {
                v["headphones_3_4"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_lowercase))
                        .collect()
                })
            })
            .unwrap_or_default()
    });
    &CARDS
}

/// Zero-based device channels of the headphone feed on a multichannel device, `None` for plain stereo/SPLIT.
/// `hp` = [`HP_ENV`]; unset, a device matching an `audio-cards.json` entry (a DJ controller: master on its
/// outputs 1/2, headphones on 3/4) gets channels 3/4. Errors name the env var.
pub fn phones_channels(hp: Option<&str>, device: &str) -> Result<Option<[usize; 2]>, String> {
    phones_channels_in(hp, device, cards())
}

fn phones_channels_in(
    hp: Option<&str>,
    device: &str,
    cards: &[String],
) -> Result<Option<[usize; 2]>, String> {
    let Some(hp) = hp else {
        let device = device.to_lowercase();
        return Ok(cards
            .iter()
            .any(|c| device.contains(c.as_str()))
            .then_some([2, 3]));
    };
    let parsed: Vec<usize> = hp
        .split(',')
        .filter_map(|c| c.trim().parse().ok())
        .collect();
    match parsed[..] {
        [a, b] if a >= 3 && b >= 3 && a != b && hp.split(',').count() == 2 => {
            Ok(Some([a - 1, b - 1]))
        }
        _ => Err(format!(
            "{HP_ENV}={hp:?}: want two different channel numbers from 3 up, like \"3,4\" (channels 1/2 carry the master)"
        )),
    }
}

/// Where the headphone feed goes, decided once when the stream opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Headphones {
    /// No second device requested: SPLIT on the main device.
    Split,
    /// Separate headphone output in use: a second device (its name) or channels of the main one ("<device> ch 3/4").
    Device(String),
    /// A separate output was requested but could not be set up (the reason, listing the available devices); split stays in effect.
    Failed(String),
}

/// Upper bound of the master and headphone gains (+6 dB).
pub const MAX_LEVEL: f32 = 2.0;

/// Constant-power split of `x` (0..1): `[cos, sin]`, gains² always sum to 1. Crossfader and headphone mix.
pub fn equal_power(x: f32) -> [f32; 2] {
    let a = x.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    [a.cos(), a.sin()]
}

/// Per-frame bus settings, every field already a plain linear value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub master: f32,
    pub split: bool,
    /// Headphone mix: 0 = cue only, 1 = master only.
    pub mix: f32,
    pub headphone: f32,
}

/// What one bus frame sends where.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Out {
    /// Main device frame: the SPLIT feed or the stereo master.
    pub device: [f32; 2],
    /// Stereo master after the master gain (what the recorder takes), whatever `split` says.
    pub master: [f32; 2],
    /// Stereo headphone feed for a second device: blend of cue and master by `mix`, × headphone level.
    pub phones: [f32; 2],
}

/// Main device frame: the SPLIT feed (mono cue left, mono master right) or the stereo master `m`; `[gc, gm]` = [`equal_power`] of the mix.
fn device_frame(m: [f32; 2], cue: [f32; 2], p: &Params, [gc, gm]: [f32; 2]) -> [f32; 2] {
    if !p.split {
        return m;
    }
    [(mono(cue) * gc + mono(m) * gm) * p.headphone, mono(m)]
}

/// Everything one frame feeds, from the summed channel signals `master` (post fader / crossfader) and `cue` (pre
/// fader): the main device, the recorder and a second headphone device.
pub fn out(master: [f32; 2], cue: [f32; 2], p: &Params) -> Out {
    let m = master.map(|x| x * p.master);
    let gains @ [gc, gm] = equal_power(p.mix);
    Out {
        device: device_frame(m, cue, p, gains),
        master: m,
        phones: [0, 1].map(|k| (cue[k] * gc + m[k] * gm) * p.headphone),
    }
}

/// Mixer channels (decks) the bus carries; the audio engine renders this many. Four decks.
/// In 2-deck mode channels 3 and 4 stay silent (nothing is loaded on them) and cost almost nothing.
pub const CHANNELS: usize = 4;
/// Index of the AUX channel's buffer in the channel buffers the mixer sums (the decks come first).
pub const AUX: usize = CHANNELS;
/// Beat FX units (FX1, FX2).
pub const FX_UNITS: usize = 2;
/// Effect slots of a Beat FX unit (MULTI mode: FX1-1 to FX1-3), each with its own effect, beat, level and ON.
pub const FX_SLOTS: usize = 3;

/// Knob values shared with the audio thread (all `f32` bits in atomics) plus the master peak it writes back.
pub struct Bus {
    master: AtomicU32,
    mix: AtomicU32,
    headphone: AtomicU32,
    split: AtomicBool,
    level: AtomicU32,
    headphones: parking_lot::Mutex<Headphones>,
    pub(super) tap: Arc<Tap>,
    /// The Beat FX knobs ([`super::fx`]): per unit (FX1, FX2) its three slots, each with its own effect, beat, level, ON,
    /// assignment and tempo (the app gives a unit's slots one assignment and tempo). The audio thread runs FX1-1 to FX2-3 in
    /// that order each buffer, so all of them can stack on one channel.
    pub fx: [[super::fx::Controls; FX_SLOTS]; FX_UNITS],
    /// One PAD FX unit's knobs per mixer channel (index = deck): the deck's performance pads set effect, beat
    /// and ON while a pad is held; unit `i` is fixed on channel `i` ([`Bus::new`]), so decks never share one.
    pub pad_fx: [super::fx::Controls; CHANNELS],
    /// One MERGE FX side's knobs per mixer channel (index = deck; MFX1 = deck A, MFX2 = deck B), see [`super::merge`].
    pub merge: [super::merge::Controls; CHANNELS],
    /// The sampler's knobs ([`super::sampler`]); mixed into the master sum after the Beat FX.
    pub sampler: super::sampler::Controls,
    /// The browser preview player ([`super::preview`]): heard on the headphone feed, or on its own pair in External mixer mode.
    pub preview: super::preview::Controls,
    /// The microphone ([`super::mic`]); one process-wide set of knobs, so it survives a device switch.
    pub mic: Arc<super::mic::Controls>,
    /// The AUX / LINE channel ([`super::auxin`]); process-wide like the mic.
    pub aux: Arc<super::auxin::Controls>,
    /// MIC FX ([`super::fx::Unit::rack`] of the mic slot): runs on the mic after its level, before it joins the sum.
    pub mic_fx: super::fx::Controls,
    /// SEND FX knobs: ON, effect, RETURN level and parameter. The effect hears [`Bus::sends`] times each channel's master path.
    pub send: super::fx::Controls,
    /// Per channel send amount 0..1 (`f32` bits).
    sends: [AtomicU32; CHANNELS],
    /// LEVER FX knobs (assigned to the master): knob 0 = rest.
    pub lever: super::fx::Controls,
    /// Booth output level (linear), see [`super::route`].
    booth: AtomicU32,
    /// Metronome timbre and volume ([`super::metronome`]); the per-deck on/off and beat list are the player's.
    metro_tone: AtomicUsize,
    metro_volume: AtomicU32,
    /// Frames of the last output buffer the callback saw (the buffer size actually in use).
    frames: AtomicU32,
}

/// Slack between the audio thread and the recorder's writer: ~2.7 s at 48 kHz.
const TAP_FRAMES: usize = 1 << 17;

impl Bus {
    /// `rate` of the stream (the recorder tap's frames are at it) and where the headphone feed goes.
    pub(super) fn new(rate: u32, headphones: Headphones) -> Bus {
        Bus {
            master: AtomicU32::new(1f32.to_bits()),
            mix: AtomicU32::new(0f32.to_bits()),
            headphone: AtomicU32::new(1f32.to_bits()),
            split: AtomicBool::new(false),
            level: AtomicU32::new(0f32.to_bits()),
            headphones: parking_lot::Mutex::new(headphones),
            tap: Arc::new(Tap::new(rate, TAP_FRAMES)),
            fx: std::array::from_fn(|_| std::array::from_fn(|_| super::fx::Controls::default())),
            pad_fx: std::array::from_fn(|i| {
                let c = super::fx::Controls::default();
                c.set_assign(super::fx::Assign::from_index(i));
                c
            }),
            merge: std::array::from_fn(|_| super::merge::Controls::default()),
            sampler: super::sampler::Controls::default(),
            preview: super::preview::Controls::default(),
            mic: super::mic::controls(),
            aux: super::auxin::controls(),
            mic_fx: super::fx::Controls::default(),
            send: super::fx::Controls::default(),
            sends: std::array::from_fn(|_| AtomicU32::new(0)),
            lever: {
                let c = super::fx::Controls::default();
                c.set_assign(super::fx::Assign::Master);
                c
            },
            booth: AtomicU32::new(1f32.to_bits()),
            metro_tone: AtomicUsize::new(0),
            metro_volume: AtomicU32::new(0.5f32.to_bits()),
            frames: AtomicU32::new(0),
        }
    }

    /// Applies the live knobs of the Preferences ([`super::route::Settings`]): booth level, metronome tone and
    /// volume, preview level. (The channel layout needs the stream reopened and is not a knob.)
    pub fn apply(&self, s: &super::route::Settings) {
        store_clamped(&self.booth, s.booth_gain, MAX_LEVEL, 1.0);
        self.metro_tone
            .store(s.metronome_tone.min(super::metronome::TONES - 1), Relaxed);
        store_clamped(&self.metro_volume, s.metronome_volume, 1.0, 0.5);
        self.preview
            .set_gain(clamped(s.preview_gain, MAX_LEVEL, 1.0));
        self.mic
            .set(s.mic_gain, s.talkover_advanced, s.talkover_gain);
        self.aux.set_duck(s.aux_duck, s.aux_release);
    }

    /// Send amount of channel `ch` (clamped to 0..=1; out-of-range channels are ignored).
    pub fn set_send(&self, ch: usize, amount: f32) {
        if let Some(a) = self.sends.get(ch) {
            store_clamped(a, amount, 1.0, 0.0);
        }
    }

    /// Every channel's send amount.
    pub(super) fn sends(&self) -> [f32; CHANNELS] {
        std::array::from_fn(|k| load_f32(&self.sends[k]))
    }

    /// Booth output gain (linear).
    pub(super) fn booth(&self) -> f32 {
        load_f32(&self.booth)
    }

    /// Metronome (timbre index, linear volume).
    pub(super) fn metronome(&self) -> (usize, f32) {
        (self.metro_tone.load(Relaxed), load_f32(&self.metro_volume))
    }

    pub(super) fn set_frames(&self, n: u32) {
        self.frames.store(n, Relaxed);
    }

    /// Frames per output buffer as the driver delivers them (0 before the first callback).
    pub fn buffer_frames(&self) -> u32 {
        self.frames.load(Relaxed)
    }

    /// Where the headphone feed goes now (it changes when the output device does, see [`Bus::route`]).
    pub fn headphones(&self) -> Headphones {
        self.headphones.lock().clone()
    }

    /// The stream moved to a device running at `rate` with this headphone route: the recorder tap's frames are at the new rate.
    pub(super) fn route(&self, rate: u32, headphones: Headphones) {
        self.tap.set_rate(rate);
        *self.headphones.lock() = headphones;
    }

    /// The post-master mix for a [`super::Recorder`].
    pub fn tap(&self) -> Arc<Tap> {
        self.tap.clone()
    }

    /// Master gain after the channel sum, clamped to 0..=[`MAX_LEVEL`].
    pub fn set_master(&self, gain: f32) {
        store_clamped(&self.master, gain, MAX_LEVEL, 1.0);
    }

    /// Headphone mix (clamped 0..=1; 0 = cue only, 1 = master only) and headphone gain (0..=[`MAX_LEVEL`]).
    pub fn set_headphone(&self, mix: f32, gain: f32) {
        store_clamped(&self.mix, mix, 1.0, 0.0);
        store_clamped(&self.headphone, gain, MAX_LEVEL, 1.0);
    }

    pub fn set_split(&self, on: bool) {
        self.split.store(on, Relaxed);
    }

    /// Peak (max |sample|, either channel) of the summed master, after the master gain, in the last output buffer.
    pub fn master_level(&self) -> f32 {
        load_f32(&self.level)
    }

    pub(super) fn set_level(&self, peak: f32) {
        self.level.store(peak.to_bits(), Relaxed);
    }

    /// Targets the audio thread ramps towards (linear).
    pub(super) fn params(&self) -> Params {
        Params {
            master: load_f32(&self.master),
            split: self.split.load(Relaxed),
            mix: load_f32(&self.mix),
            headphone: load_f32(&self.headphone),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::fx::{Assign, Controls, Effect, Frame, Unit};
    use crate::audio::route::Settings;

    const P: Params = Params {
        master: 1.0,
        split: true,
        mix: 0.0,
        headphone: 1.0,
    };
    const N: usize = 6000;

    type Chans = Vec<Vec<(Frame, Frame)>>;

    fn route(master: [f32; 2], cue: [f32; 2], p: &Params) -> [f32; 2] {
        out(master, cue, p).device
    }

    /// `n` channels of master path 1.0 and cue path 0.7.
    fn chans(n: usize) -> Chans {
        vec![vec![([1.0; 2], [0.7; 2]); N]; n]
    }

    /// A full-level 1-beat gate, switched on (the assignment stays as it is).
    fn trans(c: &Controls) {
        c.set_effect(Effect::Trans);
        c.set_beat(2); // 1/4
        c.set_level(1.0);
        c.set_on(true);
    }

    fn gate(c: &Controls, a: Assign) {
        trans(c);
        c.set_assign(a);
    }

    fn gated(ch: &[(Frame, Frame)]) -> bool {
        ch.iter().any(|f| f.0[0] < 0.05)
    }

    fn run(units: &mut [Unit], controls: &[Controls], buf: &mut Chans) {
        units.iter_mut().zip(controls).for_each(|(u, c)| {
            u.prepare(c.settings().effect);
            u.apply(c, buf, N);
        });
    }

    fn cue_untouched(buf: &Chans) -> bool {
        buf.iter().flatten().all(|f| f.1 == [0.7; 2])
    }

    /// The engine runs `bus.pad_fx[i]` through its own unit: a pad held on one deck changes that channel only,
    /// and both decks can hold a pad at once (each its own effect) without one releasing or retuning the other.
    #[test]
    fn pad_fx_is_one_unit_per_deck_on_its_own_channel() {
        let bus = Bus::new(48_000, Headphones::Split);
        let mut units: Vec<Unit> = (0..CHANNELS).map(|_| Unit::new(48_000)).collect();

        trans(&bus.pad_fx[1]);
        let mut buf = chans(2);
        run(&mut units, &bus.pad_fx, &mut buf);
        assert!(gated(&buf[1]), "deck 2's pad gates deck 2");
        assert_eq!(buf[0], chans(2)[0], "deck 1 is untouched");

        trans(&bus.pad_fx[0]);
        bus.pad_fx[1].set_on(false); // releasing deck 2's pad must not stop deck 1's
        let mut buf = chans(2);
        run(&mut units, &bus.pad_fx, &mut buf);
        assert!(gated(&buf[0]), "deck 1's pad gates deck 1");
        assert!(cue_untouched(&buf), "the cue path is never touched");

        // deck D's pad touches deck D only
        let mut units: Vec<Unit> = (0..CHANNELS).map(|_| Unit::new(48_000)).collect();
        let bus = Bus::new(48_000, Headphones::Split);
        trans(&bus.pad_fx[3]);
        let mut buf = chans(CHANNELS);
        run(&mut units, &bus.pad_fx, &mut buf);
        assert!(
            gated(&buf[3]) && (0..3).all(|k| buf[k] == chans(CHANNELS)[k]),
            "deck 4's pad gates deck 4 only"
        );
    }

    /// The six Beat FX slots (FX1-1 … FX2-3) are independent units: each keeps its own assignment and ON, and switching one
    /// off or re-routing it never touches another; two slots of one unit stack on one channel.
    #[test]
    fn beat_fx_slots_are_independent_and_route_by_assignment() {
        let bus = Bus::new(48_000, Headphones::Split);
        let mut units: Vec<Unit> = (0..FX_UNITS * FX_SLOTS)
            .map(|_| Unit::new(48_000))
            .collect();
        let fx = bus.fx.as_flattened();

        gate(&bus.fx[1][0], Assign::B);
        let mut buf = chans(2);
        run(&mut units, fx, &mut buf);
        assert!(
            gated(&buf[1]) && buf[0] == chans(2)[0],
            "FX2-1 on CH B gates B only; FX1 is off"
        );

        gate(&bus.fx[0][2], Assign::A);
        bus.fx[1][0].set_on(false);
        let mut buf = chans(2);
        run(&mut units, fx, &mut buf);
        assert!(gated(&buf[0]), "FX1-3 on CH A gates A");
        assert!(
            buf[1][1000..].iter().all(|f| f.0[0] > 0.9),
            "FX2-1 off no longer gates B (its ramp-out is done)"
        );
        assert!(cue_untouched(&buf), "the cue path is never touched");

        // FX1-1 and FX1-3 on CH A at once: both run (the 1/4 gate and a 1/8 gate shut more of A than either alone)
        let alone = buf[0].iter().filter(|f| f.0[0] < 0.05).count();
        gate(&bus.fx[0][0], Assign::A);
        bus.fx[0][0].set_beat(1); // 1/8
        let mut buf = chans(2);
        run(&mut units, fx, &mut buf);
        assert!(
            buf[0].iter().filter(|f| f.0[0] < 0.05).count() > alone,
            "two slots stack"
        );
    }

    /// Four decks: a Beat FX can sit on CH3 / CH4 and MASTER still sums all four.
    #[test]
    fn fx_assignment_reaches_decks_c_and_d() {
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 9].map(Assign::from_index),
            [
                Assign::A,
                Assign::B,
                Assign::C,
                Assign::D,
                Assign::Master,
                Assign::Aux,
                Assign::Master
            ]
        );
        for (a, ch) in [(Assign::C, 2), (Assign::D, 3)] {
            let (mut u, c) = (Unit::new(48_000), Controls::default());
            gate(&c, a);
            let mut buf = chans(CHANNELS);
            u.prepare(c.settings().effect);
            u.apply(&c, &mut buf, N);
            assert!(gated(&buf[ch]), "CH{} is gated", ch + 1);
            assert!(
                (0..CHANNELS)
                    .filter(|&k| k != ch)
                    .all(|k| buf[k] == chans(CHANNELS)[k]),
                "the other channels are untouched"
            );
        }
        let (mut u, c) = (Unit::new(48_000), Controls::default());
        gate(&c, Assign::Master);
        let mut buf = chans(CHANNELS);
        u.prepare(c.settings().effect);
        u.apply(&c, &mut buf, N);
        let sum = |i: usize| (0..CHANNELS).map(|k| buf[k][i].0[0]).sum::<f32>();
        assert!(
            (0..N).any(|i| sum(i).abs() < 0.05) && (0..N).any(|i| (sum(i) - 4.0).abs() < 0.05),
            "MASTER gates and opens the sum of four channels (the difference lands on channel A)"
        );
    }

    /// A Beat FX assigned to AUX runs on the AUX channel's buffer only; MASTER sums it with the decks.
    #[test]
    fn a_beat_fx_can_sit_on_the_aux_channel() {
        let (mut u, c) = (Unit::new(48_000), Controls::default());
        gate(&c, Assign::Aux);
        let mut buf = chans(CHANNELS + 1);
        u.prepare(c.settings().effect);
        u.apply(&c, &mut buf, N);
        assert!(gated(&buf[AUX]), "the AUX channel is gated");
        assert!(
            (0..CHANNELS).all(|k| buf[k] == chans(CHANNELS)[k]),
            "the decks are untouched"
        );
        assert!(cue_untouched(&buf), "the cue path is never touched");
        let (mut u, c) = (Unit::new(48_000), Controls::default());
        gate(&c, Assign::Master);
        let mut buf = chans(CHANNELS + 1);
        u.prepare(c.settings().effect);
        u.apply(&c, &mut buf, N);
        let sum = |i: usize| buf.iter().map(|b| b[i].0[0]).sum::<f32>();
        assert!(
            (0..N).any(|i| (sum(i) - 5.0).abs() < 0.05) && (0..N).any(|i| sum(i).abs() < 0.05),
            "MASTER gates and opens the sum of the four decks and the AUX"
        );
    }

    #[test]
    fn split_puts_mono_cue_left_and_mono_master_right() {
        let [l, r] = route([0.5, 0.1], [0.2, 0.4], &P);
        assert!((l - 0.3).abs() < 1e-6, "left = mono cue, got {l}");
        assert!((r - 0.3).abs() < 1e-6, "right = mono master, got {r}");
        // told apart: silent cue leaves the left silent, silent master the right
        assert_eq!(route([0.8, 0.8], [0.0; 2], &P), [0.0, 0.8]);
        assert_eq!(route([0.0; 2], [0.6, 0.6], &P), [0.6, 0.0]);
    }

    #[test]
    fn without_split_the_master_plays_stereo_and_cue_is_silent() {
        let p = Params { split: false, ..P };
        assert_eq!(route([0.5, -0.25], [0.9, 0.9], &p), [0.5, -0.25]);
    }

    #[test]
    fn mix_knob_endpoints_and_constant_power() {
        let (m, c) = ([0.5, 0.5], [0.25, 0.25]);
        assert_eq!(route(m, c, &Params { mix: 0.0, ..P })[0], 0.25); // CUE only
        assert!((route(m, c, &Params { mix: 1.0, ..P })[0] - 0.5).abs() < 1e-6); // MASTER only
        let mid = route(m, c, &Params { mix: 0.5, ..P })[0];
        assert!((mid - (0.25 + 0.5) * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        for i in 0..=20 {
            let [a, b] = equal_power(i as f32 / 20.0);
            assert!((a * a + b * b - 1.0).abs() < 1e-6);
        }
        assert_eq!(equal_power(-1.0), equal_power(0.0));
        assert_eq!(equal_power(2.0), equal_power(1.0));
    }

    #[test]
    fn master_and_headphone_levels_scale_their_own_side() {
        let near = |a: f32, b: f32| assert!((a - b).abs() < 1e-6, "{a} != {b}");
        let p = Params {
            master: 0.5,
            headphone: 0.25,
            ..P
        };
        let [l, r] = route([0.8, 0.8], [0.8, 0.8], &p);
        near(l, 0.2); // cue 0.8 × headphone 0.25
        near(r, 0.4); // master 0.8 × 0.5
        // the master part of the headphone feed is post master gain
        near(
            route(
                [0.8, 0.8],
                [0.0; 2],
                &Params {
                    master: 0.5,
                    mix: 1.0,
                    ..P
                },
            )[0],
            0.4,
        );
        assert_eq!(
            route(
                [0.8, 0.8],
                [0.8, 0.8],
                &Params {
                    master: 0.0,
                    split: false,
                    ..P
                }
            ),
            [0.0, 0.0]
        );
    }

    #[test]
    fn second_device_gets_a_stereo_feed_and_the_recorder_the_post_master_mix() {
        let near = |a: [f32; 2], b: [f32; 2]| {
            assert!(
                a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6),
                "{a:?} != {b:?}"
            )
        };
        let p = Params {
            master: 0.5,
            headphone: 0.5,
            ..P
        };
        let o = out([0.8, 0.4], [0.2, 0.6], &p); // mix 0 = cue only
        near(o.phones, [0.1, 0.3]); // stereo, not mono-summed: L and R differ
        near(o.master, [0.4, 0.2]);
        near(o.device, [0.5 * (0.2 + 0.6) / 2.0, 0.5 * (0.4 + 0.2)]); // split: mono cue left, mono master right
        // the recorder sees the stereo master whatever split does
        near(
            out([0.8, 0.4], [0.0; 2], &Params { split: false, ..p }).master,
            [0.4, 0.2],
        );
        // mix 1 = master only, scaled by the headphone level
        near(
            out([0.8, 0.4], [0.9, 0.9], &Params { mix: 1.0, ..p }).phones,
            [0.2, 0.1],
        );
    }

    #[test]
    fn setters_clamp_and_reject_nan() {
        let b = Bus::new(48000, Headphones::Split);
        b.set_master(9.0);
        b.set_headphone(-1.0, f32::NAN);
        let p = b.params();
        assert_eq!((p.master, p.mix, p.headphone), (MAX_LEVEL, 0.0, 1.0));
        b.set_split(true);
        assert!(b.params().split);
        b.set_level(0.7);
        assert_eq!(b.master_level(), 0.7);
    }

    #[test]
    fn phones_channels_default_to_3_4_on_listed_cards_and_the_env_overrides() {
        let cards = ["ctrl4x".to_owned()];
        let p = |hp, d| phones_channels_in(hp, d, &cards);
        assert_eq!(p(None, "sysdefault:CARD=CTRL4X"), Ok(Some([2, 3])));
        assert_eq!(p(None, "default"), Ok(None));
        assert_eq!(p(Some("5, 6"), "default"), Ok(Some([4, 5])));
        assert_eq!(p(Some("4,3"), "front:CARD=CTRL4X"), Ok(Some([3, 2])));
        for bad in ["", "3", "1,2", "3,3", "3,4,5", "3,x", "0,4"] {
            assert!(
                phones_channels(Some(bad), "default")
                    .unwrap_err()
                    .contains(HP_ENV),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn apply_sets_the_live_knobs_clamped_and_leaves_the_rest() {
        let b = Bus::new(48000, Headphones::Split);
        b.apply(&Settings {
            booth_gain: 0.25,
            metronome_tone: 99,
            metronome_volume: 7.0,
            preview_gain: 0.5,
            ..Settings::default()
        });
        assert_eq!(
            (b.booth(), b.metronome(), b.preview.gain()),
            (0.25, (crate::audio::metronome::TONES - 1, 1.0), 0.5)
        );
        b.apply(&Settings {
            booth_gain: f32::NAN,
            preview_gain: 9.0,
            ..Settings::default()
        });
        assert_eq!((b.booth(), b.preview.gain()), (1.0, MAX_LEVEL));
        assert_eq!(b.buffer_frames(), 0);
        b.set_frames(512);
        assert_eq!(b.buffer_frames(), 512);
    }
}
