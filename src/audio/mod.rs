//! Audio. Base: [`Pcm`] (interleaved stereo f32), its resampling read [`Pcm::frame_at`], the read-only [`Frames`] view analysis
//! and measuring code take (f32 slices and [`Track16`]), the position arithmetic
//! children share ([`Loop`], [`advance`]), the lock-free frame queue [`Ring`], the device-frame writer [`put_frame`], the
//! output-device lookup [`find_output`], and the knob helpers [`clamped`], [`store_clamped`], [`load_f32`] and [`mono`].
//! Children: [`decode`] (whole-file symphonia decode to f32 [`Pcm`], or straight to a [`Track16`] with the audio fingerprint;
//! [`decode_async`] / [`decode_track16_async`] on a worker thread), [`dsp`] (EQ + filter biquads),
//! [`stretch`] (WSOLA key lock), [`bus`] (master / headphone routing math and knobs), [`route`] (which device channel pairs
//! carry Master / Headphones / Booth, or the six External outputs), [`device`] (what the OS devices support), [`input`] (an
//! input pair for REC), [`mic`] (the microphone and talkover), [`auxin`] (the AUX / LINE channel: input pair, strip and ducking),
//! [`duck`] (the ducker the mic and AUX share), [`metronome`] (the grid click), [`preview`] (the browser's
//! preview player), [`cue`] (second output device for the headphones, `OPENFADER_CUE_DEVICE`), [`rec`] (post-master WAV
//! recorder), [`player`] (one cpal stream shared by the mixer's `Player`s: varispeed or key-locked tempo, loop, volume, cue
//! gain, EQ, filter, level meter, and the latency-compensated position clock), [`transport`] (key shift, reverse, slip and
//! brake state), [`fx`] (the Beat FX unit on a channel or the master), [`cfx`] (the channel's COLOR FX knob: FILTER, SPACE,
//! DUB ECHO, SWEEP, NOISE, CRUSH), [`merge`] (MERGE FX: one knob builds up a high-pass, noise and echo per channel),
//! [`stems`] (STEMS: a track's separated parts, [`Source`] and the per-stem gain mix the voice plays; the separation itself
//! is `crate::stems`), [`sampler`] (16 slice slots mixed into the master bus), [`groove`] (BEAT LAB drum slots),
//! [`handover`] (the UI to callback mailbox the sampler and groove share), [`loudness`] (AUTO GAIN loudness estimate);
//! [`track16`] ([`Track16`]: the 16-bit track a deck's player and the browser preview keep, half the RAM of a [`Pcm`]); [`wavout`] writes a slice as a
//! 16-bit stereo WAV.

pub mod auxin;
mod bus;
pub mod cfx;
mod cue;
mod decode;
pub mod device;
mod dsp;
mod duck;
pub mod fx;
pub mod groove;
mod handover;
pub mod input;
pub mod loudness;
pub mod merge;
pub mod metronome;
pub mod mic;
mod player;
pub mod preview;
mod rec;
pub mod route;
pub mod sampler;
pub mod stemfile;
mod stems;
mod stretch;
mod track16;
mod transport;
pub(crate) mod wavout;

pub use bus::{equal_power, Bus, Headphones, AUX, CHANNELS, FX_SLOTS, FX_UNITS, MAX_LEVEL};
pub use decode::{
    DecodeJob, decode, decode_async, decode_cancellable, decode_track16_async,
    decode_track16_cancellable, probe_rate,
};
pub use dsp::{EQ_KILL_DB, EQ_MAX_DB};
pub use player::{
    Player, Status, configure, controller, live_bus, release_controller, set_output, status,
};
pub use rec::{new_path as rec_path, Options as RecOptions, Recorder, Take};
pub use stems::{
    BASS, COUNT as STEM_COUNT, DRUMS, Grid, INST, Mixed, NAMES as STEM_NAMES, Source, Stems, VOCAL,
};
pub use track16::Track16;
pub use wavout::write_wav;

use cpal::FromSample;
use std::ops::Range;
use std::sync::atomic::{
    AtomicU32, AtomicU64, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

/// Lock-free queue of stereo frames between exactly one producer thread (`push`) and one consumer
/// thread (`pop`); that split is by convention, not enforced. A frame is one `AtomicU64` (L bits
/// << 32 | R bits). A full ring drops the pushed frame and counts it in [`Ring::dropped`].
pub struct Ring {
    slots: Box<[AtomicU64]>,
    head: AtomicUsize, // frames ever pushed
    tail: AtomicUsize, // frames ever popped
    dropped: AtomicU64,
}

impl Ring {
    /// `capacity` frames (rounded up to a power of two).
    pub fn new(capacity: usize) -> Ring {
        let n = capacity.max(2).next_power_of_two();
        Ring {
            slots: (0..n).map(|_| AtomicU64::new(0)).collect(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.head
            .load(Acquire)
            .wrapping_sub(self.tail.load(Acquire))
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }

    /// Producer side. False (and counted) when full.
    pub fn push(&self, [l, r]: [f32; 2]) -> bool {
        let head = self.head.load(Relaxed);
        if head.wrapping_sub(self.tail.load(Acquire)) == self.slots.len() {
            self.dropped.fetch_add(1, Relaxed);
            return false;
        }
        self.slots[head & (self.slots.len() - 1)]
            .store((l.to_bits() as u64) << 32 | r.to_bits() as u64, Relaxed);
        self.head.store(head.wrapping_add(1), Release);
        true
    }

    /// Consumer side.
    pub fn pop(&self) -> Option<[f32; 2]> {
        let tail = self.tail.load(Relaxed);
        if tail == self.head.load(Acquire) {
            return None;
        }
        let v = self.slots[tail & (self.slots.len() - 1)].load(Relaxed);
        self.tail.store(tail.wrapping_add(1), Release);
        Some([f32::from_bits((v >> 32) as u32), f32::from_bits(v as u32)])
    }
}

/// The first output device whose name contains `want` (case-insensitive) and its name. Errors name `env` and
/// list every available device; nothing falls back to another device.
fn find_output(want: &str, env: &str) -> Result<(String, cpal::Device), String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let mut devs: Vec<(String, cpal::Device)> = cpal::default_host()
        .output_devices()
        .map_err(|e| format!("{env}: cannot list output devices: {e}"))?
        .filter_map(|d| Some((d.name().ok()?, d)))
        .collect();
    let want_lc = want.to_lowercase();
    match devs
        .iter()
        .position(|(n, _)| n.to_lowercase().contains(&want_lc))
    {
        Some(i) => Ok(devs.swap_remove(i)),
        None => {
            let names: Vec<_> = devs.iter().map(|(n, _)| n.as_str()).collect();
            Err(format!(
                "{env}={want:?}: no such output device; available: {}",
                names.join(", ")
            ))
        }
    }
}

/// Mean of a stereo frame.
pub(crate) fn mono([l, r]: [f32; 2]) -> f32 {
    (l + r) * 0.5
}

/// Writes one stereo frame into a device frame of any channel count: mono = mean, channels past 2 silent.
fn put_frame<T: cpal::Sample + FromSample<f32>>(out: &mut [T], frame @ [l, r]: [f32; 2]) {
    let n = out.len();
    for (c, o) in out.iter_mut().enumerate() {
        *o = T::from_sample(match (n, c) {
            (1, _) => mono(frame),
            (_, 0) => l,
            (_, 1) => r,
            _ => 0.0,
        });
    }
}

/// The post-master stereo mix offered to a recorder: the audio thread pushes into `ring` while `on`
/// is set; `rate` is the stream's sample rate (what the frames are at).
pub struct Tap {
    pub ring: Ring,
    pub on: std::sync::atomic::AtomicBool,
    rate: AtomicU32,
}

impl Tap {
    /// `capacity` frames of slack for the writer thread (a stalled disk longer than that loses frames, counted by `ring.dropped()`).
    pub fn new(rate: u32, capacity: usize) -> Tap {
        Tap {
            ring: Ring::new(capacity),
            on: std::sync::atomic::AtomicBool::new(false),
            rate: AtomicU32::new(rate),
        }
    }

    /// Sample rate of the frames (it follows the output device, see [`bus::Bus`]).
    pub fn rate(&self) -> u32 {
        self.rate.load(Relaxed)
    }

    fn set_rate(&self, rate: u32) {
        self.rate.store(rate, Relaxed);
    }
}

/// Loop in source frames, `(start, end)`.
type Loop = Option<(f64, f64)>;

/// Source-frame position after `delta` more frames from `pos`. With a loop, wraps at its end when
/// moving forward from below it and the result reaches it, and at its start when moving backward
/// (`delta < 0`) from at/above it and the result falls below it (carrying the overshoot; several
/// loops if `delta` is long).
fn advance(pos: f64, delta: f64, lp: Loop) -> f64 {
    let p = pos + delta;
    match lp {
        Some((s, e)) if delta >= 0.0 && pos < e && p >= e => s + (p - e) % (e - s),
        Some((s, e)) if delta < 0.0 && pos >= s && p < s => e - (s - p) % (e - s),
        _ => p,
    }
}

/// The loop changed from `old` to `new` and the playhead `pos` is at or past the new end, either where the old loop held it
/// (halved, a shorter beat loop, a new SLICER / SLICE LOOP length) or less than `lag` past it (a loop worked out from the
/// heard playhead, which lags what the callback already wrote by the output latency: a short loop's end can be behind the
/// written position already): where it continues, the same phase of the new loop (DJ software keeps playing in the loop).
/// `None` when nothing has to move ([`advance`] carries on as usual).
fn shrunk(old: Loop, new: Loop, pos: f64, lag: f64) -> Option<f64> {
    let (s, e) = new?;
    let held = old.is_some_and(|(os, oe)| (os..oe).contains(&pos));
    (old != new && pos >= e && e > s && (held || pos - e < lag)).then(|| s + (pos - s) % (e - s))
}

/// Interleaved stereo f32 at `rate` Hz.
#[derive(Default, Clone)]
pub struct Pcm {
    pub rate: u32,
    pub samples: Vec<f32>,
}

impl Pcm {
    pub fn duration_ms(&self) -> u32 {
        if self.rate == 0 {
            0
        } else {
            (self.samples.len() as u64 / 2 * 1000 / self.rate as u64) as u32
        }
    }

    /// Stereo frame at fractional source-frame position `pos`, linearly interpolated; None past the end.
    /// ponytail: linear interpolation aliases/dulls when rates differ (44.1k<->48k is audible on bright
    /// material); upgrade to a windowed-sinc/polyphase resampler (e.g. rubato) if that matters.
    pub fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        lerp_at(self.samples.len() / 2, pos, |k, c| self.samples[2 * k + c])
    }
}

/// Read-only stereo audio, read front to back: the one input of the analysis and measuring code, so a deck's [`Track16`] is
/// analysed without a full-length f32 copy. Interleaved f32 (`[f32]`, `Vec<f32>`, anything `AsRef<[f32]>`; the rate is then
/// a separate argument, as it is for [`Track16::rate`]) and [`Track16`] implement it.
pub trait Frames: Sync {
    fn frame_count(&self) -> usize;
    /// Frames `range`, dequantised if need be. Panics when the range is past the end.
    fn frames_in(&self, range: Range<usize>) -> impl Iterator<Item = [f32; 2]>;
}

impl<T: AsRef<[f32]> + Sync + ?Sized> Frames for T {
    fn frame_count(&self) -> usize {
        self.as_ref().len() / 2
    }

    fn frames_in(&self, range: Range<usize>) -> impl Iterator<Item = [f32; 2]> {
        self.as_ref()[2 * range.start..2 * range.end]
            .chunks_exact(2)
            .map(|f| [f[0], f[1]])
    }
}

/// Frame `pos` of a `frames`-frame signal read through `sample(frame, channel)`, linearly interpolated; the last frame
/// holds, `None` at or past the end. The one resampling read of [`Pcm`] and [`Track16`].
#[inline]
fn lerp_at(frames: usize, pos: f64, sample: impl Fn(usize, usize) -> f32) -> Option<[f32; 2]> {
    let i = pos as usize;
    if i >= frames {
        return None;
    }
    let j = (i + 1).min(frames - 1);
    let t = (pos - i as f64) as f32;
    Some([0, 1].map(|c| {
        let a = sample(i, c);
        a + (sample(j, c) - a) * t
    }))
}

/// The `f32` stored as bits in `a` (see [`store_clamped`]).
pub(crate) fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Relaxed))
}

/// `v` clamped to `lo..=hi`, `bad` when it is NaN or infinite.
pub(crate) fn clamped_in(v: f32, lo: f32, hi: f32, bad: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { bad }
}

/// [`clamped_in`] on `0..=max`.
pub(crate) fn clamped(v: f32, max: f32, bad: f32) -> f32 {
    clamped_in(v, 0.0, max, bad)
}

/// Stores [`clamped`]`(v, max, bad)` as bits (a knob the audio thread reads).
pub(crate) fn store_clamped(a: &AtomicU32, v: f32, max: f32, bad: f32) {
    a.store(clamped(v, max, bad).to_bits(), Relaxed);
}

/// Gain at frame `i` of an `n`-frame buffer that ramps linearly from `from` to `to`, landing on `to` at the last frame.
pub(crate) fn gain_at(from: f32, to: f32, i: usize, n: usize) -> f32 {
    from + (to - from) * ((i + 1) as f32 / n as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_maps_positions() {
        // ramp: frame k = (k, -k); 22.05 kHz source on a 44.1 kHz device => step 0.5 source frames
        let pcm = Pcm {
            rate: 22050,
            samples: (0..4).flat_map(|k| [k as f32, -(k as f32)]).collect(),
        };
        let step = 22050.0 / 44100.0;
        let got: Vec<_> = (0..7)
            .map(|n| pcm.frame_at(n as f64 * step).map(|f| f[0]))
            .collect();
        // last real frame (k=3) holds, nothing at/after pos 4.0
        assert_eq!(
            got,
            [
                Some(0.0),
                Some(0.5),
                Some(1.0),
                Some(1.5),
                Some(2.0),
                Some(2.5),
                Some(3.0)
            ]
        );
        assert_eq!(pcm.frame_at(3.5).map(|f| f[1]), Some(-3.0));
        assert_eq!(pcm.frame_at(4.0), None);
        // downsample 48k -> 44.1k: step > 1 lands on interpolated positions
        assert_eq!(
            pcm.frame_at(48000.0 / 44100.0).unwrap()[0],
            48000.0 / 44100.0
        );
    }

    #[test]
    fn ring_keeps_order_wraps_and_drops_when_full() {
        let r = Ring::new(4);
        assert_eq!(r.pop(), None);
        for round in 0..10 {
            let base = (round * 4) as f32;
            for k in 0..4 {
                assert!(r.push([base + k as f32, -(base + k as f32)]));
            }
            assert_eq!((r.len(), r.push([9.0; 2])), (4, false), "full ring refuses");
            for k in 0..4 {
                assert_eq!(r.pop(), Some([base + k as f32, -(base + k as f32)]));
            }
        }
        assert_eq!((r.len(), r.dropped()), (0, 10));
    }

    #[test]
    fn ring_passes_every_frame_across_threads() {
        let r = std::sync::Arc::new(Ring::new(64));
        let p = r.clone();
        let t = std::thread::spawn(move || {
            for k in 0..20_000 {
                while !p.push([k as f32; 2]) {
                    std::thread::yield_now();
                }
            }
        });
        for k in 0..20_000 {
            let f = loop {
                if let Some(f) = r.pop() {
                    break f;
                }
                std::thread::yield_now();
            };
            assert_eq!(f, [k as f32; 2]);
        }
        t.join().unwrap();
    }

    #[test]
    fn advance_wraps_only_when_crossing_the_loop_end() {
        let lp = Some((1000.0, 2000.0));
        assert_eq!(advance(500.0, 100.0, lp), 600.0);
        assert_eq!(advance(1999.5, 1.0, lp), 1000.5); // overshoot carried, continues from start
        assert_eq!(advance(1500.0, 2600.0, lp), 1100.0); // several loops in one extrapolation
        assert_eq!(advance(2500.0, 10.0, lp), 2510.0); // seeked past the end: loop ignored, not deleted
        assert_eq!(advance(1500.0, 10.0, None), 1510.0);
        // before the loop but jumping over it whole still lands inside
        assert_eq!(advance(900.0, 1200.0, lp), 1100.0);
    }

    #[test]
    fn advance_backwards_wraps_at_the_loop_start() {
        let lp = Some((1000.0, 2000.0));
        assert_eq!(advance(1500.0, -100.0, lp), 1400.0);
        assert_eq!(advance(1000.5, -1.0, lp), 1999.5); // undershoot carried, continues from the end
        assert_eq!(advance(1000.0, -1.0, lp), 1999.0); // standing on the start already wraps
        assert_eq!(advance(1500.0, -2600.0, lp), 1900.0); // several loops
        assert_eq!(advance(500.0, -10.0, lp), 490.0); // before the loop: ignored
    }

    #[test]
    fn a_loop_shrinking_under_the_playhead_keeps_it_inside() {
        let old = Some((1000.0, 2000.0));
        let lag = 100.0;
        // halved with the playhead in the second half: same phase of the new loop, not out past its end
        assert_eq!(
            shrunk(old, Some((1000.0, 1500.0)), 1700.0, lag),
            Some(1200.0)
        );
        assert_eq!(
            shrunk(old, Some((1000.0, 1250.0)), 1999.0, lag),
            Some(1249.0)
        );
        // still inside the new loop, or the loop grew: nothing moves
        assert_eq!(shrunk(old, Some((1000.0, 1500.0)), 1200.0, lag), None);
        assert_eq!(shrunk(old, Some((1000.0, 4000.0)), 1700.0, lag), None);
        // a new loop whose end the written playhead already passed by less than the lag: wrapped in, same phase
        assert_eq!(
            shrunk(None, Some((1000.0, 1062.5)), 1080.0, lag),
            Some(1017.5)
        );
        // further past it, not out of the old loop (seeked past it): left alone, as `advance` does
        assert_eq!(shrunk(old, Some((1000.0, 1500.0)), 2500.0, lag), None);
        assert_eq!(shrunk(None, Some((1000.0, 1500.0)), 1700.0, lag), None);
        assert_eq!(shrunk(old, None, 1700.0, lag), None);
        // the same loop as before: nothing to do
        assert_eq!(shrunk(old, old, 2050.0, lag), None);
    }
}
