//! Jog scratch: the playhead chases a target built from the platter's accumulated motion.
//!
//! `Player::scratch_move` adds track seconds to a target; per output frame the playhead follows it
//! through two cascaded one-pole filters with the same time constant ([`TAU`]), i.e. a critically damped
//! second-order response with no overshoot. The audio is read at the playhead itself (varispeed:
//! pitch follows the speed, reverse when it falls), so key lock and key shift are bypassed like on
//! vinyl. A held platter (speed ~ 0) is silent: the gain follows the speed ([`GATE_SPEED`]) instead of
//! holding one sample as DC. Loops are ignored while scratching. (Model chosen by reasoning, time
//! constant inferred.)
//!
//! VINYL SPEED ADJUST (`Player::set_vinyl`; Preferences > Controller > Deck "Touch/Brake" and "Release/Start"; the linear ramps are ours): touching the platter of a
//! playing deck first *coasts* ([`Scratch::start`]): the target keeps moving at the deck's speed and slows
//! linearly to a stop over the touch time, until the platter moves (`step` with a non-zero motion cancels it).
//! Releasing the platter of a playing deck, or pressing PLAY on a stopped one, *spins up* ([`Spin`]): the speed
//! factor rises linearly from 0 to 1 over the release time (the mirror of the brake in [`super::super::transport`]).

/// Filter time constant of the platter-to-audio chase, seconds. ~5-10 ms feels tight without zipper noise.
const TAU: f64 = 0.007;
/// Speed (relative to normal playback) at and above which the scratch is at full volume.
pub(super) const GATE_SPEED: f64 = 0.025;
/// Gate smoothing per output frame (~2 ms at 44.1 kHz).
const GATE_SLEW: f32 = 0.01;

/// Audio-thread state of one deck's scratch; `on` follows the shared flag.
pub(super) struct Scratch {
    pub(super) on: bool,
    target: f64,
    mid: f64,
    pos: f64,
    gate: f32,
    alpha: f64,
    /// Touch ramp: fraction of the deck's speed the target still moves at (1 = full, 0 = stopped) and its
    /// decrement per output frame.
    coast: f32,
    coast_step: f32,
}

impl Scratch {
    pub(super) fn new() -> Scratch {
        Scratch {
            on: false,
            target: 0.0,
            mid: 0.0,
            pos: 0.0,
            gate: 0.0,
            alpha: 0.0,
            coast: 0.0,
            coast_step: 0.0,
        }
    }

    /// Start (or restart after a seek) at source frame `pos`, at rest; `heard` = the deck was playing, so the gate starts open.
    /// With `heard` and a `touch_s` above 0 the platter instead starts at the deck's speed (`play` source frames per
    /// output frame) and slows to a stop over `touch_s` seconds; the chase filters start in their steady state at that
    /// speed, so the sound neither jumps nor dips as the platter is touched.
    pub(super) fn start(&mut self, pos: f64, heard: bool, dev_rate: u32, touch_s: f32, play: f64) {
        let alpha = 1.0 - (-1.0 / (TAU * dev_rate as f64)).exp();
        let coast = heard && touch_s > 0.0;
        let lag = if coast {
            play * (1.0 - alpha) / alpha
        } else {
            0.0
        }; // lag of one filter behind a ramp at `play`
        *self = Scratch {
            on: true,
            target: pos + 2.0 * lag,
            mid: pos + lag,
            pos,
            gate: if heard { 1.0 } else { 0.0 },
            alpha,
            coast: if coast { 1.0 } else { 0.0 },
            coast_step: if coast {
                1.0 / (touch_s * dev_rate as f32)
            } else {
                0.0
            },
        };
    }

    /// Target moved by `d` source frames (clamped to `0..=max`); returns the playhead and the speed gate.
    /// `nominal` = source frames per output frame at normal speed, `play` = at the deck's current speed (the touch ramp's start).
    pub(super) fn step(&mut self, d: f64, max: f64, nominal: f64, play: f64) -> (f64, f32) {
        let d = if d != 0.0 {
            self.coast = 0.0; // the platter moves: the user is scratching, no more coasting
            d
        } else if self.coast > 0.0 {
            let c = self.coast;
            self.coast = (c - self.coast_step).max(0.0);
            play * c as f64
        } else {
            0.0
        };
        self.target = (self.target + d).clamp(0.0, max);
        self.mid += (self.target - self.mid) * self.alpha;
        let next = self.pos + (self.mid - self.pos) * self.alpha;
        let speed = (next - self.pos).abs() / nominal;
        self.pos = next;
        self.gate += ((speed / GATE_SPEED).min(1.0) as f32 - self.gate) * GATE_SLEW;
        (next, self.gate)
    }
}

/// Speed-up ramp (audio thread): the factor is `done / total`, reaching 1 on the last of `total` frames; idle = 1.
pub(super) struct Spin {
    done: u32,
    total: u32, // 0 = idle
}

impl Spin {
    pub(super) fn new() -> Spin {
        Spin { done: 0, total: 0 }
    }

    /// Ramp from 0 to 1 over `secs` at `dev_rate`.
    pub(super) fn start(&mut self, secs: f32, dev_rate: u32) {
        *self = Spin {
            done: 0,
            total: ((secs * dev_rate as f32) as u32).max(2),
        };
    }

    pub(super) fn active(&self) -> bool {
        self.total > 0
    }

    /// Speed factor for the next frame: above 0 from the first one (a 0 would read as "stopped"), 1 once the ramp is over.
    pub(super) fn tick(&mut self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        self.done += 1;
        let f = self.done as f32 / self.total as f32;
        if self.done >= self.total {
            *self = Spin::new();
        }
        f.min(1.0)
    }

    pub(super) fn clear(&mut self) {
        *self = Spin::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// A deck at normal speed of a 48 kHz track.
    const PLAY: f64 = 1.0;

    #[test]
    fn spin_up_rises_linearly_from_just_above_zero_to_one_then_idles() {
        let mut s = Spin::new();
        assert_eq!((s.tick(), s.active()), (1.0, false));
        s.start(0.5, 1000);
        let f: Vec<f32> = (0..500).map(|_| s.tick()).collect();
        assert!(f[0] > 0.0 && f.windows(2).all(|w| w[1] > w[0]));
        assert!((f[249] - 0.5).abs() < 0.01);
        assert_eq!((f[499], s.active(), s.tick()), (1.0, false, 1.0));
    }

    /// Plays `frames` frames of a scratch started on a playing deck; returns the playhead speed per frame.
    fn speeds(touch_s: f32, moves: impl Fn(usize) -> f64, frames: usize) -> Vec<f64> {
        let mut sc = Scratch::new();
        sc.start(10_000.0, true, RATE, touch_s, PLAY);
        let mut last = 10_000.0;
        (0..frames)
            .map(|i| {
                let (p, _) = sc.step(moves(i), 1e9, PLAY, PLAY);
                let v = p - last;
                last = p;
                v
            })
            .collect()
    }

    #[test]
    fn touching_a_playing_deck_keeps_its_speed_then_slows_to_a_stop_over_the_touch_time() {
        let v = speeds(0.5, |_| 0.0, 40_000);
        assert!(
            (v[0] - PLAY).abs() < 0.01 && (v[100] - PLAY).abs() < 0.01,
            "no dip at the touch: {} {}",
            v[0],
            v[100]
        );
        let half = v[12_000]; // 0.25 s into the 0.5 s ramp (the chase lags by ~14 ms)
        assert!(
            (half - 0.5).abs() < 0.1,
            "about half speed in the middle: {half}"
        );
        assert!(
            v[30_000].abs() < 1e-3,
            "stopped after the touch time: {}",
            v[30_000]
        );
        assert!(
            v.windows(2).all(|w| w[1] <= w[0] + 1e-9),
            "slows monotonically"
        );
    }

    #[test]
    fn a_platter_move_cancels_the_ramp_and_without_a_touch_time_the_start_is_the_old_instant_stop()
    {
        let v = speeds(
            0.5,
            |i| {
                if i == 2_000 {
                    0.0
                } else if i > 2_000 {
                    0.2
                } else {
                    0.0
                }
            },
            8_000,
        );
        assert!(
            (v[7_000] - 0.2).abs() < 0.01,
            "follows the platter after it moved: {}",
            v[7_000]
        );
        let v = speeds(0.0, |_| 0.0, 4_000);
        assert!(
            v[0].abs() < 0.01 && v[3_999].abs() < 1e-6,
            "instant stop, as before: {}",
            v[0]
        );
        let mut sc = Scratch::new();
        sc.start(10_000.0, false, RATE, 0.5, PLAY);
        assert!(
            sc.step(0.0, 1e9, PLAY, PLAY).0 == 10_000.0,
            "a paused deck has nothing to coast on"
        );
    }
}
