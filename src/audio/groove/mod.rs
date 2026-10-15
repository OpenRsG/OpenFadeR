//! BEAT LAB on the audio side: DRUM SWAP slots, DRUM CAPTURE and the DRUM RELEASE FX knobs of one deck.
//!
//! Each deck has [`SLOTS`] DRUM SWAP SLOTs (two DRUM LOOP BANKs of [`PER_BANK`]). A slot holds a drum loop audio source ([`Drum`]: audio and
//! its tempo; at most [`MAX_BEATS`] = 32 beats play, a longer source loops from its start by 32 beats). [`Controls::press`]
//! plays it, a second press stops it; it starts on the deck's next beat and from then on is tied to the deck's beat grid
//! ([`phase`]), time-stretched to the deck's current tempo with the pitch kept ([`crate::audio::stretch`], like the deck's key lock and the
//! sampler's BPM SYNC; at exactly the source tempo the loop is read sample-exact). Playback stops by itself when the deck stops, plays
//! backwards, is scratched or braked, or the position jumps off the beat grid (a manual loop or a seek with quantize off);
//! quantized loops, hot cues and beat jumps keep it playing. The same slot of BANK A and BANK B never plays together.
//!
//! SINGLE mode (the default): one slot at a time, and while it plays the deck's DRUMS stem is off ([`Controls::swapping`] makes the player
//! mute it, ramped; it returns when the slot stops). MULTI mode: several slots at once, the DRUMS stem stays on. DRUM RELEASE FX
//! ([`Controls::release`]): while held, an effect runs on the DRUMS stem of the deck (the player's stem-part unit) and, in SINGLE mode, on the
//! playing loop (the [`Voice`]'s own unit). DRUM CAPTURE ([`capture`]) cuts a range of beats out of the DRUMS stem (or the
//! original track) of the deck's audio for a slot.
//!
//! Threads: the UI sets atomics in [`Controls`] and hands a new loop over through the slot's [`Handover`] (the callback neither allocates,
//! frees nor blocks). Children: [`phase`] (beat clock), [`voice`] (the callback's playback), [`capture`].

pub mod capture;
mod phase;
#[cfg(test)]
mod tests;
mod voice;

use super::fx::{self, Assign, Effect, Settings};
use super::handover::Handover;
use super::{DRUMS, Pcm, load_f32, store_clamped as put};
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU32,
    Ordering::{Acquire, Relaxed, Release},
};

pub(super) use voice::{Link, Voice};

/// DRUM LOOP BANKs A and B, four slots each.
pub const BANKS: usize = 2;
pub const PER_BANK: usize = 4;
pub const SLOTS: usize = BANKS * PER_BANK;
/// Longest loop that plays, in beats.
pub const MAX_BEATS: u32 = 32;
/// Tempo range a source may have (the effects' own range).
const BPM_RANGE: (f32, f32) = (20.0, 400.0);

/// The effects DRUM RELEASE FX offers (the unit is a Beat FX unit).
pub const RELEASE_FX: [Effect; 6] = [
    Effect::Echo,
    Effect::Reverb,
    Effect::Delay,
    Effect::Roll,
    Effect::Lpf,
    Effect::Hpf,
];
pub const RELEASE_FX_NAMES: [&str; 6] = ["ECHO", "REVERB", "DELAY", "ROLL", "LPF", "HPF"];

/// A drum loop audio source: its audio, its tempo, and how many beats of it play (its length at that tempo, 1..=[`MAX_BEATS`]).
pub struct Drum {
    pub pcm: Pcm,
    pub bpm: f32,
    pub beats: u32,
}

impl Drum {
    /// `pcm` at `bpm` BPM; an empty source or a tempo outside [`BPM_RANGE`] is an error. Only the first [`MAX_BEATS`] beats are kept.
    pub fn new(mut pcm: Pcm, bpm: f32) -> Result<Drum, String> {
        if pcm.rate == 0 || pcm.samples.len() < 2 {
            return Err("the drum loop is empty".into());
        }
        if !(BPM_RANGE.0..=BPM_RANGE.1).contains(&bpm) {
            return Err(format!("{bpm} BPM is not a usable tempo for a drum loop"));
        }
        let secs = (pcm.samples.len() / 2) as f64 / pcm.rate as f64;
        let beats = (secs * bpm as f64 / 60.0)
            .round()
            .clamp(1.0, MAX_BEATS as f64) as u32;
        let keep = (MAX_BEATS as f64 * 60.0 / bpm as f64 * pcm.rate as f64).ceil() as usize + 1;
        pcm.samples.truncate(2 * keep);
        Ok(Drum { pcm, bpm, beats })
    }

    /// Source frames in one beat.
    pub fn frames_per_beat(&self) -> f64 {
        self.pcm.rate as f64 * 60.0 / self.bpm as f64
    }
}

/// What a slot is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    /// Pressed: starts on the deck's next beat.
    Waiting,
    Playing,
}

impl State {
    fn from(v: u8) -> State {
        [State::Idle, State::Waiting, State::Playing][(v as usize).min(2)]
    }
}

struct Slot {
    /// The loaded loop (`None` empties the slot), for [`Controls::drum`].
    drum: Handover<Drum>,
    /// Presses and stops asked for so far (the callback compares with its own counts).
    presses: AtomicU32,
    stops: AtomicU32,
    gain: AtomicU32,
    /// Written by the callback: [`State`] and the position in the loop, 0..1 (f32 bits).
    state: AtomicU8,
    at: AtomicU32,
}

impl Slot {
    fn new() -> Slot {
        Slot {
            drum: Handover::default(),
            presses: AtomicU32::new(0),
            stops: AtomicU32::new(0),
            gain: AtomicU32::new(1f32.to_bits()),
            state: AtomicU8::new(0),
            at: AtomicU32::new(0),
        }
    }
}

/// Everything the UI sets for one deck (all lock-free except the loop hand-over); read by the audio thread each buffer.
pub struct Controls {
    slots: [Slot; SLOTS],
    /// Stops every slot requested so far.
    halts: AtomicU32,
    multi: AtomicBool,
    master: AtomicU32,
    /// Written by the callback: a SINGLE-mode loop is playing, so the deck's DRUMS stem is off.
    swapping: AtomicBool,
    /// DRUM RELEASE FX: a Beat FX unit's knobs limited to this deck's DRUMS stem (effect, beats, level, on = held).
    pub release: fx::Controls,
}

impl Default for Controls {
    fn default() -> Controls {
        let release = fx::Controls::default();
        release.set_part(1 << DRUMS);
        Controls {
            slots: std::array::from_fn(|_| Slot::new()),
            halts: AtomicU32::new(0),
            multi: AtomicBool::new(false),
            master: AtomicU32::new(1f32.to_bits()),
            swapping: AtomicBool::new(false),
            release,
        }
    }
}

impl Controls {
    fn with(&self, i: usize, f: impl FnOnce(&Slot)) {
        if let Some(s) = self.slots.get(i) {
            f(s);
        }
    }

    /// Hands `drum` to slot `i` (it stops whatever the slot was playing); `None` empties the slot.
    pub fn load(&self, i: usize, drum: Option<Drum>) {
        self.with(i, |s| s.drum.load(drum.map(Arc::new)));
    }

    /// Slot `i`'s loaded loop (shared, not copied); `None` when the slot is empty.
    pub fn drum(&self, i: usize) -> Option<Arc<Drum>> {
        self.slots.get(i)?.drum.kept()
    }

    /// The PLAY button of slot `i`: starts it on the deck's next beat, or stops it when it is waiting or playing. Ignored while the deck is not
    /// playing forward.
    pub fn press(&self, i: usize) {
        self.with(i, |s| {
            s.presses.fetch_add(1, Release);
        });
    }

    /// Stops slot `i` (fade-out); an idle slot ignores it.
    pub fn stop(&self, i: usize) {
        self.with(i, |s| {
            s.stops.fetch_add(1, Release);
        });
    }

    pub fn stop_all(&self) {
        self.halts.fetch_add(1, Release);
    }

    /// DRUM SWAP SLOT GAIN of slot `i`, linear 0..=2 (non-finite = 1).
    pub fn set_gain(&self, i: usize, v: f32) {
        self.with(i, |s| put(&s.gain, v, 2.0, 1.0));
    }

    pub fn gain(&self, i: usize) -> f32 {
        self.slots.get(i).map_or(1.0, |s| load_f32(&s.gain))
    }

    /// DRUM SWAP GAIN: the volume of all slots, linear 0..=2 (non-finite = 1).
    pub fn set_master(&self, v: f32) {
        put(&self.master, v, 2.0, 1.0);
    }

    fn master(&self) -> f32 {
        load_f32(&self.master)
    }

    /// MULTI (true) or SINGLE (false) playback mode.
    pub fn set_multi(&self, on: bool) {
        self.multi.store(on, Relaxed);
    }

    pub fn multi(&self) -> bool {
        self.multi.load(Relaxed)
    }

    /// What slot `i` was doing at the end of the last buffer.
    pub fn state(&self, i: usize) -> State {
        self.slots
            .get(i)
            .map_or(State::Idle, |s| State::from(s.state.load(Relaxed)))
    }

    /// Where slot `i` is in its loop, 0..1.
    pub fn position(&self, i: usize) -> f32 {
        self.slots.get(i).map_or(0.0, |s| load_f32(&s.at))
    }

    /// A SINGLE-mode loop is playing: the deck's DRUMS stem is off meanwhile.
    pub fn swapping(&self) -> bool {
        self.swapping.load(Acquire)
    }

    /// The DRUM RELEASE FX as a stem-part target of deck channel `ch` (always `Some`: the unit idles while it is not held).
    pub(super) fn release_target(&self, ch: usize) -> Option<(u8, Settings)> {
        self.release.set_assign(Assign::from_index(ch));
        self.release.stem_target(ch)
    }
}
