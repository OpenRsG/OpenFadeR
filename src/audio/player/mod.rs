//! The mixer channel's public face: [`Player`] and the state it shares with the audio thread.
//!
//! `Player` (UI thread) sets atomics in [`Shared`]; the audio callback reads them. A FADER plugin curve ([`Player::fader`])
//! replaces the channel's fader gain (see [`voice`]). Children: [`clock`]
//! (latency-compensated position clock), [`voice`] (one channel's render: varispeed or WSOLA, EQ, filter,
//! gain, key shift / reverse / brake / scratch / nudge), [`scratch`] (jog scratch filter), [`mix`] (the audio
//! callback and the bus mix), [`engine`] (the one cpal stream all `Player`s share and its lifecycle) and [`output`]
//! (which device, rate, buffer and channel layout the next stream gets, and what the last choice was).
//! Children reach each other only through the imports below.

mod clock;
mod engine;
#[cfg(test)]
mod hotcue_tests;
mod mix;
mod output;
mod scratch;
#[cfg(test)]
mod tests;
mod voice;

use super::bus::Bus;
use super::cfx;
use super::dsp;
use super::fx;
use super::groove;
use super::metronome::{self, Hits};
use super::transport::Transport;
use super::{clamped_in, load_f32, store_clamped, Pcm, Stems, Track16, STEM_COUNT};
use clock::{audible, heard_frames, Anchor, Clock};
pub use engine::{configure, live_bus, set_output};
use engine::{Engine, ENGINE};
pub use output::{controller, release_controller, status, Status};
use parking_lot::Mutex;
use std::rc::{Rc, Weak};
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU32, AtomicU64,
    Ordering::{Relaxed, Release},
};
use std::sync::Arc;
use std::time::Instant;
use voice::{Done, Voice, render};

const NO_SEEK: u64 = u64::MAX;
/// Playback rate limits: 0 (the bottom of the WIDE tempo range, the deck stands still) to double speed.
const RATE_RANGE: (f64, f64) = (0.0, 2.0);
/// Largest nudge offset added to the rate (either way).
const NUDGE_MAX: f32 = 0.25;
const MAX_GAIN: f32 = 4.0;

/// Source frames per output frame.
fn step(src_rate: f64, rate: f64, dev_rate: u32) -> f64 {
    rate * src_rate / dev_rate as f64
}

/// Gain of output frame `i` of `n`: linear ramp from the previous buffer's gain `g0`, reaching `g1` on the last frame.
fn gain_at(g0: f32, g1: f32, i: usize, n: usize) -> f32 {
    g0 + (g1 - g0) * ((i + 1) as f32 / n as f32)
}

/// A loop's ms are kept to 1/`LOOP_STEPS` ms (4.6 hours of track fit 32 bits): a loop of whole beats keeps its exact length, whole ms
/// would make it up to a ms per pass too long or short.
const LOOP_STEPS: f64 = 256.0;

/// Loop `(start_ms, end_ms)` packed into one atomic (start << 32 | end, in 1/`LOOP_STEPS` ms); 0 = none (a loop has end > start, so never 0).
fn pack(l: Option<(f64, f64)>) -> u64 {
    let steps = |ms: f64| (ms * LOOP_STEPS).round() as u32;
    l.map(|(s, e)| (steps(s), steps(e)))
        .filter(|&(s, e)| e > s)
        .map_or(0, |(s, e)| (s as u64) << 32 | e as u64)
}

fn unpack(v: u64) -> Option<(f64, f64)> {
    let ms = |steps: u32| steps as f64 / LOOP_STEPS;
    (v != 0).then_some((ms((v >> 32) as u32), ms(v as u32)))
}

/// A beat grid for a seek as one word (two f32, so a request cannot tear): `len` source frames per beat, and `start`, a frame a beat
/// starts at, reduced to its offset into a beat.
fn pack_beat(start: f64, len: f64) -> u64 {
    let bits = |x: f64| (x as f32).to_bits() as u64;
    bits(start.rem_euclid(len)) << 32 | bits(len)
}

/// `(offset, len)` of [`pack_beat`] in frames; `None` for NO_SEEK.
fn unpack_beat(v: u64) -> Option<(f64, f64)> {
    let f = |bits: u32| f32::from_bits(bits) as f64;
    (v != NO_SEEK).then(|| (f((v >> 32) as u32), f(v as u32)))
}

struct Shared {
    /// Shared (not copied) with the stems job and the stem waveform.
    pcm: Mutex<Arc<Track16>>,
    /// The separated parts of `pcm` (same rate and length), `None` until a separation finishes. Taken with `try_lock` by the callback.
    stems: Mutex<Option<Arc<Stems>>>,
    /// BEAT LAB: this deck's DRUM SWAP slots and DRUM RELEASE FX knobs ([`groove`]).
    groove: groove::Controls,
    stem: [AtomicU32; STEM_COUNT], // f32 bits, target gain 0..=1 of each stem (voice ramps toward it)
    pos: AtomicU64,                // f64 bits, source-frame position (written by the callback)
    seek: AtomicU64,               // f64 bits or NO_SEEK, consumed by the callback
    /// f64 bits or NO_SEEK: the heard position (source frames) `seek` was worked out at ([`Player::seek_in_phase`]).
    seek_from: AtomicU64,
    /// [`pack_beat`] or NO_SEEK: the beat grid a seek is kept on ([`Player::seek_on_beat`]), consumed with `seek`.
    seek_beat: AtomicU64,
    /// f32 bits: a loop's length in frames the landing of a seek on the beat wraps in (0 = none), consumed with `seek_beat`.
    seek_wrap: AtomicU32,
    playing: AtomicBool,
    rate: AtomicU64,         // f64 bits, varispeed
    nudge: AtomicU32,        // f32 bits, temporary offset added to the rate (jog bend)
    scratch: AtomicBool, // platter held: the callback follows `scratch_ns` instead of the transport
    scratch_ns: AtomicI64, // track nanoseconds moved by the platter since the callback last looked
    lp: AtomicU64,       // pack()ed loop, track ms
    gain: AtomicU32,     // f32 bits, target (master path: trim × fader × crossfade)
    gain_applied: AtomicU32, // f32 bits, gain at the end of the last buffer (callback only)
    cue: AtomicU32,      // f32 bits, target of the cue path (trim while CUE is on, else 0)
    cue_applied: AtomicU32, // f32 bits, cue gain at the end of the last buffer (callback only)
    eq: [AtomicU32; 3],  // f32 bits, dB per band (low, mid, high)
    cfx: cfx::Controls, // COLOR FX type, knob (-1..=1; the filter's while the type is FILTER) and tempo
    /// FADER plugin curve ([`fx::Unit::rack`] of slot Fader): on = a plugin curve is selected, index = its position among them,
    /// knob = the raw fader position 0..=1. The voice runs it before the gain, which then holds trim and crossfader only.
    fader: fx::Controls,
    key_lock: AtomicBool,
    cut: AtomicBool, // JOG CUTTER gate: channel cut (ramped per sample, see voice::CUT_S)
    t: Transport,    // key shift, reverse, slip, brake
    level: AtomicU32, // f32 bits, post-gain peak of the last buffer (callback only)
    /// The callback only `try_lock`s it (a missed buffer costs one anchor, see `mix`); `position_ms` locks it.
    clock: Mutex<Clock>,
    /// External mixer mode: the channel's output is the raw track (no EQ, filter, COLOR FX, fader or crossfader);
    /// set when the stream opens.
    raw: AtomicBool,
    /// The loaded track's sample rate (0 = none), for placing the metronome's beats.
    src_rate: AtomicU32,
    /// Metronome on this channel and the grid it clicks on (beat times in track ms, ascending). The callback only
    /// `try_lock`s the list; the replaced one is dropped by the UI thread.
    metro: AtomicBool,
    beats: Mutex<Option<Arc<[u32]>>>,
    /// Vinyl speed adjust ([`Player::set_vinyl`]): touch ramp and release ramp seconds (f32 bits, 0 = instant), and a
    /// pending speed-up request (seconds, consumed by the callback while the channel plays).
    vinyl_touch: AtomicU32,
    vinyl_release: AtomicU32,
    spin: AtomicU32,
    /// No brake and no spin-up ramp ran in the last buffer: the deck plays at its tempo (callback writes, [`Player::steady`]).
    steady: AtomicBool,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            pcm: Mutex::new(Arc::default()),
            stems: Mutex::new(None),
            groove: groove::Controls::default(),
            stem: [0; STEM_COUNT].map(|_| AtomicU32::new(1f32.to_bits())),
            pos: AtomicU64::new(0),
            seek: AtomicU64::new(NO_SEEK),
            seek_from: AtomicU64::new(NO_SEEK),
            seek_beat: AtomicU64::new(NO_SEEK),
            seek_wrap: AtomicU32::new(0),
            steady: AtomicBool::new(true),
            playing: AtomicBool::new(false),
            rate: AtomicU64::new(1f64.to_bits()),
            nudge: AtomicU32::new(0f32.to_bits()),
            scratch: AtomicBool::new(false),
            scratch_ns: AtomicI64::new(0),
            lp: AtomicU64::new(0),
            gain: AtomicU32::new(1f32.to_bits()),
            gain_applied: AtomicU32::new(1f32.to_bits()),
            cue: AtomicU32::new(0f32.to_bits()),
            cue_applied: AtomicU32::new(0f32.to_bits()),
            eq: [0, 1, 2].map(|_| AtomicU32::new(0f32.to_bits())),
            cfx: cfx::Controls::default(),
            fader: {
                let c = fx::Controls::default();
                c.set_level(1.0); // a fader effect gets the whole effect, never a blend
                c
            },
            key_lock: AtomicBool::new(false),
            cut: AtomicBool::new(false),
            t: Transport::new(),
            level: AtomicU32::new(0f32.to_bits()),
            clock: Mutex::new(Clock::new()),
            raw: AtomicBool::new(false),
            src_rate: AtomicU32::new(0),
            metro: AtomicBool::new(false),
            beats: Mutex::new(None),
            vinyl_touch: AtomicU32::new(0),
            vinyl_release: AtomicU32::new(0),
            spin: AtomicU32::new(0),
        }
    }

    fn level(&self) -> f32 {
        load_f32(&self.level)
    }

    /// Loop in track ms (to 1/[`LOOP_STEPS`] ms). Under slip, setting one diverges from the shadow, clearing the last manipulation returns to it.
    fn set_loop(&self, l: Option<(f64, f64)>) {
        let p = pack(l);
        self.lp.store(p, Relaxed);
        if p != 0 {
            self.t.diverge()
        } else {
            self.t.release(false)
        }
    }

    fn set_reverse(&self, on: bool) {
        self.t.set_reverse(on);
        if on {
            self.t.diverge()
        } else {
            self.t.release(self.lp.load(Relaxed) != 0)
        }
    }

    /// Seek to source frame `f`; made while playing under slip it diverges from the shadow.
    fn seek_to(&self, f: f64) {
        self.seek_from_heard(f, None);
    }

    /// [`Shared::seek_to`]; with `heard` (the source frame heard when `f` was worked out) the callback lands as much past `f`
    /// as the deck plays on before the jump: the output latency plus the wait for the next buffer. A seek still waiting was
    /// worked out from where the first one was heard, which stays the reference: `heard` is then its target, not what is heard.
    fn seek_from_heard(&self, f: f64, heard: Option<f64>) {
        if self.playing.load(Relaxed) {
            self.t.diverge();
        }
        if heard.is_none() || self.seek.load(Relaxed) == NO_SEEK {
            self.seek_from
                .store(heard.map_or(NO_SEEK, f64::to_bits), Relaxed);
        }
        self.seek_beat.store(NO_SEEK, Relaxed);
        self.seek.store(f.to_bits(), Release);
    }

    /// Seek to source frame `f` plus how far into its beat the written playhead is when the callback takes the seek: the grid has
    /// a beat of `len` frames starting at `start` ([`Voice`]'s `render`); a `wrap` > 0 (a loop's length in frames) wraps that
    /// phase round the loop the seek lands in. Whatever the press timing, the jump continues the beat phase of the frames the
    /// deck has written, so it is never a beat early or late. A later seek replaces it whole.
    fn seek_on_beat(&self, f: f64, start: f64, len: f64, wrap: f64) {
        if self.playing.load(Relaxed) {
            self.t.diverge();
        }
        self.seek_from.store(NO_SEEK, Relaxed);
        self.seek_wrap.store((wrap as f32).to_bits(), Relaxed);
        self.seek_beat.store(pack_beat(start, len), Relaxed);
        self.seek.store(f.to_bits(), Release);
    }

    /// Platter touched: scratching starts at the current position, paused or playing. Made while playing under slip it diverges from the shadow.
    fn scratch_begin(&self) {
        if self.playing.load(Relaxed) {
            self.t.diverge();
        }
        self.scratch_ns.store(0, Relaxed);
        self.scratch.store(true, Release);
    }

    /// Platter moved by `secs` of track time (negative = backwards); ignored unless scratching.
    fn scratch_move(&self, secs: f64) {
        if secs.is_finite() && self.scratch.load(Relaxed) {
            self.scratch_ns.fetch_add((secs * 1e9) as i64, Relaxed);
        }
    }

    /// Platter released: the transport (rate, direction, key lock) carries on; under slip the playhead returns to the shadow.
    fn scratch_end(&self) {
        if self.scratch.swap(false, Release) {
            self.t.release(self.lp.load(Relaxed) != 0);
            if self.playing.load(Relaxed) {
                self.request_spin(); // the platter let go of a playing deck: it spins up again (vinyl speed adjust)
            }
        }
    }

    /// Plays; `ramp`: a stopped deck takes the start ramp ([`Shared::request_spin`]). A PAUSE brake still running is cancelled.
    fn play(&self, ramp: bool) {
        self.t.cancel_brake();
        if ramp && !self.playing.load(Relaxed) {
            self.request_spin();
        }
        self.playing.store(true, Relaxed);
    }

    /// Asks the callback to ramp the speed factor from 0 to 1 over the release time of [`Player::set_vinyl`] (no-op when it is 0).
    fn request_spin(&self) {
        let s = self.vinyl_release.load(Relaxed);
        if f32::from_bits(s) > 0.0 {
            self.spin.store(s, Relaxed);
        }
    }

    /// The metronome clicks of this buffer (see [`metronome::schedule`]); none while it is off or the track is stopped.
    fn beat_hits(&self, d: &Done, dev_rate: u32, n: usize) -> Hits {
        let rate = self.src_rate.load(Relaxed);
        if !self.metro.load(Relaxed) || !d.playing || rate == 0 {
            return Hits::NONE;
        }
        match self.beats.try_lock().as_deref() {
            Some(Some(beats)) => {
                metronome::schedule(beats, rate as f64, d.start, d.fps, dev_rate as f64, n, d.lp)
            }
            _ => Hits::NONE,
        }
    }
}

pub struct Player {
    shared: Arc<Shared>,
    engine: Rc<Engine>, // keeps the stream alive; !Send on some platforms
    src_rate: u32,
    frames: u64,
    has_stems: bool,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.pause(); // its channel stays allocated, see `Engine::claimed`
    }
}

impl Player {
    /// Takes the next mixer channel (the first `Player` is channel 0) of the shared output stream.
    pub fn new() -> Result<Player, String> {
        let engine = match ENGINE.with_borrow(Weak::upgrade) {
            Some(e) => e,
            None => {
                let e = Rc::new(Engine::open()?);
                ENGINE.set(Rc::downgrade(&e));
                e
            }
        };
        let slot = engine.claimed.get();
        let shared = engine
            .shared
            .get(slot)
            .ok_or("every mixer channel is in use")?
            .clone();
        engine.claimed.set(slot + 1);
        Ok(Player {
            shared,
            engine,
            src_rate: 0,
            frames: 0,
            has_stems: false,
        })
    }

    /// Master/headphone knobs shared by every `Player` of this stream.
    pub fn bus(&self) -> Arc<Bus> {
        self.engine.bus.clone()
    }

    /// Cue (headphone) feed gain of this channel: the post-EQ, post-filter, pre-fader signal × `gain`
    /// (the mixer passes trim while CUE is on and 0 while off), clamped to 0.0..=`MAX_GAIN`, ramped across the next buffer.
    pub fn set_cue_gain(&mut self, gain: f32) {
        store_clamped(&self.shared.cue, gain, MAX_GAIN, 0.0);
    }

    /// Replaces the track: pauses, seeks to 0, clears the loop, the reverse and any slip divergence.
    /// Rate, key shift, slip mode and volume persist.
    pub fn load(&mut self, pcm: Pcm) {
        self.load_shared(Arc::new(Track16::from(pcm)));
    }

    /// Installs prepared audio without copying or quantising it on the caller's thread.
    pub fn load_shared(&mut self, pcm: Arc<Track16>) {
        self.pause();
        self.shared.scratch.store(false, Relaxed);
        self.shared.lp.store(0, Relaxed);
        self.shared.t.reset();
        self.src_rate = pcm.rate;
        self.shared.src_rate.store(pcm.rate, Relaxed);
        drop(self.shared.beats.lock().take()); // the grid belongs to the old track
        self.frames = pcm.frames() as u64;
        self.shared.seek_beat.store(NO_SEEK, Relaxed); // a seek of the old track's grid is not this one's
        self.shared.seek.store(0f64.to_bits(), Relaxed);
        self.shared.pos.store(0f64.to_bits(), Relaxed);
        let old_stems = self.shared.stems.lock().take();
        let old = std::mem::replace(&mut *self.shared.pcm.lock(), pcm);
        self.has_stems = false;
        drop((old, old_stems)); // free outside the lock path of the audio thread
    }

    /// The loaded track, shared (the stems job and the stem waveform read it without a copy).
    pub fn pcm(&self) -> Arc<Track16> {
        self.shared.pcm.lock().clone()
    }

    /// Stems of the loaded track ([`Stems`]: same rate and length as the audio, else an error); `None` drops them.
    /// Replaced by [`Player::load`]. The gains ([`Player::set_stem_gain`]) apply at once, ramped, so a mute set while
    /// the separation was running takes effect when the stems arrive.
    pub fn set_stems(&mut self, stems: Option<Arc<Stems>>) -> Result<(), String> {
        if let Some(s) = &stems {
            if s.frames() as u64 != self.frames {
                return Err(format!(
                    "stems have {} frames, the track {}",
                    s.frames(),
                    self.frames
                ));
            }
        }
        self.has_stems = stems.is_some();
        let old = std::mem::replace(&mut *self.shared.stems.lock(), stems);
        drop(old);
        Ok(())
    }

    pub fn has_stems(&self) -> bool {
        self.has_stems
    }

    /// The deck's DRUM SWAP slots, banks and DRUM RELEASE FX ([`groove::Controls`]).
    pub fn groove(&self) -> &groove::Controls {
        &self.shared.groove
    }

    /// The separated parts of the loaded track, if it has them ([`Player::set_stems`]); DRUM CAPTURE reads the DRUMS stem from it.
    pub fn stems(&self) -> Option<Arc<Stems>> {
        self.shared.stems.lock().clone()
    }

    /// Gain 0.0..=1.0 of stem `stem` ([`super::VOCAL`]…); ramped over 10 ms, so muting never clicks. Without stems it is stored and has no effect yet.
    pub fn set_stem_gain(&mut self, stem: usize, gain: f32) {
        if let Some(a) = self.shared.stem.get(stem) {
            store_clamped(a, gain, 1.0, 1.0);
        }
    }

    pub fn stem_gain(&self, stem: usize) -> f32 {
        self.shared.stem.get(stem).map_or(1.0, load_f32)
    }

    /// Starts playing at full speed (a hot cue, CUE, fader start, synced start); a PAUSE brake still running is cancelled.
    pub fn play(&mut self) {
        self.launch(false);
    }

    /// The PLAY button: [`Player::play`], with the vinyl start ramp ([`Player::set_vinyl`]) when the deck was stopped.
    pub fn start(&mut self) {
        self.launch(true);
    }

    fn launch(&mut self, ramp: bool) {
        if self.frames == 0 {
            return;
        }
        let (pos, frames) = (self.pos_frames(), self.frames as f64);
        if self.reverse() {
            if pos <= 0.0 || pos >= frames {
                self.shared.seek.store((frames - 1.0).to_bits(), Relaxed); // reversed to the start: restart from the end
            }
        } else if pos >= frames {
            self.shared.seek.store(0f64.to_bits(), Relaxed); // finished track: restart
        }
        self.shared.play(ramp);
    }

    pub fn pause(&mut self) {
        self.shared.playing.store(false, Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Relaxed)
    }

    /// Varispeed playback speed (1.0 = normal; pitch follows), clamped to [0.5, 2.0]. Takes effect at
    /// the next buffer; `position_ms` stays continuous (the clock switches when the new rate is audible).
    pub fn set_rate(&mut self, rate: f64) {
        let r = if rate.is_finite() {
            rate.clamp(RATE_RANGE.0, RATE_RANGE.1)
        } else {
            1.0
        };
        self.shared.rate.store(r.to_bits(), Relaxed);
    }

    /// Temporary pitch bend: `rate_offset` (clamped to ±0.25, 0 = none) is added to the rate while set, for audio and
    /// position clock; the tempo fader value ([`Self::rate`]) and the slip shadow's tempo are untouched.
    pub fn nudge(&mut self, rate_offset: f32) {
        let o = clamped_in(rate_offset, -NUDGE_MAX, NUDGE_MAX, 0.0);
        self.shared.nudge.store(o.to_bits(), Relaxed);
    }

    /// Platter touched (jog top / scratch): follow the platter instead of the transport, paused or playing.
    /// See [`scratch`]; slip diverges from the shadow when playing.
    pub fn scratch_begin(&mut self) {
        self.shared.scratch_begin();
    }

    /// Platter motion in seconds of track time (negative = backwards), accumulated until the next buffer; ignored unless scratching.
    /// The playhead lands on the target within a few ms (critically damped chase).
    pub fn scratch_move(&mut self, delta_seconds: f64) {
        self.shared.scratch_move(delta_seconds);
    }

    /// Platter released: playing decks resume at the normal rate/direction/key lock from where the playhead is, paused ones stay put; under slip the playhead returns to the shadow.
    pub fn scratch_end(&mut self) {
        self.shared.scratch_end();
    }

    pub fn is_scratching(&self) -> bool {
        self.shared.scratch.load(Relaxed)
    }

    pub fn rate(&self) -> f64 {
        f64::from_bits(self.shared.rate.load(Relaxed))
    }

    /// Loop `[start_ms, end_ms)` in track time (to 1/256 ms: a beat loop keeps its length off the whole ms); the audio callback
    /// wraps sample-accurately. `None`, or an empty/reversed range, clears it.
    pub fn set_loop(&mut self, l: Option<(f64, f64)>) {
        self.shared.set_loop(l);
    }

    /// The loop in whole ms.
    pub fn looping(&self) -> Option<(u32, u32)> {
        unpack(self.shared.lp.load(Relaxed)).map(|(s, e)| (s.round() as u32, e.round() as u32))
    }

    /// Linear gain, clamped to 0.0..=`MAX_GAIN` (4.0, +12 dB; above 1.0 the device clips); ramped linearly across the next buffer.
    pub fn set_volume(&mut self, gain: f32) {
        store_clamped(&self.shared.gain, gain, MAX_GAIN, 1.0);
    }

    /// Frames written (or the pending seek target): the resting position.
    fn pos_frames(&self) -> f64 {
        let s = self.shared.seek.load(Relaxed);
        f64::from_bits(if s != NO_SEEK {
            s
        } else {
            self.shared.pos.load(Relaxed)
        })
    }

    /// Isolator EQ in dB per band (low ~200 Hz shelf, mid ~1 kHz peak, high ~4 kHz shelf), each clamped
    /// to [`EQ_KILL_DB`, `EQ_MAX_DB`]; `EQ_KILL_DB` (-26) is a full kill. Ramped across the next buffer.
    pub fn set_eq(&mut self, low_db: f32, mid_db: f32, high_db: f32) {
        for (a, db) in self.shared.eq.iter().zip([low_db, mid_db, high_db]) {
            a.store(dsp::clamp_db(db).to_bits(), Relaxed);
        }
    }

    /// The channel's COLOR FX knob, type and tempo ([`cfx::Controls`]); with the type FILTER the knob is the
    /// one-knob filter (-1..=1: 0 off, negative low-pass sweep, positive high-pass sweep).
    pub fn cfx(&self) -> &cfx::Controls {
        &self.shared.cfx
    }

    /// The channel's FADER plugin curve ([`fx::Controls`]: `set_on`, `set_index`, `set_knob` = fader position 0..=1). While on,
    /// the plugin applies the fader gain, so [`Player::set_volume`] should hold trim and crossfader only.
    pub fn fader(&self) -> &fx::Controls {
        &self.shared.fader
    }

    /// Key lock: at rate != 1 the tempo changes without the pitch (WSOLA, see [`stretch`]). Off = varispeed.
    pub fn set_key_lock(&mut self, on: bool) {
        self.shared.key_lock.store(on, Relaxed);
    }

    pub fn key_lock(&self) -> bool {
        self.shared.key_lock.load(Relaxed)
    }

    /// Key shift in semitones, clamped to -12..=12, independent of tempo and of key lock: the output
    /// pitch is `2^(st/12)` times the source's (times `rate` too when key lock is off). Takes effect
    /// grain by grain (cross-faded), so it can be swept. Zero with key lock off is plain varispeed.
    pub fn set_key_shift(&mut self, semitones: f32) {
        self.shared.t.set_shift(semitones);
    }

    pub fn key_shift(&self) -> f32 {
        self.shared.t.shift()
    }

    /// Slip mode: while on, a shadow playhead runs at normal speed; loops, reverse and seeks made while
    /// playing diverge from it and the playhead returns to it on release (see [`super::transport`]).
    pub fn set_slip(&mut self, on: bool) {
        self.shared.t.set_slip(on);
    }

    pub fn slip(&self) -> bool {
        self.shared.t.slip()
    }

    /// Shadow playhead in seconds while slip is on (equals the playhead until something diverges).
    pub fn slip_position(&self) -> Option<f64> {
        self.shared
            .t
            .shadow()
            .filter(|_| self.src_rate != 0)
            .map(|f| f / self.src_rate as f64)
    }

    /// Release a hot cue / beat jump / seek made in slip mode: the playhead moves to the shadow at the
    /// next buffer. No-op unless slip is on and diverged. Loop end and reverse off do this by themselves.
    pub fn slip_return(&mut self) {
        self.shared.t.slip_return();
    }

    /// Slip off once the playhead is back on the shadow: for a REVERSE that engaged slip itself, after `set_reverse(false)`
    /// asked for the return. With a loop still set nothing returns and slip goes off at once.
    pub fn slip_off_after_return(&mut self) {
        if self.looping().is_some() {
            self.shared.t.set_slip(false);
        } else {
            self.shared.t.slip_off_after_return();
        }
    }

    /// The playhead was put somewhere on purpose while paused (CUE, gate cue): slip has nothing to return to.
    pub fn slip_settle(&mut self) {
        self.shared.t.settle();
    }

    /// JOG CUTTER gate: `true` cuts the channel (post-source, pre-EQ/FX, so effect tails ring on), `false` opens it.
    /// Ramps over ~1 ms per sample, so no click; the playhead keeps running.
    pub fn set_cut(&mut self, on: bool) {
        self.shared.cut.store(on, Relaxed);
    }

    pub fn cut(&self) -> bool {
        self.shared.cut.load(Relaxed)
    }

    /// Play backwards (loops still wrap; reaching the start pauses).
    pub fn set_reverse(&mut self, on: bool) {
        self.shared.set_reverse(on);
    }

    pub fn reverse(&self) -> bool {
        self.shared.t.reverse()
    }

    /// Vinyl stop: the speed ramps linearly to 0 over `seconds` (0.05..=10, pitch follows like a
    /// turntable, key lock and key shift are bypassed), then the deck pauses where it stopped. No-op while paused.
    pub fn brake(&mut self, seconds: f32) {
        if self.is_playing() {
            self.shared.t.brake(seconds);
        }
    }

    /// Post-gain peak of the most recent output buffer (not clamped: a gain above 1 can exceed it).
    pub fn level(&self) -> f32 {
        self.shared.level()
    }

    /// Metronome of this deck on or off: while on and playing forward, a click is mixed into the deck's master path
    /// on every beat handed over with [`Player::set_beats`]. Tone and volume are the Preferences' (`Bus::apply`).
    pub fn set_metronome(&mut self, on: bool) {
        self.shared.metro.store(on, Relaxed);
    }

    pub fn metronome(&self) -> bool {
        self.shared.metro.load(Relaxed)
    }

    /// The beat grid the metronome clicks on: beat times in track ms, ascending (`None` = no grid). Replaced by
    /// [`Player::load`]; call again after editing the grid.
    pub fn set_beats(&mut self, beats: Option<Arc<[u32]>>) {
        let old = std::mem::replace(&mut *self.shared.beats.lock(), beats);
        drop(old); // freed here, not on the audio thread
    }

    /// VINYL SPEED ADJUST (Preferences > Controller > Deck): `touch_s` = seconds a playing deck takes to slow to a
    /// stop when the platter is touched (a scratch move cancels the ramp), `release_s` = seconds it takes to get
    /// back to full speed when the platter is released or PLAY starts a stopped deck. 0 = instant (the default).
    /// Each is clamped to 0..=10 s; non-numbers read as 0.
    pub fn set_vinyl(&mut self, touch_s: f32, release_s: f32) {
        store_clamped(&self.shared.vinyl_touch, touch_s, 10.0, 0.0);
        store_clamped(&self.shared.vinyl_release, release_s, 10.0, 0.0);
    }

    /// Audible position in track ms (fractional): latency-compensated, interpolated between callbacks, inside the loop.
    pub fn position_ms(&self) -> f64 {
        if self.src_rate == 0 {
            return 0.0;
        }
        let frames = self.frames as f64;
        // Paused, or a seek not yet consumed: no extrapolation, report the resting position. A scratch moves the paused playhead too.
        let f = if (self.is_playing() || self.is_scratching())
            && self.shared.seek.load(Relaxed) == NO_SEEK
        {
            heard_frames(&self.shared.clock.lock(), Instant::now(), frames)
        } else {
            self.pos_frames().min(frames)
        };
        f * 1000.0 / self.src_rate as f64
    }

    /// [`Self::position_ms`] follows the audio being heard: no seek waits for the next buffer and, playing, the first buffer
    /// since the last seek or start is past its output latency (until then the position holds at the target).
    pub fn settled(&self) -> bool {
        self.shared.seek.load(Relaxed) == NO_SEEK
            && audible(&self.shared.clock.lock(), Instant::now())
    }

    /// No brake or spin-up ramp ran in the last buffer: the deck plays at its tempo, so its position is not changing speed.
    pub fn steady(&self) -> bool {
        self.shared.steady.load(Relaxed)
    }

    /// Delay (ms) from a buffer being rendered to it being heard, as the last callback saw it (0 before the first).
    pub fn output_latency_ms(&self) -> f64 {
        self.shared.clock.lock().latency_ms()
    }

    /// Where the next buffer starts, in track ms: [`Self::position_ms`] plus what is still on its way to the speakers. A deck of the
    /// same track seeked here plays in step with this one.
    pub fn written_ms(&self) -> f64 {
        if self.src_rate == 0 {
            return 0.0;
        }
        self.pos_frames().min(self.frames as f64) * 1000.0 / self.src_rate as f64
    }

    /// Clamped to the track length. Leaves the loop as is (see the module doc).
    pub fn seek(&mut self, ms: u32) {
        let f = (ms as f64 * self.src_rate as f64 / 1000.0).min(self.frames as f64);
        self.shared.seek_to(f);
    }

    /// [`Player::seek`] for a jump worked out from where the deck was heard: `ms` was worked out while [`Player::position_ms`]
    /// was `heard_ms`. A seek only takes effect at the next buffer and is heard one output latency later, so the jump lands
    /// as much past `ms` as the deck plays meanwhile. (A jump that keeps a beat grid's phase is [`Player::seek_on_beat`].)
    pub fn seek_in_phase(&mut self, ms: u32, heard_ms: f64) {
        let r = self.src_rate as f64 / 1000.0;
        let f = (ms as f64 * r).min(self.frames as f64);
        self.shared.seek_from_heard(f, Some(heard_ms * r));
    }

    /// [`Player::seek`] keeping the beat (hot cue / stored loop with Q): the jump lands on `ms` plus how far into a beat of the
    /// grid the deck's playhead is at the buffer the jump takes effect in, wrapped in `wrap_ms` when that is > 0 (a loop's length:
    /// the landing stays inside it). The grid has a beat `beat_ms` long starting at `beat_start_ms` (any
    /// beat of it: the grid is taken as even). The phase is read from the audio thread's own position when it takes the seek,
    /// never from a position the caller heard earlier: the press, the next buffer and the output latency lie between them, and a
    /// beat boundary in that gap would otherwise put the landing a whole beat off. A grid that is not usable seeks plainly.
    /// A tempo change needs no beat list here: the phase is the time since the last boundary (`start` plus whole `beat_ms`), which
    /// past the end of the caller's beat is the time since the next beat began, whatever its own length turns out to be; it
    /// goes wrong only when the press-to-buffer gap (tens of ms) outlasts that whole next beat. Where the beat window and
    /// length come from is the caller's grid (the beat under the playhead at the press, so across a tempo change they are the
    /// source section's).
    pub fn seek_on_beat(&mut self, ms: u32, beat_start_ms: f64, beat_ms: f64, wrap_ms: f64) {
        let r = self.src_rate as f64 / 1000.0;
        let (start, len) = (beat_start_ms * r, beat_ms * r);
        if !(start.is_finite() && len.is_finite() && len >= 1.0) {
            return self.seek(ms);
        }
        let f = (ms as f64 * r).min(self.frames as f64);
        self.shared.seek_on_beat(f, start, len, wrap_ms * r);
    }

    pub fn duration_ms(&self) -> u32 {
        if self.src_rate == 0 {
            0
        } else {
            (self.frames * 1000 / self.src_rate as u64) as u32
        }
    }

    /// Copy of `secs` of the loaded track from `from_ms` (shorter at the end of the track, empty without one):
    /// what the sampler captures. Copied in short locked chunks so the audio callback's `try_lock` of the
    /// track rarely misses (a miss is one silent buffer).
    pub fn capture(&self, from_ms: f64, secs: f64) -> Pcm {
        const CHUNK: usize = 8192;
        let rate = self.src_rate as f64;
        let start = ((from_ms.max(0.0) * rate / 1000.0) as usize).min(self.frames as usize);
        let end = (start + (secs.max(0.0) * rate) as usize).min(self.frames as usize);
        let mut samples = Vec::with_capacity(2 * (end - start));
        for a in (start..end).step_by(CHUNK) {
            let pcm = self.shared.pcm.lock();
            samples.extend(pcm.iter_f32(a..(a + CHUNK).min(end)));
        }
        Pcm {
            rate: self.src_rate,
            samples,
        }
    }
}
