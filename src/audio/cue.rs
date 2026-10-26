//! Second output device for the headphones: `OPENFADER_CUE_DEVICE=<substring of a cpal output device name>`.
//!
//! The main callback pushes the stereo headphone feed ([`super::bus::Out::phones`]) into a lock-free
//! [`Ring`]; this device's own callback pops it ([`Feed::fill`]). Two clocks never agree exactly, so the
//! ring fill drifts. Window (inferred, tuned by reasoning, not measured on hardware), for a consumer
//! buffer of `n` frames: the feed starts once the fill reaches 2n (before that, and after any
//! underrun, it plays silence and re-primes); each callback with the fill under 1.5n repeats one frame,
//! over 4n drops one. ponytail: dropping/duplicating a single frame is an audible micro-glitch (a click
//! on tonal material) at up to one per callback; upgrade = a drift-tracking resampler. The device must
//! run at the main stream's rate (checked at open); a mismatching device is refused, not resampled.

use super::{Ring, find_output, put_frame};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SampleRate, SizedSample, Stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Env: substring of the second (headphone) output device's name.
pub const ENV: &str = "OPENFADER_CUE_DEVICE";

/// ~0.7 s at 48 kHz: far above the window, so only a stalled consumer overflows it.
const RING_FRAMES: usize = 1 << 15;

/// Consumer side of the headphone ring: turns the drifting fill into a steady stream of frames.
pub struct Feed {
    ring: Arc<Ring>,
    last: [f32; 2],
    primed: bool,
}

impl Feed {
    pub fn new(ring: Arc<Ring>) -> Feed {
        Feed {
            ring,
            last: [0.0; 2],
            primed: false,
        }
    }

    /// Hands `n` frames to `put(i, frame)`, correcting the ring fill by at most one frame (see the module doc).
    pub fn fill(&mut self, n: usize, mut put: impl FnMut(usize, [f32; 2])) {
        let fill = self.ring.len();
        if !self.primed && fill >= 2 * n {
            self.primed = true;
        }
        if !self.primed {
            (0..n).for_each(|i| put(i, [0.0; 2]));
            return;
        }
        let mut drop_one = fill > 4 * n;
        let mut repeat = fill < n * 3 / 2;
        for i in 0..n {
            if std::mem::take(&mut drop_one) {
                self.ring.pop();
            }
            if i == 0 && std::mem::take(&mut repeat) {
                put(0, self.last);
                continue;
            }
            match self.ring.pop() {
                Some(f) => {
                    self.last = f;
                    put(i, f);
                }
                None => {
                    self.primed = false; // underrun: silence until it refills
                    (i..n).for_each(|i| put(i, [0.0; 2]));
                    return;
                }
            }
        }
    }
}

/// An open second device: keep `stream` alive, push frames into `ring`.
pub struct Cue {
    pub ring: Arc<Ring>,
    pub name: String,
    pub _stream: Stream,
    pub(crate) failed: Arc<AtomicBool>,
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    dev: &cpal::Device,
    cfg: &cpal::StreamConfig,
    mut feed: Feed,
    failed: Arc<AtomicBool>,
) -> Result<Stream, cpal::BuildStreamError> {
    let ch = cfg.channels as usize;
    dev.build_output_stream(
        cfg,
        move |o: &mut [T], _| {
            feed.fill(o.len() / ch, |i, f| {
                put_frame(&mut o[i * ch..(i + 1) * ch], f)
            })
        },
        move |_| failed.store(true, Relaxed),
        None,
    )
}

/// Opens the first output device whose name contains `want` (case-insensitive) at `rate` Hz and starts it.
/// Errors name the available devices; nothing falls back to another device.
pub fn open(want: &str, rate: u32) -> Result<Cue, String> {
    let (name, dev) = find_output(want, ENV)?;
    open_device(name, dev, rate)
}

/// Opens an already resolved device, without enumerating the host on the stream-owning thread.
pub(crate) fn open_device(name: String, dev: cpal::Device, rate: u32) -> Result<Cue, String> {
    let sup = dev
        .supported_output_configs()
        .map_err(|e| format!("{ENV}: {name}: {e}"))?
        .filter(|c| {
            c.channels() >= 2 && (c.min_sample_rate().0..=c.max_sample_rate().0).contains(&rate)
        })
        .max_by_key(|c| c.sample_format() == SampleFormat::F32)
        .ok_or_else(|| {
            format!("{ENV}: {name} has no stereo output at {rate} Hz (the main device's rate)")
        })?
        .with_sample_rate(SampleRate(rate));
    let fmt = sup.sample_format();
    let cfg: cpal::StreamConfig = sup.into();
    let ring = Arc::new(Ring::new(RING_FRAMES));
    let feed = Feed::new(ring.clone());
    let failed = Arc::new(AtomicBool::new(false));
    let stream = match fmt {
        SampleFormat::F32 => build::<f32>(&dev, &cfg, feed, failed.clone()),
        SampleFormat::I16 => build::<i16>(&dev, &cfg, feed, failed.clone()),
        SampleFormat::U16 => build::<u16>(&dev, &cfg, feed, failed.clone()),
        f => return Err(format!("{ENV}: {name}: unsupported sample format {f:?}")),
    }
    .map_err(|e| format!("{ENV}: {name}: {e}"))?;
    stream.play().map_err(|e| format!("{ENV}: {name}: {e}"))?;
    Ok(Cue {
        ring,
        name,
        _stream: stream,
        failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ring holding frames `[k, k]` for k in `from..from + count`.
    fn ramp(from: usize, count: usize) -> Arc<Ring> {
        let r = Arc::new(Ring::new(1 << 12));
        (from..from + count).for_each(|k| assert!(r.push([k as f32; 2])));
        r
    }

    fn pull(f: &mut Feed, n: usize) -> Vec<f32> {
        let mut out = vec![f32::NAN; n];
        f.fill(n, |i, [l, _]| out[i] = l);
        out
    }

    #[test]
    fn in_window_plays_contiguously() {
        let mut f = Feed::new(ramp(0, 300)); // n = 100: fill 300 = 3n, inside 1.5n..4n
        assert_eq!(
            pull(&mut f, 100),
            (0..100).map(|k| k as f32).collect::<Vec<_>>()
        );
        assert_eq!(f.ring.len(), 200);
    }

    #[test]
    fn overfull_ring_drops_one_frame() {
        let mut f = Feed::new(ramp(0, 450)); // fill 4.5n > 4n
        let out = pull(&mut f, 100);
        assert_eq!(out[0], 1.0, "frame 0 dropped"); // skipped 0, then contiguous
        assert!(out.windows(2).all(|w| w[1] == w[0] + 1.0));
        assert_eq!(f.ring.len(), 450 - 101);
    }

    #[test]
    fn underfull_ring_repeats_one_frame() {
        let mut f = Feed::new(ramp(0, 200));
        pull(&mut f, 100); // fill 200 -> 100 left, below 1.5n
        let out = pull(&mut f, 100);
        assert_eq!(out[0], 99.0, "last frame repeated"); // 99 was the previous callback's last frame
        assert_eq!(out[1], 100.0);
        assert_eq!(
            f.ring.len(),
            100 - 99,
            "one frame less consumed than asked for"
        );
    }

    #[test]
    fn silent_until_primed_and_after_underrun() {
        let mut f = Feed::new(ramp(0, 150)); // 1.5n: not yet 2n
        assert_eq!(pull(&mut f, 100), vec![0.0; 100]);
        assert_eq!(f.ring.len(), 150, "nothing consumed while priming");
        (150..250).for_each(|k| assert!(f.ring.push([k as f32; 2])));
        assert_eq!(pull(&mut f, 100)[99], 99.0); // primed at 250 >= 2n
        pull(&mut f, 100); // 150 left, plays 100..199
        // 50 left: one repeated frame + all 50 real ones, then dry: the rest is silence and it re-primes
        let out = pull(&mut f, 100);
        assert!(out[..51].iter().all(|x| *x != 0.0) && out[51..].iter().all(|x| *x == 0.0));
        assert_eq!(pull(&mut f, 100), vec![0.0; 100]);
    }

    /// Producer pushes `ratio` x the consumer's frames per tick for 20 000 ticks; returns (fill range seen, silent frames, dropped pushes).
    fn drift(ratio: f64) -> ((usize, usize), usize, u64) {
        let r = Arc::new(Ring::new(1 << 12));
        let mut f = Feed::new(r.clone());
        let n = 480;
        (0..2 * n).for_each(|_| {
            r.push([1.0; 2]);
        });
        let (mut carry, mut range, mut silent) = (0.0, (usize::MAX, 0), 0);
        for _ in 0..20_000 {
            carry += n as f64 * ratio;
            let m = carry as usize;
            carry -= m as f64;
            (0..m).for_each(|_| {
                r.push([1.0; 2]);
            });
            range = (range.0.min(r.len()), range.1.max(r.len()));
            f.fill(n, |_, [l, _]| silent += (l == 0.0) as usize);
        }
        (range, silent, r.dropped())
    }

    #[test]
    fn single_frame_corrections_hold_the_fill_against_drift() {
        // 0.1% drift either way (one correction per 480-frame callback absorbs up to 0.2%)
        for ratio in [1.001, 0.999] {
            let ((lo, hi), silent, dropped) = drift(ratio);
            assert!(
                lo >= 480 * 3 / 2 - 2 && hi <= 4 * 480 + 481,
                "ratio {ratio}: fill {lo}..{hi}"
            );
            assert_eq!((silent, dropped), (0, 0), "ratio {ratio}");
        }
    }
}
