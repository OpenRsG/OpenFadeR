//! Performance transport state: key shift, reverse, slip and brake. Session-only; the UI thread sets,
//! the audio callback reads (atomics, no locks).
//!
//! Key shift: the output pitch is `2^(st/12) * (key_lock ? 1 : rate)` while the clock still advances at
//! `rate` ([`pitch`]); the WSOLA stretcher ([`super::stretch`]) does the difference by reading its grains
//! at `pitch` times the resampling step. Zero shift without key lock is plain varispeed.
//!
//! Slip (our design): while
//! slip is on, a *shadow* playhead advances at the deck's normal forward speed. The real playhead stays
//! glued to it until the user *diverges* (a loop is set, reverse is engaged, or a seek/hot cue/jump is
//! made while playing). Releasing (loop cleared with no reverse, reverse off with no loop, or
//! [`Transport::slip_return`]) moves the real playhead to the shadow at the next buffer, sample-accurate.
//! The shadow ignores loops. ponytail: brake and rate changes do not bend the shadow.
//!
//! Brake: vinyl stop. The playback speed factor falls linearly from its current value to 0 over the
//! requested time (varispeed, pitch follows like a turntable), then the deck pauses where it stopped.

use super::{clamped_in, load_f32};
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};

/// Key shift limit in semitones (either way).
pub const SHIFT_MAX: f32 = 12.0;
/// Shortest and longest accepted brake time, seconds.
const BRAKE_SECS: (f32, f32) = (0.05, 10.0);

/// Output pitch relative to the source for a key shift of `st` semitones at tempo `rate`.
pub fn pitch(st: f32, key_lock: bool, rate: f64) -> f64 {
    2f64.powf(st as f64 / 12.0) * if key_lock { 1.0 } else { rate }
}

/// Every field starts at its all-zero bits: no shift, no reverse, no brake, slip off.
#[derive(Default)]
pub struct Transport {
    /// f32 bits, semitones.
    shift: AtomicU32,
    reverse: AtomicBool,
    /// f32 bits, requested seconds; 0 = none (consumed by the callback).
    brake: AtomicU32,
    slip: AtomicBool,
    diverged: AtomicBool,
    /// Return-to-shadow requested (consumed by the callback).
    back: AtomicBool,
    /// f64 bits, source frames (written by the callback).
    shadow: AtomicU64,
}

impl Transport {
    pub fn new() -> Transport {
        Transport::default()
    }

    pub fn set_shift(&self, st: f32) {
        let st = clamped_in(st, -SHIFT_MAX, SHIFT_MAX, 0.0);
        self.shift.store(st.to_bits(), Relaxed);
    }

    pub fn shift(&self) -> f32 {
        load_f32(&self.shift)
    }

    pub fn set_reverse(&self, on: bool) {
        self.reverse.store(on, Relaxed);
    }

    pub fn reverse(&self) -> bool {
        self.reverse.load(Relaxed)
    }

    /// Ask the callback for a brake over `secs` (clamped to 0.05..=10).
    pub fn brake(&self, secs: f32) {
        let s = clamped_in(secs, BRAKE_SECS.0, BRAKE_SECS.1, BRAKE_SECS.1);
        self.brake.store(s.to_bits(), Relaxed);
    }

    /// Callback: the pending brake time, if any.
    pub fn take_brake(&self) -> Option<f32> {
        Some(f32::from_bits(self.brake.swap(0, Relaxed))).filter(|&s| s > 0.0)
    }

    /// Slip on starts with the shadow on the playhead; off drops any divergence in place.
    pub fn set_slip(&self, on: bool) {
        self.slip.store(on, Relaxed);
        self.diverged.store(false, Relaxed);
        self.back.store(false, Relaxed);
    }

    pub fn slip(&self) -> bool {
        self.slip.load(Relaxed)
    }

    /// The user manipulates the playhead: from now on the shadow runs on its own (slip only).
    pub fn diverge(&self) {
        if self.slip() {
            self.diverged.store(true, Release);
        }
    }

    /// The manipulation ended (`looping`: a loop is still set): return to the shadow unless another one holds.
    pub fn release(&self, looping: bool) {
        if !looping && !self.reverse() {
            self.slip_return();
        }
    }

    /// Return the playhead to the shadow at the next buffer (no-op unless slip is on and diverged).
    pub fn slip_return(&self) {
        self.back.store(true, Release);
    }

    /// Shadow position in source frames while slip is on.
    pub fn shadow(&self) -> Option<f64> {
        self.slip()
            .then(|| f64::from_bits(self.shadow.load(Relaxed)))
    }

    /// Callback, start of a buffer at `pos`: the position to jump to when a return is due; otherwise
    /// the shadow follows the playhead until it diverges.
    pub fn begin(&self, pos: f64) -> Option<f64> {
        let back = self.back.swap(false, Acquire);
        if !self.slip() {
            return None;
        }
        if !self.diverged.load(Acquire) {
            self.shadow.store(pos.to_bits(), Relaxed);
            return None;
        }
        if back {
            self.diverged.store(false, Relaxed);
            return Some(f64::from_bits(self.shadow.load(Relaxed)));
        }
        None
    }

    /// Callback, end of a buffer: the shadow moved `by` frames (clamped to `frames`).
    pub fn advance_shadow(&self, by: f64, frames: f64) {
        if self.slip() {
            let s = (f64::from_bits(self.shadow.load(Relaxed)) + by).min(frames);
            self.shadow.store(s.to_bits(), Relaxed);
        }
    }

    /// New track: no reverse, no divergence (slip mode itself and the shift stay).
    pub fn reset(&self) {
        self.reverse.store(false, Relaxed);
        self.diverged.store(false, Relaxed);
        self.back.store(false, Relaxed);
        self.brake.store(0, Relaxed);
    }
}

/// Audio-thread brake ramp: speed factor `left / total`, one step less per frame.
#[derive(Default)]
pub struct Brake {
    left: u32,
    /// 0 = idle.
    total: u32,
}

impl Brake {
    pub fn new() -> Brake {
        Brake::default()
    }

    /// Ramp from the current factor to 0 over `secs` at `dev_rate`.
    pub fn start(&mut self, secs: f32, dev_rate: u32) {
        let f = if self.total == 0 {
            1.0
        } else {
            self.left as f32 / self.total as f32
        };
        self.total = ((secs * dev_rate as f32) as u32).max(1);
        self.left = (f * self.total as f32).round() as u32;
    }

    pub fn active(&self) -> bool {
        self.total > 0
    }

    /// Speed factor for the next frame (1 when idle); 0 on the frame the deck stands still.
    pub fn tick(&mut self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        self.left = self.left.saturating_sub(1);
        self.left as f32 / self.total as f32
    }

    pub fn clear(&mut self) {
        *self = Brake::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_follows_rate_only_without_key_lock() {
        assert_eq!(pitch(0.0, false, 1.08), 1.08);
        assert_eq!(pitch(0.0, true, 1.08), 1.0);
        assert!((pitch(12.0, true, 1.5) - 2.0).abs() < 1e-12);
        assert!((pitch(-12.0, false, 2.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn shift_is_clamped() {
        let t = Transport::new();
        t.set_shift(40.0);
        assert_eq!(t.shift(), 12.0);
        t.set_shift(f32::NAN);
        assert_eq!(t.shift(), 0.0);
    }

    #[test]
    fn brake_ramp_is_linear_and_ends_at_zero() {
        let mut b = Brake::new();
        assert_eq!(b.tick(), 1.0);
        b.start(0.5, 1000);
        let f: Vec<f32> = (0..500).map(|_| b.tick()).collect();
        assert!(f.windows(2).all(|w| w[1] < w[0]));
        assert_eq!(*f.last().unwrap(), 0.0);
        assert!((f[249] - 0.5).abs() < 0.01);
    }
}
