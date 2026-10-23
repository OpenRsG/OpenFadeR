//! Sampler: 16 slots, each one audio slice, mixed into the master bus ([`Sampler::add`], called once per
//! buffer by the player's callback after the Beat FX) with an optional copy on the cue (headphone) path.
//!
//! SAMPLER. Per slot: ONE-SHOT or
//! LOOP, TRIGGER (press starts) or GATE (plays while held), BPM SYNC and a volume; the sampler has a master
//! volume and a CUE switch. Press semantics: a one-shot press restarts the slice; a loop press
//! starts it and a second press stops it; gate plays while held (looping when the slot is LOOP).
//! BPM SYNC plays the slice at `master tempo / slice tempo` through the player's WSOLA stretcher
//! ([`super::stretch`], pitch kept), so a slice of `b` beats lasts `b * 60 / tempo` seconds.
//!
//! Threads: the UI sets atomics in [`Controls`] and hands a new slice over through the slot's
//! [`Handover`](super::handover::Handover) (the callback neither allocates, frees nor blocks). Children: [`voice`]
//! (one slot's playback), [`seq`] (the pattern sequencer).
//!
//! ponytail: slots stay at their own sample rate and are resampled linearly like decks; they are not
//! loudness-matched; the sampler bypasses the Beat FX and the channel EQ/filters.

mod seq;
mod voice;
pub use seq::{BARS, Mode, TICKS, len_ticks, pack};
#[cfg(test)]
mod tests;

use super::handover::Handover;
use super::{Pcm, load_f32, store_clamped as put};
use std::sync::{
    Arc,
    atomic::{
        AtomicBool, AtomicU32,
        Ordering::{Relaxed, Release},
    },
};
use voice::Voice;

/// Slots (two banks of eight pads).
pub const SLOTS: usize = 16;
/// Longest slice a load should hand over, seconds (callers truncate; the sampler never looks at it).
/// ponytail: 30 s x 16 slots x 48 kHz stereo f32 is at most ~180 MB; raise only if that is acceptable.
pub const MAX_SECS: f64 = 30.0;

/// One slot's knobs, shared with the audio thread.
struct Slot {
    /// The loaded slice; an empty [`Pcm`] stands for an empty slot.
    slice: Handover<Pcm>,
    /// Presses so far (the callback compares with its own count) and whether the pad is down now.
    trig: AtomicU32,
    held: AtomicBool,
    /// Stops requested so far (the callback compares with its own count).
    stops: AtomicU32,
    looped: AtomicBool,
    gate: AtomicBool,
    sync: AtomicBool,
    volume: AtomicU32,
    /// Tempo of the slice in BPM, 0 = unknown (then SYNC does nothing).
    bpm: AtomicU32,
    /// Written by the callback: the slot is sounding.
    playing: AtomicBool,
}

impl Slot {
    fn new() -> Slot {
        Slot {
            slice: Handover::default(),
            trig: AtomicU32::new(0),
            stops: AtomicU32::new(0),
            held: AtomicBool::new(false),
            looped: AtomicBool::new(false),
            gate: AtomicBool::new(false),
            sync: AtomicBool::new(false),
            volume: AtomicU32::new(1f32.to_bits()),
            bpm: AtomicU32::new(0),
            playing: AtomicBool::new(false),
        }
    }
}

/// Everything the UI sets (all lock-free except the slice handover); read by the audio thread each buffer.
pub struct Controls {
    slots: [Slot; SLOTS],
    /// Slots in use (the sequencer plays no others).
    used: AtomicU32,
    master: AtomicU32,
    cue: AtomicBool,
    bpm: AtomicU32,
    seq: seq::Ctl,
}

impl Default for Controls {
    fn default() -> Controls {
        Controls {
            slots: std::array::from_fn(|_| Slot::new()),
            used: AtomicU32::new(SLOTS as u32),
            master: AtomicU32::new(1f32.to_bits()),
            cue: AtomicBool::new(false),
            bpm: AtomicU32::new(0),
            seq: seq::Ctl::default(),
        }
    }
}

impl Controls {
    fn with(&self, i: usize, f: impl FnOnce(&Slot)) {
        if let Some(s) = self.slots.get(i) {
            f(s);
        }
    }

    /// Hands `pcm` to slot `i` (it stops whatever the slot was playing); `bpm` is the slice's tempo, 0 = unknown.
    /// An empty `pcm` empties the slot.
    pub fn load(&self, i: usize, pcm: Pcm, bpm: f32) {
        self.with(i, |s| {
            put(&s.bpm, bpm, 999.0, 0.0);
            s.slice.load(Some(Arc::new(pcm)));
        });
    }

    /// Slot `i`'s loaded slice (shared, not copied); `None` when the slot is empty.
    pub fn slice(&self, i: usize) -> Option<Arc<Pcm>> {
        self.slots
            .get(i)?
            .slice
            .kept()
            .filter(|p| !p.samples.is_empty())
    }

    pub fn clear(&self, i: usize) {
        self.load(i, Pcm::default(), 0.0);
    }

    /// How many slots are in use (the first `n`): the sequencer skips the events of the others.
    pub fn set_used(&self, n: usize) {
        self.used.store(n.min(SLOTS) as u32, Relaxed);
    }

    /// Pad pressed: see the module doc for what that does per mode.
    pub fn press(&self, i: usize) {
        self.with(i, |s| {
            s.held.store(true, Relaxed);
            s.trig.fetch_add(1, Release);
        });
    }

    /// Stops slot `i` (fade-out) whatever its mode; an idle slot ignores it.
    pub fn stop(&self, i: usize) {
        self.with(i, |s| {
            s.stops.fetch_add(1, Release);
        });
    }

    /// Pad released: ends a GATE slot.
    pub fn release(&self, i: usize) {
        self.with(i, |s| s.held.store(false, Release));
    }

    /// LOOP (true) or ONE-SHOT (false).
    pub fn set_loop(&self, i: usize, on: bool) {
        self.with(i, |s| s.looped.store(on, Relaxed));
    }

    /// GATE (true: plays while held) or TRIGGER (false).
    pub fn set_gate(&self, i: usize, on: bool) {
        self.with(i, |s| s.gate.store(on, Relaxed));
    }

    pub fn set_sync(&self, i: usize, on: bool) {
        self.with(i, |s| s.sync.store(on, Relaxed));
    }

    /// Slot volume, linear 0..=2 (non-finite = 1).
    pub fn set_volume(&self, i: usize, v: f32) {
        self.with(i, |s| put(&s.volume, v, 2.0, 1.0));
    }

    /// Sampler master volume, linear 0..=2 (non-finite = 1).
    pub fn set_master(&self, v: f32) {
        put(&self.master, v, 2.0, 1.0);
    }

    /// Also feed the headphone (cue) path.
    pub fn set_cue(&self, on: bool) {
        self.cue.store(on, Relaxed);
    }

    /// Tempo SYNC slots follow, BPM; 0 / non-finite = none (SYNC then plays at the slice's own tempo).
    pub fn set_bpm(&self, bpm: f32) {
        put(&self.bpm, bpm, 999.0, 0.0);
    }

    /// Is slot `i` sounding (as of the last buffer)?
    pub fn playing(&self, i: usize) -> bool {
        self.slots.get(i).is_some_and(|s| s.playing.load(Relaxed))
    }
}

/// Audio-thread state: one [`Voice`] per slot, all allocated up front.
pub struct Sampler {
    rate: u32,
    voices: [Voice; SLOTS],
    seq: seq::State,
}

impl Sampler {
    /// `rate`: the output stream's sample rate.
    pub fn new(rate: u32) -> Sampler {
        Sampler {
            rate,
            voices: std::array::from_fn(|_| Voice::new()),
            seq: seq::State::new(),
        }
    }

    /// Adds every sounding slot into `buf` (per frame: master path, cue path) × slot volume × master volume; the
    /// sequencer's events restart their slot inside the buffer ([`seq`]). Does not allocate or block.
    pub fn add(&mut self, c: &Controls, buf: &mut [([f32; 2], [f32; 2])]) {
        let (master, cue, tempo) = (load_f32(&c.master), c.cue.load(Relaxed), load_f32(&c.bpm));
        let hits = self.seq.step(c, buf.len(), self.rate);
        let mut from = 0;
        for &(at, slot) in hits.at[..hits.n].iter().chain(&[(buf.len(), 0)]) {
            if at > from {
                for (v, s) in self.voices.iter_mut().zip(&c.slots) {
                    v.add(s, master, cue, tempo, self.rate, &mut buf[from..at]);
                }
                from = at;
            }
            if at < buf.len() {
                self.voices[slot as usize].fire();
            }
        }
    }
}
