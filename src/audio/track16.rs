//! [`Track16`]: the track a deck's player (and the browser preview) keeps for the whole session, as 16-bit integers (half the
//! RAM of the f32 [`Pcm`] it is built from: 10.6 MB per minute of stereo at 44.1 kHz). The decoder can fill one directly
//! ([`super::decode_track16_cancellable`]), so a loaded track never exists as a full-length f32 buffer.

use super::{Frames, Pcm, Source, resample_at};
use std::ops::Range;

/// Stored value = sample × `SCALE`: the i16 range covers ±2.0, so a decoded file's overs up to +6 dBFS survive and a
/// normal track loses only the lowest bit (a step of 2⁻¹⁴ = -84 dBFS; ~90 dB SNR against the ±2.0 range, 0.5 LSB rounding error).
const SCALE: f32 = 16384.0;

/// Interleaved stereo at `rate` Hz as `i16` with 6 dB of headroom: sample `x` is stored as `round(x / 2 · 32768)` clamped to
/// the i16 range and read back as `v · 2 / 32768` (so ±2.0, the positive end 32767/16384). Immutable: built once from a
/// decoded [`Pcm`] by [`Track16::from`] or filled by the decoder, read through [`Source::frame_at`] (the same interpolation as
/// [`Pcm::frame_at`]), [`Track16::iter_f32`] or [`Frames`] (analysis). Sampler, edit and groove stay on f32 [`Pcm`].
#[derive(Default)]
pub struct Track16 {
    pub rate: u32,
    samples: Box<[i16]>,
}

#[inline]
fn deq(v: i16) -> f32 {
    v as f32 * (1.0 / SCALE)
}

/// The stored value of sample `x` (NaN becomes silence, overs past ±2.0 clip).
#[inline]
pub(super) fn quantise(x: f32) -> i16 {
    (x * SCALE).round() as i16
}

impl From<Pcm> for Track16 {
    /// Quantises `pcm` (dropped on return; NaN becomes silence, overs past ±2.0 clip).
    fn from(pcm: Pcm) -> Self {
        Self::from(&pcm)
    }
}

impl From<&Pcm> for Track16 {
    fn from(pcm: &Pcm) -> Self {
        let samples = pcm.samples.iter().map(|&x| quantise(x)).collect();
        Track16 {
            rate: pcm.rate,
            samples,
        }
    }
}

impl Track16 {
    /// A track of `rate` Hz from interleaved stereo `samples` already quantised with [`quantise`] (shrunk to fit).
    pub(super) fn new(rate: u32, samples: Vec<i16>) -> Self {
        Track16 {
            rate,
            samples: samples.into_boxed_slice(),
        }
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// Stereo frame at fractional source-frame position `pos`, interpolated exactly like [`Pcm::frame_at`]; None past the end.
    #[inline]
    pub fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        self.frame_at_step(pos, 1.0)
    }

    /// [`Track16::frame_at`] for a read that advances `step` source frames per output frame, like [`Pcm::frame_at_step`].
    #[inline]
    pub fn frame_at_step(&self, pos: f64, step: f64) -> Option<[f32; 2]> {
        resample_at(self.frames(), pos, step, |k, c| deq(self.samples[2 * k + c]))
    }

    /// The interleaved samples of frames `frames` as f32 (dequantised). Panics when the range is past the end.
    pub fn iter_f32(&self, frames: Range<usize>) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.samples[2 * frames.start..2 * frames.end]
            .iter()
            .map(|&v| deq(v))
    }

    /// Exact sample identity for in-process work sharing. Linear: call on a preparation worker, not the UI thread.
    pub fn content_digest(&self, cancelled: &dyn Fn() -> bool) -> Option<[u8; 32]> {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        // i16 has no padding; the native-endian byte view is only compared within this process.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self.samples.as_ptr().cast::<u8>(),
                std::mem::size_of_val(self.samples.as_ref()),
            )
        };
        for chunk in bytes.chunks(1 << 16) {
            if cancelled() {
                return None;
            }
            digest.update(chunk);
        }
        (!cancelled()).then(|| digest.finish().as_ref().try_into().expect("SHA-256"))
    }
}

impl Frames for Track16 {
    fn frame_count(&self) -> usize {
        self.frames()
    }

    fn frames_in(&self, range: Range<usize>) -> impl Iterator<Item = [f32; 2]> {
        self.samples[2 * range.start..2 * range.end]
            .chunks_exact(2)
            .map(|f| [deq(f[0]), deq(f[1])])
    }
}

impl Source for Track16 {
    #[inline]
    fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        Track16::frame_at(self, pos)
    }
    #[inline]
    fn frame_at_step(&self, pos: f64, step: f64) -> Option<[f32; 2]> {
        Track16::frame_at_step(self, pos, step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSB: f32 = 1.0 / SCALE;

    #[test]
    fn round_trip_is_within_one_lsb_and_keeps_overs() {
        let samples: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.37).sin() * 1.9).collect();
        let pcm = Pcm {
            rate: 44100,
            samples: samples.clone(),
        };
        let t = Track16::from(pcm);
        assert_eq!((t.rate, t.frames()), (44100, 1000));
        let back: Vec<f32> = t.iter_f32(0..1000).collect();
        assert!(
            samples.iter().zip(&back).all(|(a, b)| (a - b).abs() <= LSB),
            "within 1 LSB"
        );
        let overs = Track16::from(Pcm {
            rate: 1,
            samples: vec![1.99, -2.0, 1.5, -1.25, 9.0, f32::NAN],
        });
        let back: Vec<f32> = overs.iter_f32(0..3).collect();
        assert!((back[0] - 1.99).abs() <= LSB && back[1] == -2.0 && (back[2] - 1.5).abs() <= LSB);
        assert!((back[3] + 1.25).abs() <= LSB, "a +6 dBFS over is kept");
        assert!(
            back[4] > 1.999 && back[5] == 0.0,
            "clips at the range end, NaN is silent"
        );
    }

    #[test]
    fn frame_at_matches_pcm_frame_at() {
        let samples: Vec<f32> = (0..64).map(|i| (i as f32 * 0.9).sin() * 1.5).collect();
        let pcm = Pcm {
            rate: 44100,
            samples,
        };
        let t = Track16::from(pcm.clone());
        for n in 0..70 {
            let pos = n as f64 * 0.5;
            match (pcm.frame_at(pos), t.frame_at(pos)) {
                (Some(a), Some(b)) => assert!(
                    // the kernel's weights sum to 1 but not their magnitudes: each stored sample's half-LSB error can add up
                    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 2.0 * LSB),
                    "at {pos}"
                ),
                (a, b) => assert_eq!(a, b, "at {pos}"),
            }
        }
        assert_eq!(t.frame_at(32.0), None);
    }

    #[test]
    fn content_digest_matches_independent_decodes_and_checks_every_chunk() {
        let pcm = Pcm {
            rate: 44100,
            samples: vec![0.5; 1 << 17],
        };
        let a = Track16::from(&pcm);
        let b = Track16::from(&pcm);
        assert_eq!(a.content_digest(&|| false), b.content_digest(&|| false));
        let mut changed = pcm;
        *changed.samples.last_mut().unwrap() = -0.5;
        assert_ne!(
            a.content_digest(&|| false),
            Track16::from(changed).content_digest(&|| false)
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            a.content_digest(&|| {
                calls.set(calls.get() + 1);
                calls.get() == 2
            })
            .is_none()
        );
        assert_eq!(calls.get(), 2);
        assert!(Track16::default().content_digest(&|| true).is_none());
    }
}
