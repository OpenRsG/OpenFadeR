//! Microphone input with talkover (Controller > Mixer > Microphone Input, "Talkover Mode" Advanced / Normal
//! and the talkover level; the level steps -24 / -18 / -12 / -6 dB are the usual mixer talk-over list).
//!
//! The mic is one input pair ([`input::open_into`]) whose frames land in [`Controls`]' tap; the audio thread ([`Voice`])
//! mixes them into channel A's master path, resampling by linear interpolation when the input device runs at another
//! rate than the output. Talkover (as hardware mixers do it): while the mic is louder than the ducking threshold
//! ([`super::duck`]) the other channels are ducked to the talkover level, release [`RELEASE_S`]. Normal ducks the whole
//! signal; Advanced only the mid range ([`BAND`] Hz, a difference of two
//! one-pole low-passes), so the bass and the highs of the music keep playing under the voice. Not in External mixer mode
//! (nothing is mixed there). A talkover gain of 1 (0 dB) means no ducking.

use super::duck::{Duck, Rates};
use super::input::{self, Feed, Keeper};
use super::Tap;
use openfader_plugin::Slot;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};

/// Seconds the other channels take to come back.
const RELEASE_S: f32 = 0.4;
/// Mid range ducked by Advanced talkover, Hz.
const BAND: [f32; 2] = [300.0, 3000.0];

/// The mic's knobs and its frames, shared by the Preferences (UI thread) and the callback.
pub struct Controls {
    tap: Arc<Tap>,
    /// Linear mic level; 0 = the mic is off.
    gain: AtomicU32,
    advanced: AtomicBool,
    /// Linear gain of the other channels while the mic is talking (1 = no talkover).
    duck: AtomicU32,
}

impl Controls {
    fn new() -> Controls {
        Controls {
            tap: Arc::new(Tap::new(48_000, input::TAP_FRAMES)),
            gain: AtomicU32::new(0),
            advanced: AtomicBool::new(true),
            duck: AtomicU32::new(1f32.to_bits()),
        }
    }

    /// The mic level (linear, 0 = off), the talkover mode and the talkover gain (linear, 1 = off); live.
    pub fn set(&self, gain: f32, advanced: bool, duck: f32) {
        super::store_clamped(&self.gain, gain, 4.0, 0.0);
        self.advanced.store(advanced, Relaxed);
        super::store_clamped(&self.duck, duck, 1.0, 1.0);
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Relaxed))
    }

    fn duck(&self) -> f32 {
        f32::from_bits(self.duck.load(Relaxed))
    }
}

/// The process-wide mic (the bus and the Preferences share it, whichever stream is open).
pub fn controls() -> Arc<Controls> {
    static MIC: LazyLock<Arc<Controls>> = LazyLock::new(|| Arc::new(Controls::new()));
    MIC.clone()
}

thread_local! {
    /// The open input stream (a cpal stream stays on the thread that built it).
    static OPEN: RefCell<Keeper> = RefCell::new(Keeper::default());
}

/// Opens (or closes, `on` false) the mic's input pair; a no-op when it already runs for `device` / `pair`. A failure is kept
/// for [`problem`] and the mic stays off.
pub fn open(on: bool, device: &str, pair: u8) {
    OPEN.with_borrow_mut(|k| k.set(on, device, pair, &controls().tap));
}

/// Why the mic could not be opened, if it could not.
pub fn problem() -> Option<String> {
    OPEN.with_borrow(Keeper::problem)
}

/// One-pole low-pass coefficient for `hz` at `rate`.
fn pole(hz: f32, rate: f32) -> f32 {
    1.0 - (-std::f32::consts::TAU * hz / rate).exp()
}

/// Audio-thread state of the mic.
pub struct Voice {
    feed: Feed,
    /// Talkover: peak follower of the mic and the gain of the other channels.
    duck: Duck,
    /// Low-pass states of the summed music at [`BAND`]`[1]` and `[0]`, per side.
    hi: [f32; 2],
    lo: [f32; 2],
    /// MIC FX rack ([`Slot::Mic`]): on the mic after its level.
    fx: super::fx::Unit,
}

impl Voice {
    pub fn new(rate: u32) -> Voice {
        Voice::with_fx(super::fx::Unit::rack(Slot::Mic, rate))
    }

    fn with_fx(fx: super::fx::Unit) -> Voice {
        Voice {
            feed: Feed::default(),
            duck: Duck::new(),
            hi: [0.0; 2],
            lo: [0.0; 2],
            fx,
        }
    }

    /// Adds the mic to channel 0's master path of `buf` after ducking the rest (every channel's `.0` is a master path).
    pub fn mix(
        &mut self,
        c: &Controls,
        fx: &super::fx::Controls,
        rate: u32,
        buf: &mut [Vec<([f32; 2], [f32; 2])>],
        n: usize,
    ) {
        self.fx.set(fx.settings());
        let gain = c.gain();
        if gain == 0.0 {
            self.duck.reset();
            return;
        }
        let step = self.feed.begin(&c.tap, rate, n);
        let (target, advanced) = (c.duck(), c.advanced.load(Relaxed));
        let rates = Rates::new(rate, RELEASE_S);
        let (a_hi, a_lo) = (pole(BAND[1], rate as f32), pole(BAND[0], rate as f32));
        for i in 0..n {
            let dry = self.feed.next(&c.tap, step).map(|x| x * gain);
            let m = self.fx.process(dry); // talkover follows the voice as heard, effect included
            let cut = 1.0 - self.duck.tick(m[0].abs().max(m[1].abs()), target, &rates);
            for k in 0..2 {
                let sum: f32 = buf.iter().map(|b| b[i].0[k]).sum();
                self.hi[k] += (sum - self.hi[k]) * a_hi;
                self.lo[k] += (sum - self.lo[k]) * a_lo;
                let ducked = cut
                    * if advanced {
                        self.hi[k] - self.lo[k]
                    } else {
                        sum
                    };
                buf[0][i].0[k] += m[k] - ducked;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::fx;

    /// `n` frames of constant `music` on channel 0 (the other channels silent) with `mic` waiting in the ring.
    fn run_fx(
        c: &Controls,
        fx: &fx::Controls,
        voice: &mut Voice,
        music: f32,
        mic: f32,
        n: usize,
    ) -> Vec<f32> {
        for _ in 0..n {
            c.tap.ring.push([mic; 2]);
        }
        let mut buf = vec![vec![([0.0; 2], [0.0; 2]); n]; 4];
        buf[0].fill(([music; 2], [0.0; 2]));
        voice.mix(c, fx, 48_000, &mut buf, n);
        buf[0].iter().map(|f| f.0[0]).collect()
    }

    fn run(c: &Controls, voice: &mut Voice, music: f32, mic: f32, n: usize) -> Vec<f32> {
        run_fx(c, &fx::Controls::default(), voice, music, mic, n)
    }

    #[test]
    fn mic_off_leaves_the_music_alone() {
        let c = Controls::new();
        let out = run(&c, &mut Voice::new(48_000), 0.5, 0.5, 64);
        assert!(out.iter().all(|&x| x == 0.5));
    }

    #[test]
    fn normal_talkover_ducks_the_whole_signal_to_the_level_and_adds_the_mic() {
        let c = Controls::new();
        c.set(1.0, false, 0.25);
        c.tap.set_rate(48_000);
        let mut v = Voice::new(48_000);
        let out = run(&c, &mut v, 0.5, 0.2, 48_000);
        let end = *out.last().unwrap();
        assert!((end - (0.5 * 0.25 + 0.2)).abs() < 0.01, "{end}");
        let out = run(&c, &mut v, 0.5, 0.0, 48_000 * 2);
        assert!(
            (out.last().unwrap() - 0.5).abs() < 0.01,
            "the music comes back when the mic is quiet"
        );
    }

    #[test]
    fn advanced_talkover_keeps_the_low_end() {
        let c = Controls::new();
        c.set(1.0, true, 0.1);
        c.tap.set_rate(48_000);
        // A constant (bass) lies below the ducked band: the music stays at full level under the voice.
        let out = run(&c, &mut Voice::new(48_000), 0.5, 0.2, 48_000);
        assert!(
            (out.last().unwrap() - 0.7).abs() < 0.02,
            "{}",
            out.last().unwrap()
        );
    }

    // GAIN (doubles the signal) from the plugin tests' fake library stands in for a MIC FX plugin.

    #[test]
    fn mic_fx_changes_the_mic_and_not_the_music() {
        let c = Controls::new();
        c.set(1.0, false, 1.0); // talkover level 0 dB: no ducking, the sum is music + mic
        c.tap.set_rate(48_000);
        let effects = crate::plugin::effect::fake_effects();
        let mut v = Voice::with_fx(fx::Unit::rack_with(48_000, &effects));
        let fxc = fx::Controls::default();
        fxc.set_level(1.0);
        let off = run_fx(&c, &fxc, &mut v, 0.5, 0.1, 4800);
        assert!(
            (off.last().unwrap() - 0.6).abs() < 1e-3,
            "{}",
            off.last().unwrap()
        );
        fxc.set_on(true);
        let on = run_fx(&c, &fxc, &mut v, 0.5, 0.1, 4800);
        assert!(
            (on.last().unwrap() - 0.7).abs() < 1e-3,
            "{}",
            on.last().unwrap()
        );
    }
}
