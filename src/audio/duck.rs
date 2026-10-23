//! Talkover-style ducking shared by the mic ([`super::mic`]) and AUX ([`super::auxin`]): a peak follower on the voice that
//! ducks, and the gain the others are pulled to while it is louder than [`THRESHOLD`] (attack [`ATTACK_S`], then back at the
//! release time of the setting).

/// Loudness (linear peak) above which ducking starts: -40 dBFS.
const THRESHOLD: f32 = 0.01;
/// Seconds to duck.
const ATTACK_S: f32 = 0.02;
/// Time constant of the loudness follower, seconds.
const FOLLOW_S: f32 = 0.008;
/// Closer to 1 than this the gain lands on 1, so a released duck leaves the signal bit-exact.
const SNAP: f32 = 1e-4;

/// One-pole coefficient with time constant `s` seconds at `rate`.
pub(super) fn tau(s: f32, rate: f32) -> f32 {
    1.0 - (-1.0 / (s.max(1e-4) * rate)).exp()
}

/// The per-frame coefficients of one buffer.
pub(super) struct Rates {
    att: f32,
    rel: f32,
    decay: f32,
}

impl Rates {
    /// For a stream at `rate` Hz and a release time of `release_s` seconds.
    pub fn new(rate: u32, release_s: f32) -> Rates {
        let rate = rate as f32;
        Rates {
            att: tau(ATTACK_S, rate),
            rel: tau(release_s, rate),
            decay: 1.0 - tau(FOLLOW_S, rate),
        }
    }
}

pub(super) struct Duck {
    /// Peak follower of the ducking voice.
    env: f32,
    /// Gain of the others now (1 = untouched).
    gain: f32,
}

impl Duck {
    pub fn new() -> Duck {
        Duck {
            env: 0.0,
            gain: 1.0,
        }
    }

    /// One frame: the follower takes `peak`; while it is above [`THRESHOLD`] the gain moves to `target`, else back to 1.
    /// Returns the gain.
    pub fn tick(&mut self, peak: f32, target: f32, r: &Rates) -> f32 {
        self.env = if peak > self.env {
            peak
        } else {
            self.env * r.decay
        };
        let want = if self.env > THRESHOLD { target } else { 1.0 };
        self.gain += (want - self.gain) * if want < self.gain { r.att } else { r.rel };
        if want == 1.0 && self.gain > 1.0 - SNAP {
            self.gain = 1.0;
        }
        self.gain
    }

    /// Nothing is ducking: the gain is 1 and the follower is down.
    pub fn reset(&mut self) {
        *self = Duck::new();
    }

    /// The gain is 1 and the follower is below the threshold.
    pub fn idle(&self) -> bool {
        self.gain == 1.0 && self.env <= THRESHOLD
    }
}
