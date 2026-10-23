//! AUX / LINE input: a fifth mixer channel fed by one input pair (Preferences > Audio > AUX INPUT), beside the mic.
//!
//! The pair is opened like the mic's ([`input::Keeper`]) into [`Controls`]' tap while the strip's ON is set. The audio thread
//! ([`Voice`]) reads the ring ([`Feed`], linear resampling to the stream's rate) and runs the strip: TRIM, the 3-band EQ and
//! the one-knob filter ([`super::dsp::Fx`], as a deck has them), then the LEVEL fader. The result is the channel's master path
//! in the channel buffers' last slot ([`super::AUX`]), so the Beat FX can sit on it (assign AUX) and the Master FX and the
//! LEVER rack hear it in their sum. The cue path is the signal before the level while CUE is on, like a deck's.
//!
//! Ducking (an option, off at a talkover gain of 1): while the AUX is audible after its level, the four deck channels' master
//! paths are pulled to the set gain ([`super::duck`]) and come back over the set release time. The mic's talkover is
//! [`super::mic`]; it ducks everything else including this channel. Not in External mixer mode (nothing is mixed there).

use super::dsp::Fx;
use super::duck::{Duck, Rates};
use super::input::{self, Feed, Keeper};
use super::{clamped_in, gain_at, load_f32, store_clamped, Tap};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};

type Frame = ([f32; 2], [f32; 2]);

/// Linear top of TRIM and LEVEL.
const MAX_GAIN: f32 = 4.0;
/// Release times the ducking accepts, seconds.
const RELEASE_S: (f32, f32) = (0.05, 10.0);

/// The AUX strip's knobs and its frames, shared by the mixer UI and the callback.
pub struct Controls {
    pub(super) tap: Arc<Tap>,
    on: AtomicBool,
    cue: AtomicBool,
    /// Linear TRIM and LEVEL, EQ bands in dB (low, mid, high), filter knob -1..=1.
    trim: AtomicU32,
    level: AtomicU32,
    eq: [AtomicU32; 3],
    filter: AtomicU32,
    /// Linear gain of the decks while the AUX is heard (1 = no ducking) and the seconds they take to come back.
    duck: AtomicU32,
    release: AtomicU32,
    /// Peak of the channel after its level in the last buffer (written by the callback).
    peak: AtomicU32,
}

impl Controls {
    pub(super) fn new() -> Controls {
        Controls {
            tap: Arc::new(Tap::new(48_000, input::TAP_FRAMES)),
            on: AtomicBool::new(false),
            cue: AtomicBool::new(false),
            trim: AtomicU32::new(1f32.to_bits()),
            level: AtomicU32::new(0f32.to_bits()),
            eq: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            filter: AtomicU32::new(0f32.to_bits()),
            duck: AtomicU32::new(1f32.to_bits()),
            release: AtomicU32::new(0.4f32.to_bits()),
            peak: AtomicU32::new(0f32.to_bits()),
        }
    }

    /// The ON switch: off, the channel fades out and the input pair is not read.
    pub fn set_on(&self, on: bool) {
        self.on.store(on, Relaxed);
    }

    pub fn on(&self) -> bool {
        self.on.load(Relaxed)
    }

    /// Headphone CUE: the signal before the level goes to the headphone feed.
    pub fn set_cue(&self, cue: bool) {
        self.cue.store(cue, Relaxed);
    }

    /// TRIM and LEVEL (linear), the EQ bands (dB, low / mid / high) and the filter knob (-1 low-pass .. 1 high-pass); live.
    pub fn set_strip(&self, trim: f32, eq_db: [f32; 3], filter: f32, level: f32) {
        store_clamped(&self.trim, trim, MAX_GAIN, 1.0);
        store_clamped(&self.level, level, MAX_GAIN, 0.0);
        for (a, db) in self.eq.iter().zip(eq_db) {
            a.store(super::dsp::clamp_db(db).to_bits(), Relaxed);
        }
        self.filter
            .store(clamped_in(filter, -1.0, 1.0, 0.0).to_bits(), Relaxed);
    }

    /// The gain the decks are pulled to while the AUX is heard (linear, 1 = no ducking) and their release time in seconds; live.
    pub fn set_duck(&self, gain: f32, release_s: f32) {
        store_clamped(&self.duck, gain, 1.0, 1.0);
        self.release.store(
            clamped_in(release_s, RELEASE_S.0, RELEASE_S.1, 0.4).to_bits(),
            Relaxed,
        );
    }

    /// Peak (max |sample|) of the channel after its level in the last output buffer, for the meter.
    pub fn peak(&self) -> f32 {
        load_f32(&self.peak)
    }
}

/// The process-wide AUX (the bus and the mixer share it, whichever stream is open).
pub fn controls() -> Arc<Controls> {
    static AUX: LazyLock<Arc<Controls>> = LazyLock::new(|| Arc::new(Controls::new()));
    AUX.clone()
}

thread_local! {
    /// The open input stream (a cpal stream stays on the thread that built it).
    static OPEN: RefCell<Keeper> = RefCell::new(Keeper::default());
}

/// Opens (or closes, `on` false) the AUX input pair; a no-op when it already runs for `device` / `pair`. A failure is kept
/// for [`problem`] and the channel stays silent.
pub fn open(on: bool, device: &str, pair: u8) {
    OPEN.with_borrow_mut(|k| k.set(on, device, pair, &controls().tap));
}

/// Why the AUX input could not be opened, if it could not.
pub fn problem() -> Option<String> {
    OPEN.with_borrow(Keeper::problem)
}

/// Audio-thread state of the AUX channel.
pub struct Voice {
    feed: Feed,
    /// TRIM, LEVEL and cue gain at the end of the last buffer (ramped like a deck's gains).
    gains: [f32; 3],
    fx: Fx,
    duck: Duck,
}

impl Voice {
    pub fn new() -> Voice {
        Voice {
            feed: Feed::default(),
            gains: [1.0, 0.0, 0.0],
            fx: Fx::new(),
            duck: Duck::new(),
        }
    }

    /// Renders `n` frames of the strip into `aux` (master path, cue path) and ducks `decks`' master paths by the ducking gain.
    /// Does not allocate or block.
    pub fn render(
        &mut self,
        c: &Controls,
        rate: u32,
        decks: &mut [Vec<Frame>],
        aux: &mut [Frame],
        n: usize,
    ) {
        let on = c.on();
        let [trim0, level0, cue0] = self.gains;
        if !on && level0 == 0.0 && cue0 == 0.0 && self.duck.idle() {
            while c.tap.ring.pop().is_some() {} // nothing stale waits for the next ON
            aux[..n].fill(([0.0; 2], [0.0; 2]));
            c.peak.store(0, Relaxed);
            return;
        }
        let trim1 = load_f32(&c.trim);
        let level1 = if on { load_f32(&c.level) } else { 0.0 };
        let cue1 = if on && c.cue.load(Relaxed) { 1.0 } else { 0.0 };
        self.gains = [trim1, level1, cue1];
        self.fx.set(
            rate as f64,
            c.eq.each_ref().map(load_f32),
            load_f32(&c.filter),
            n,
        );
        let step = self.feed.begin(&c.tap, rate, n);
        let (target, rates) = (load_f32(&c.duck), Rates::new(rate, load_f32(&c.release)));
        let mut peak = 0f32;
        for (i, out) in aux[..n].iter_mut().enumerate() {
            let t = gain_at(trim0, trim1, i, n);
            let x = self.feed.next(&c.tap, step);
            let (l, r) = self.fx.process(x[0] * t, x[1] * t);
            let (g, gc) = (gain_at(level0, level1, i, n), gain_at(cue0, cue1, i, n));
            let m = [l * g, r * g];
            *out = (m, [l * gc, r * gc]);
            let heard = m[0].abs().max(m[1].abs());
            peak = peak.max(heard);
            let d = self.duck.tick(heard, target, &rates);
            if d != 1.0 {
                for deck in decks.iter_mut() {
                    deck[i].0 = deck[i].0.map(|s| s * d);
                }
            }
        }
        c.peak.store(peak.to_bits(), Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::EQ_KILL_DB;

    const RATE: u32 = 48_000;

    /// `n` frames of a `hz` sine of amplitude `amp`, starting `at` frames in.
    fn sine(hz: f32, amp: f32, at: usize, n: usize) -> impl Iterator<Item = [f32; 2]> {
        (at..at + n).map(move |i| {
            let v = amp * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin();
            [v, v]
        })
    }

    fn rms(x: &[Frame]) -> f32 {
        (x.iter().map(|f| f.0[0] * f.0[0]).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Two buffers of `n` frames of the `hz` sine through `v` (the first lets the ramps settle); returns the second's
    /// aux frames and the decks (four channels holding `deck` on their master path).
    fn run(
        c: &Controls,
        v: &mut Voice,
        hz: f32,
        amp: f32,
        deck: f32,
        n: usize,
    ) -> (Vec<Frame>, Vec<Vec<Frame>>) {
        let mut out = (vec![], vec![]);
        for b in 0..2 {
            for f in sine(hz, amp, b * n, n) {
                c.tap.ring.push(f);
            }
            let mut decks = vec![vec![([deck; 2], [0.0; 2]); n]; 4];
            let mut aux = vec![([0.0; 2], [0.0; 2]); n];
            v.render(c, RATE, &mut decks, &mut aux, n);
            out = (aux, decks);
        }
        out
    }

    fn strip(level: f32) -> Controls {
        let c = Controls::new();
        c.tap.set_rate(RATE);
        c.set_on(true);
        c.set_strip(1.0, [0.0; 3], 0.0, level);
        c
    }

    #[test]
    fn input_reaches_the_master_with_the_strip_applied() {
        let n = 24_000;
        // LEVEL and TRIM scale it; the EQ and the filter are flat (bit-exact bypass).
        let c = strip(0.5);
        let (aux, _) = run(&c, &mut Voice::new(), 1000.0, 0.4, 0.0, n);
        let want = 0.4 * 0.5 / 2f32.sqrt();
        assert!((rms(&aux) - want).abs() < 0.002, "{} vs {want}", rms(&aux));
        assert!((c.peak() - 0.2).abs() < 0.002, "the meter: {}", c.peak());
        c.set_strip(2.0, [0.0; 3], 0.0, 0.5);
        let (aux, _) = run(&c, &mut Voice::new(), 1000.0, 0.2, 0.0, n);
        assert!(
            (rms(&aux) - want).abs() < 0.002,
            "TRIM x2 on half the input"
        );
        // The HIGH band killed removes a 10 kHz tone and keeps a 100 Hz one.
        c.set_strip(1.0, [0.0, 0.0, EQ_KILL_DB], 0.0, 1.0);
        let kill = rms(&run(&c, &mut Voice::new(), 10_000.0, 0.4, 0.0, n).0);
        let keep = rms(&run(&c, &mut Voice::new(), 100.0, 0.4, 0.0, n).0);
        assert!(kill < 0.01 && keep > 0.25, "{kill} {keep}");
        // The filter knob at -1 (low-pass) removes the 10 kHz tone with the EQ flat.
        c.set_strip(1.0, [0.0; 3], -1.0, 1.0);
        assert!(rms(&run(&c, &mut Voice::new(), 10_000.0, 0.4, 0.0, n).0) < 0.01);
    }

    #[test]
    fn off_is_silent_and_fades_out_instead_of_cutting() {
        let c = strip(1.0);
        let mut v = Voice::new();
        let n = 4800;
        let (aux, _) = run(&c, &mut v, 1000.0, 0.4, 0.0, n);
        assert!(rms(&aux) > 0.2);
        c.set_on(false);
        for f in sine(1000.0, 0.4, 2 * n, n) {
            c.tap.ring.push(f);
        }
        let mut aux = vec![([0.0; 2], [0.0; 2]); n];
        v.render(
            &c,
            RATE,
            &mut vec![vec![([0.0; 2], [0.0; 2]); n]; 4],
            &mut aux,
            n,
        );
        assert!(
            aux[0].0[0].abs() > 0.0 || aux[1].0[0].abs() > 0.0,
            "ramping down, not cut"
        );
        assert_eq!(aux[n - 1].0, [0.0; 2], "and gone at the end of the buffer");
        v.render(
            &c,
            RATE,
            &mut vec![vec![([0.0; 2], [0.0; 2]); n]; 4],
            &mut aux,
            n,
        );
        assert!(aux.iter().all(|f| f.0 == [0.0; 2] && f.1 == [0.0; 2]));
        assert_eq!(c.peak(), 0.0);
    }

    #[test]
    fn the_cue_feed_is_the_signal_before_the_level_and_only_while_cue_is_on() {
        let n = 24_000;
        let c = strip(0.0); // LEVEL at the bottom: nothing on the master
        let (aux, _) = run(&c, &mut Voice::new(), 1000.0, 0.4, 0.0, n);
        assert!(
            aux.iter().all(|f| f.1 == [0.0; 2]),
            "CUE off: nothing in the headphones"
        );
        c.set_cue(true);
        let (aux, _) = run(&c, &mut Voice::new(), 1000.0, 0.4, 0.0, n);
        let cue = (aux.iter().map(|f| f.1[0] * f.1[0]).sum::<f32>() / n as f32).sqrt();
        assert!(
            (cue - 0.4 / 2f32.sqrt()).abs() < 0.002,
            "CUE on: pre-fader {cue}"
        );
        assert!(
            aux.iter().all(|f| f.0 == [0.0; 2]),
            "the master stays silent"
        );
    }

    #[test]
    fn ducking_lowers_the_decks_by_the_set_amount_and_releases_in_the_set_time() {
        let n = 48_000;
        let c = strip(1.0);
        c.set_duck(0.25, 0.5);
        let mut v = Voice::new();
        let (_, decks) = run(&c, &mut v, 1000.0, 0.4, 0.8, n);
        let held = decks[2][n - 1].0[0];
        assert!(
            (held - 0.8 * 0.25).abs() < 0.005,
            "ducked to a quarter: {held}"
        );
        assert!(
            decks.iter().all(|d| d[n - 1].1 == [0.0; 2]),
            "only the master paths"
        );
        // The AUX goes quiet: the decks come back within the release, not before a good part of it.
        let quiet = |v: &mut Voice, frames: usize| {
            let mut decks = vec![vec![([0.8; 2], [0.0; 2]); frames]; 4];
            v.render(
                &c,
                RATE,
                &mut decks,
                &mut vec![([0.0; 2], [0.0; 2]); frames],
                frames,
            );
            decks[0][frames - 1].0[0]
        };
        let early = quiet(&mut v, 4800); // 0.1 s after the follower drops
        assert!(
            early < 0.8 * 0.6,
            "still held down 0.1 s into a 0.5 s release: {early}"
        );
        let late = quiet(&mut v, 4 * RATE as usize); // 4 s later
        assert!((late - 0.8).abs() < 0.002, "released: {late}");
        // Off by default: a gain of 1 leaves the decks alone.
        let c = strip(1.0);
        let (_, decks) = run(&c, &mut Voice::new(), 1000.0, 0.4, 0.8, n);
        assert!(decks.iter().flatten().all(|f| f.0 == [0.8; 2]));
    }

    #[test]
    fn a_quiet_aux_does_not_duck() {
        let c = strip(0.0); // LEVEL down: nothing is heard
        c.set_duck(0.25, 0.5);
        let (_, decks) = run(&c, &mut Voice::new(), 1000.0, 0.4, 0.8, 24_000);
        assert!(decks.iter().flatten().all(|f| f.0 == [0.8; 2]));
    }
}
