//! Sampler SEQUENCER: records the pad hits of the one-shot slots into a pattern of 1, 2 or 4 bars and plays it back in
//! step with the master tempo.
//!
//! The pattern is a list of events `(slot, tick)`, `tick` counting [`TICKS`] per beat from the start of the pattern
//! (4 beats per bar). The clock runs on the audio thread: it advances by `frames × master BPM / 60 × TICKS / rate` per
//! buffer, so the pattern follows the tempo [`Controls::set_seq_bpm`] is given (the master deck's, playing or not). Events are kept in
//! atomics (no locks, no allocation in the callback); the UI reads and writes them through [`Controls`].
//!
//! Recording (overdub): [`Mode::Standby`] waits for the first pad hit, which starts the pattern at tick 0 and goes to
//! [`Mode::Rec`]; every hit of a one-shot slot after that is stored at the pattern clock's position, moved to the nearest
//! multiple of the quantise step when that is on (`Controls::set_seq_step`; a hit is seen at the start of the buffer it
//! arrived in, so an unquantised hit is up to one buffer early). The pattern keeps looping and playing while it records
//! (overdub). Muted slots are skipped on playback, erase removes one slot's events.
//! Playback triggers are sample-accurate inside the buffer: [`Sampler::add`](super::Sampler::add) splits the buffer at
//! each event and restarts that slot's voice there.
//!
//! ponytail: the pattern clock starts at 0 when the sequencer starts, it is not phase-locked to the master deck's beat;
//! at most [`MAX_EVENTS`] events and [`PER_BUFFER`] triggers per buffer.

use super::{Controls, SLOTS, Slot, load_f32, put};
use std::sync::atomic::{
    AtomicU8, AtomicU32,
    Ordering::{Acquire, Relaxed, Release},
};

/// Ticks per beat; a sixteenth note is `TICKS / 4`.
pub const TICKS: u32 = 96;
/// Pattern lengths offered, bars.
pub const BARS: [u8; 3] = [1, 2, 4];
/// Events one pattern holds.
pub const MAX_EVENTS: usize = 256;
/// Triggers one buffer can carry (more are dropped).
pub const PER_BUFFER: usize = 32;
/// Recorded hits this pass of the pattern that must not also play back in it.
const FRESH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Stop,
    Play,
    /// REC pressed: waits for the first pad hit, which starts the pattern.
    Standby,
    /// Playing and recording the hits (overdub).
    Rec,
}

impl Mode {
    fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::Play,
            2 => Mode::Standby,
            3 => Mode::Rec,
            _ => Mode::Stop,
        }
    }
}

/// Pattern length in ticks.
pub fn len_ticks(bars: u8) -> u32 {
    bars.max(1) as u32 * 4 * TICKS
}

/// `tick` moved to the nearest multiple of `step` (0 = unquantised) inside a pattern of `len` ticks (a hit rounded up to the
/// end lands on tick 0 of the next pass).
pub fn quantise(tick: u32, step: u32, len: u32) -> u32 {
    if step == 0 {
        tick % len
    } else {
        (tick + step / 2) / step * step % len
    }
}

/// An event as stored: a set top bit marks it in use (so slot 0 at tick 0 is not "empty").
pub fn pack(slot: usize, tick: u32) -> u32 {
    1 << 31 | (slot as u32) << 16 | tick
}

pub fn unpack(e: u32) -> Option<(usize, u32)> {
    (e >> 31 == 1).then_some((((e >> 16) & 0xff) as usize, e & 0xffff))
}

/// What the UI sets (shared with the audio thread).
pub struct Ctl {
    mode: AtomicU8,
    bars: AtomicU8,
    /// Quantise step, ticks; 0 = off.
    step: AtomicU32,
    /// Slots muted from playback, bit per slot.
    mute: AtomicU32,
    /// Counts restarts asked for (the clock goes back to 0).
    restart: AtomicU32,
    /// The clock, ticks (written by the callback).
    phase: AtomicU32,
    /// Tempo the clock runs at, BPM (f32 bits); 0 = the clock stands still.
    bpm: AtomicU32,
    events: [AtomicU32; MAX_EVENTS],
}

impl Default for Ctl {
    fn default() -> Ctl {
        Ctl {
            mode: AtomicU8::new(0),
            bars: AtomicU8::new(1),
            step: AtomicU32::new(0),
            mute: AtomicU32::new(0),
            restart: AtomicU32::new(0),
            phase: AtomicU32::new(0),
            bpm: AtomicU32::new(0),
            events: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

impl Controls {
    pub fn seq_mode(&self) -> Mode {
        Mode::from_u8(self.seq.mode.load(Acquire))
    }

    pub fn set_seq_mode(&self, m: Mode) {
        self.seq.mode.store(m as u8, Release);
    }

    /// Starts the pattern from its beginning (also while it plays).
    pub fn seq_restart(&self) {
        self.seq.restart.fetch_add(1, Release);
    }

    pub fn seq_bars(&self) -> u8 {
        self.seq.bars.load(Relaxed)
    }

    /// Pattern length (one of [`BARS`], else 1); events past the new end are dropped.
    pub fn set_seq_bars(&self, bars: u8) {
        let bars = if BARS.contains(&bars) { bars } else { 1 };
        self.seq.bars.store(bars, Relaxed);
        let len = len_ticks(bars);
        for e in &self.seq.events {
            if unpack(e.load(Relaxed)).is_some_and(|(_, t)| t >= len) {
                e.store(0, Relaxed);
            }
        }
    }

    /// Quantise step in ticks, 0 = record the hit where it came.
    pub fn set_seq_step(&self, ticks: u32) {
        self.seq.step.store(ticks, Relaxed);
    }

    /// The tempo the pattern clock runs at, BPM (0 / non-finite = it stands still).
    pub fn set_seq_bpm(&self, bpm: f32) {
        put(&self.seq.bpm, bpm, 999.0, 0.0);
    }

    /// Toggles slot `i`'s sequence mute; true when it is muted now.
    pub fn seq_toggle_mute(&self, i: usize) -> bool {
        self.seq.mute.fetch_xor(1 << i, Relaxed) & 1 << i == 0
    }

    pub fn seq_muted(&self, i: usize) -> bool {
        self.seq.mute.load(Relaxed) & 1 << i != 0
    }

    /// Removes every event of slot `i`.
    pub fn seq_erase(&self, i: usize) {
        for e in &self.seq.events {
            if unpack(e.load(Relaxed)).is_some_and(|(s, _)| s == i) {
                e.store(0, Relaxed);
            }
        }
    }

    /// The pattern clock, ticks.
    pub fn seq_phase(&self) -> u32 {
        self.seq.phase.load(Relaxed)
    }

    /// The recorded events (packed, see [`pack`]), earliest first.
    pub fn seq_events(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .seq
            .events
            .iter()
            .map(|e| e.load(Relaxed))
            .filter(|&e| unpack(e).is_some())
            .collect();
        v.sort_by_key(|&e| (e & 0xffff, e >> 16));
        v
    }

    /// Replaces the pattern by `bars` bars of `events`; events that do not fit are dropped.
    pub fn seq_set(&self, bars: u8, events: &[u32]) {
        for e in &self.seq.events {
            e.store(0, Relaxed);
        }
        self.set_seq_bars(bars);
        let len = len_ticks(self.seq_bars());
        let ok = events
            .iter()
            .filter(|&&e| unpack(e).is_some_and(|(s, t)| s < SLOTS && t < len));
        for (cell, &e) in self.seq.events.iter().zip(ok) {
            cell.store(e, Relaxed);
        }
    }
}

/// Triggers of one buffer: `(frame offset, slot)`, earliest first.
pub struct Triggers {
    pub n: usize,
    pub at: [(usize, u8); PER_BUFFER],
}

impl Triggers {
    fn push(&mut self, offset: usize, slot: usize) {
        if self.n < PER_BUFFER {
            self.at[self.n] = (offset, slot as u8);
            self.n += 1;
        }
    }
}

/// Audio-thread state of the sequencer.
pub struct State {
    mode: Mode,
    /// Clock, ticks.
    phase: f64,
    restart: u32,
    /// Presses of every slot already looked at.
    seen: [u32; SLOTS],
    fresh: [u32; FRESH],
    n_fresh: usize,
}

impl State {
    pub fn new() -> State {
        State {
            mode: Mode::Stop,
            phase: 0.0,
            restart: 0,
            seen: [0; SLOTS],
            fresh: [0; FRESH],
            n_fresh: 0,
        }
    }

    /// Looks at the pad presses of this buffer (recording them when a recording is on), advances the clock by `frames`
    /// at the sequencer's tempo and returns the events falling inside the buffer.
    pub(super) fn step(&mut self, c: &Controls, frames: usize, rate: u32) -> Triggers {
        let mut out = Triggers {
            n: 0,
            at: [(0, 0); PER_BUFFER],
        };
        let ctl = &c.seq;
        let mode = c.seq_mode();
        if mode != self.mode {
            if self.mode == Mode::Stop && mode != Mode::Stop {
                self.phase = 0.0;
                self.n_fresh = 0;
            }
            self.mode = mode;
        }
        let restart = ctl.restart.load(Acquire);
        if restart != self.restart {
            (self.restart, self.phase, self.n_fresh) = (restart, 0.0, 0);
        }
        for (i, s) in c.slots.iter().enumerate() {
            let t = s.trig.load(Acquire);
            if t != self.seen[i] {
                self.seen[i] = t;
                if matches!(self.mode, Mode::Standby | Mode::Rec) && recordable(s) {
                    self.record(c, i);
                }
            }
        }
        let bpm = load_f32(&ctl.bpm);
        if matches!(self.mode, Mode::Play | Mode::Rec) && bpm > 0.0 && rate > 0 && frames > 0 {
            let len = len_ticks(c.seq_bars()) as f64;
            let adv = frames as f64 * bpm as f64 / 60.0 * TICKS as f64 / rate as f64;
            let (from, to) = (self.phase, self.phase + adv);
            let muted = ctl.mute.load(Relaxed);
            for cell in &ctl.events {
                let e = cell.load(Relaxed);
                let Some((slot, t)) = unpack(e) else { continue };
                let t = t as f64;
                // `again`: the event lies past the wrap, in the next pass, where a hit recorded in this one plays too
                let (ticks, again) = if t >= from && t < to {
                    (t - from, false)
                } else if to > len && t < to - len {
                    (t + len - from, true)
                } else {
                    continue;
                };
                if muted & 1 << slot == 0 && (again || !self.fresh[..self.n_fresh].contains(&e)) {
                    let at = (ticks / adv * frames as f64).round() as usize;
                    out.push(at.min(frames - 1), slot);
                }
            }
            if to >= len {
                self.phase = to - len;
                self.n_fresh = 0;
            } else {
                self.phase = to;
            }
            out.at[..out.n].sort_by_key(|a| a.0);
        }
        ctl.phase.store(self.phase as u32, Relaxed);
        out
    }

    /// Stores a hit of slot `i` at the clock's position.
    fn record(&mut self, c: &Controls, i: usize) {
        let ctl = &c.seq;
        if self.mode == Mode::Standby {
            if ctl
                .mode
                .compare_exchange(Mode::Standby as u8, Mode::Rec as u8, Relaxed, Relaxed)
                .is_err()
            {
                return; // the UI changed its mind in the meantime
            }
            (self.mode, self.phase, self.n_fresh) = (Mode::Rec, 0.0, 0);
        }
        let tick = quantise(
            self.phase as u32,
            ctl.step.load(Relaxed),
            len_ticks(c.seq_bars()),
        );
        let e = pack(i, tick);
        if ctl.events.iter().any(|x| x.load(Relaxed) == e) {
            return;
        }
        if let Some(free) = ctl.events.iter().find(|x| x.load(Relaxed) == 0) {
            free.store(e, Relaxed);
            if self.n_fresh < FRESH {
                self.fresh[self.n_fresh] = e;
                self.n_fresh += 1;
            }
        }
    }
}

/// Only a slot in play mode (one-shot, not gate) is recorded.
fn recordable(s: &Slot) -> bool {
    !s.looped.load(Relaxed) && !s.gate.load(Relaxed)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{RATE, dc, render};
    use super::super::*;
    use super::*;

    /// 120 BPM = 4000 frames per beat; a tick is 4000 / 96 frames.
    const BEAT: usize = 4000;

    fn setup() -> (Sampler, Controls) {
        let (s, c) = (Sampler::new(RATE), Controls::default());
        c.load(0, dc(100), 0.0);
        c.load(
            1,
            Pcm {
                samples: vec![0.25; 2 * 100],
                ..dc(0)
            },
            0.0,
        );
        c.set_seq_bpm(120.0);
        (s, c)
    }

    /// First frame of each burst of sound (the sample starts where the sequencer fired).
    fn starts(out: &[f32]) -> Vec<usize> {
        (0..out.len())
            .filter(|&i| out[i] != 0.0 && (i == 0 || out[i - 1] == 0.0))
            .collect()
    }

    #[test]
    fn quantise_rounds_to_the_step_and_wraps_at_the_pattern_end() {
        let len = len_ticks(1);
        assert_eq!(len, 384);
        assert_eq!(
            [0, 11, 12, 35, 36].map(|t| quantise(t, 24, len)),
            [0, 0, 24, 24, 48]
        );
        assert_eq!(quantise(383, 24, len), 0, "rounds up into the next pass");
        assert_eq!(quantise(100, 0, len), 100, "off: unchanged");
        assert_eq!(unpack(pack(0, 0)), Some((0, 0)));
        assert_eq!(unpack(pack(15, 1535)), Some((15, 1535)));
        assert_eq!(unpack(0), None);
    }

    #[test]
    fn a_hit_is_recorded_on_the_grid_and_plays_back_every_pass() {
        let (mut s, c) = setup();
        c.set_seq_step(TICKS / 4);
        c.set_seq_mode(Mode::Standby);
        render(&mut s, &c, 10, 10);
        assert_eq!(c.seq_mode(), Mode::Standby, "no hit yet: waiting");
        c.press(0); // the first hit starts the pattern at tick 0
        render(&mut s, &c, 1, 1);
        assert_eq!(c.seq_mode(), Mode::Rec);
        // 1.5 beats later a hit of slot 1 -> sixteenth grid: tick 144
        render(&mut s, &c, BEAT * 3 / 2 - 1 - 5, 64);
        c.press(1);
        render(&mut s, &c, 5, 5);
        assert_eq!(c.seq_events(), [pack(0, 0), pack(1, 144)]);
        // stop recording, keep playing: from the next pass the pattern sounds by itself
        c.set_seq_mode(Mode::Play);
        let rest = 4 * BEAT - BEAT * 3 / 2;
        render(&mut s, &c, rest, 100);
        let out = render(&mut s, &c, 4 * BEAT, 333);
        assert_eq!(
            starts(&out),
            [0, BEAT * 3 / 2],
            "slot 0 on the downbeat, slot 1 on the grid point after"
        );
        assert_eq!(out[0], 0.5);
        assert_eq!(out[BEAT * 3 / 2], 0.25);
    }

    #[test]
    fn a_hit_just_recorded_does_not_also_play_in_the_same_pass() {
        let (mut s, c) = setup();
        c.set_seq_step(TICKS); // whole beats: a hit at 0.6 beat is recorded on beat 1, ahead of the clock
        c.set_seq_mode(Mode::Rec);
        render(&mut s, &c, BEAT * 6 / 10, 50);
        c.press(0);
        let out = render(&mut s, &c, BEAT, 50);
        assert_eq!(c.seq_events(), [pack(0, TICKS)]);
        assert_eq!(
            starts(&out),
            [0],
            "only the live hit sounds, not the stored copy"
        );
    }

    #[test]
    fn tempo_decides_where_the_events_fall() {
        let (mut s, c) = setup();
        c.seq_set(1, &[pack(0, 2 * TICKS)]);
        c.set_seq_mode(Mode::Play);
        c.set_seq_bpm(60.0); // beat = 8000 frames
        let out = render(&mut s, &c, 4 * 2 * BEAT, 977);
        assert_eq!(starts(&out), [2 * 2 * BEAT]);
        c.set_seq_bpm(240.0);
        c.seq_restart();
        let out = render(&mut s, &c, 2 * BEAT, 977);
        assert_eq!(
            starts(&out),
            [BEAT],
            "restart at 240 BPM: beat 2 = 4000 frames"
        );
    }

    #[test]
    fn the_pattern_wraps_across_a_buffer_edge_and_muted_slots_stay_silent() {
        let (mut s, c) = setup();
        c.seq_set(1, &[pack(0, 0), pack(1, 0)]);
        c.set_seq_mode(Mode::Play);
        assert!(c.seq_toggle_mute(1) && c.seq_muted(1));
        let pass = 4 * BEAT;
        let out = render(&mut s, &c, 3 * pass, 1000); // 4000 / 1000 and the wrap both land on edges
        assert_eq!(starts(&out), [0, pass, 2 * pass]);
        assert!(out[..100].iter().all(|&x| x == 0.5), "slot 1 is muted");
        assert!(!c.seq_toggle_mute(1));
        let out = render(&mut s, &c, 100, 100);
        assert_eq!(
            out[0], 0.75,
            "both slots at the next pass start (0.5 + 0.25)"
        );
    }

    #[test]
    fn erase_removes_one_slot_and_looped_slots_are_not_recorded() {
        let (mut s, c) = setup();
        c.seq_set(2, &[pack(0, 5), pack(1, 7), pack(0, 400)]);
        c.seq_erase(0);
        assert_eq!(c.seq_events(), [pack(1, 7)]);
        c.set_seq_bars(1); // events past the new end go
        c.seq_set(1, &[pack(1, 7), pack(0, 400)]);
        assert_eq!(
            c.seq_events(),
            [pack(1, 7)],
            "tick 400 does not fit one bar"
        );
        c.set_loop(0, true);
        c.set_seq_mode(Mode::Rec);
        render(&mut s, &c, 10, 10);
        c.press(0);
        render(&mut s, &c, 10, 10);
        assert_eq!(c.seq_events(), [pack(1, 7)], "loop slots are not recorded");
    }
}
