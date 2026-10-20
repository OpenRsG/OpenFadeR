//! [`Track16`]: the track a deck's player keeps for the whole session, as 16-bit integers (half the RAM of the f32 [`Pcm`]
//! it is built from: 10.6 MB per minute of stereo at 44.1 kHz).

use super::{Pcm, Source, lerp_at};
use std::ops::Range;

/// Stored value = sample × `SCALE`: the i16 range covers ±2.0, so a decoded file's overs up to +6 dBFS survive and a
/// normal track loses only the lowest bit (a step of 2⁻¹⁴ = -84 dBFS; ~90 dB SNR against the ±2.0 range, 0.5 LSB rounding error).
const SCALE: f32 = 16384.0;

/// Interleaved stereo at `rate` Hz as `i16` with 6 dB of headroom: sample `x` is stored as `round(x / 2 · 32768)` clamped to
/// the i16 range and read back as `v · 2 / 32768` (so ±2.0, the positive end 32767/16384). Immutable: built once from a
/// decoded [`Pcm`] by [`Track16::from`], read through [`Source::frame_at`] (the same interpolation as [`Pcm::frame_at`]) or
/// [`Track16::iter_f32`]. Analysis and transient work (sampler, edit, preview) stay on f32 [`Pcm`].
#[derive(Default)]
pub struct Track16 {
    pub rate: u32,
    samples: Box<[i16]>,
}

#[inline]
fn deq(v: i16) -> f32 {
    v as f32 * (1.0 / SCALE)
}

impl From<Pcm> for Track16 {
    /// Quantises `pcm` (dropped on return; NaN becomes silence, overs past ±2.0 clip).
    fn from(pcm: Pcm) -> Self {
        Self::from(&pcm)
    }
}

impl From<&Pcm> for Track16 {
    fn from(pcm: &Pcm) -> Self {
        let samples = pcm
            .samples
            .iter()
            .map(|&x| (x * SCALE).round() as i16)
            .collect();
        Track16 {
            rate: pcm.rate,
            samples,
        }
    }
}

impl Track16 {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// Stereo frame at fractional source-frame position `pos`, linearly interpolated exactly like [`Pcm::frame_at`]; None past the end.
    #[inline]
    pub fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        lerp_at(self.frames(), pos, |k, c| deq(self.samples[2 * k + c]))
    }

    /// The interleaved samples of frames `frames` as f32 (dequantised). Panics when the range is past the end.
    pub fn iter_f32(&self, frames: Range<usize>) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.samples[2 * frames.start..2 * frames.end]
            .iter()
            .map(|&v| deq(v))
    }
}

impl Source for Track16 {
    #[inline]
    fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        Track16::frame_at(self, pos)
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
                    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= LSB),
                    "at {pos}"
                ),
                (a, b) => assert_eq!(a, b, "at {pos}"),
            }
        }
        assert_eq!(t.frame_at(32.0), None);
    }
}
