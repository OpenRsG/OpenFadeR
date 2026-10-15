//! Our own analysis for audio without stored analysis: beat grid and 3-band waveforms from decoded PCM.
//! The design is ours.
//!
//! Input is interleaved stereo f32 as in [`crate::audio::Pcm`] (`samples`, `rate`).
//!
//! This base owns the band split ([`hops`]: mean-square energy per band every 2.5 ms, or 1 ms with high
//! precision) and the waveform. Children: [`grid`] (`beat_grid_with`: tempo, phase, bar positions, tempo changes) and
//! [`vocal`] (regions where a voice is likely).
//!
//! Waveform: the band split is ours (one-pole filters at 200 Hz / 2.5 kHz), heights are per-column
//! peaks ([`peaks`]) scaled by one `top` to 0..=127 (the range of the stored 3-band waveform, [`quantise`]; [`waveform`] takes the
//! track's own loudest peak, the STEM waveform the original's so a muted part shows as a lower wave), laid out as
//! `[low, mid, high]` like the stored 3-band waveform.

mod grid;
mod vocal;

pub use grid::{GridOptions, Mode, beat_grid, beat_grid_with};
pub use vocal::{VocalRegion, vocal_regions};

use super::{DETAIL_ENTRIES_PER_SEC, PREVIEW_ENTRIES};

const FLUX_HZ: u32 = 400;
/// Flux rate of high precision BeatGrid analysis.
const PRECISE_FLUX_HZ: u32 = 1000;
const BAND_HZ: [f32; 2] = [200.0, 2500.0];

/// One-pole low-pass.
struct Pole {
    a: f32,
    y: f32,
}

impl Pole {
    fn new(hz: f32, rate: u32) -> Self {
        Pole {
            a: 1.0 - (-std::f32::consts::TAU * hz / rate as f32).exp(),
            y: 0.0,
        }
    }
    fn lp(&mut self, x: f32) -> f32 {
        self.y += self.a * (x - self.y);
        self.y
    }
}

/// Mono → `[low, mid, high]`: low = 2 low-passes at 200 Hz, mid = 2 high-passes at 200 Hz then
/// 2 low-passes at 2.5 kHz, high = 2 high-passes at 2.5 kHz. Independent chains (not a complementary
/// split) so a tone far from a corner leaks little into the other bands.
struct Split([Pole; 8]);

impl Split {
    fn new(rate: u32) -> Self {
        let [lo, hi] = BAND_HZ;
        Split([lo, lo, lo, lo, hi, hi, hi, hi].map(|hz| Pole::new(hz, rate)))
    }
    fn push(&mut self, x: f32) -> [f32; 3] {
        let p = &mut self.0;
        let low = p[0].lp(x);
        let low = p[1].lp(low);
        let h = x - p[2].lp(x);
        let h = h - p[3].lp(h);
        let mid = p[4].lp(h);
        let mid = p[5].lp(mid);
        let g = x - p[6].lp(x);
        let high = g - p[7].lp(g);
        [low, mid, high]
    }
}

fn bands(samples: &[f32], rate: u32) -> impl Iterator<Item = [f32; 3]> + '_ {
    let mut s = Split::new(rate);
    samples
        .chunks_exact(2)
        .map(move |f| s.push((f[0] + f[1]) * 0.5))
}

/// Mean-square energy per hop of each band `[low, mid, high]` at about `hz` hops per second, and the exact hops per second.
fn hops(samples: &[f32], rate: u32, hz: u32) -> ([Vec<f32>; 3], f64) {
    let h = (rate / hz).max(1) as usize;
    let (mut out, mut acc, mut n) = ([Vec::new(), Vec::new(), Vec::new()], [0f32; 3], 0);
    for b in bands(samples, rate) {
        for k in 0..3 {
            acc[k] += b[k] * b[k];
        }
        n += 1;
        if n == h {
            for k in 0..3 {
                out[k].push(acc[k] / h as f32);
            }
            (acc, n) = ([0.0; 3], 0);
        }
    }
    (out, rate as f64 / h as f64)
}

/// Per column ([`DETAIL_ENTRIES_PER_SEC`] per second) peak of each band `[low, mid, high]`, unscaled. Empty for empty input.
pub fn peaks(samples: &[f32], rate: u32) -> Vec<[f32; 3]> {
    peaks_of(
        samples.chunks_exact(2).map(|f| [f[0], f[1]]),
        samples.len() / 2,
        rate,
    )
}

/// [`peaks`] of `n` stereo frames read one by one (a mix computed on the fly needs no full-length buffer).
pub fn peaks_of(frames: impl Iterator<Item = [f32; 2]>, n: usize, rate: u32) -> Vec<[f32; 3]> {
    let cols = (n * DETAIL_ENTRIES_PER_SEC as usize).div_ceil(rate as usize);
    let mut peak = vec![[0f32; 3]; cols];
    let mut s = Split::new(rate);
    for (i, f) in frames.take(n).enumerate() {
        let b = s.push((f[0] + f[1]) * 0.5);
        let c = &mut peak[(i as u64 * DETAIL_ENTRIES_PER_SEC as u64 / rate as u64) as usize];
        for k in 0..3 {
            c[k] = c[k].max(b[k].abs());
        }
    }
    peak
}

/// The loudest band peak of `peaks`: the usual `top` of [`quantise`].
pub fn loudest(peaks: &[[f32; 3]]) -> f32 {
    peaks.iter().flatten().fold(0f32, |m, &v| m.max(v))
}

/// `(detail, preview)` from [`peaks`]: [`DETAIL_ENTRIES_PER_SEC`] columns/s and [`PREVIEW_ENTRIES`] columns over the
/// whole track, both `[low, mid, high]` heights as in the stored 3-band waveform. Our choice of scale: one
/// factor for all bands so `top` is 127, inside the fixed 0..128 range (louder peaks clip there).
/// Empty for empty input.
pub fn quantise(peak: &[[f32; 3]], top: f32) -> (Vec<[u8; 3]>, Vec<[u8; 3]>) {
    let cols = peak.len();
    let scale = if top > 0.0 { 127.0 / top } else { 0.0 };
    let detail: Vec<[u8; 3]> = peak
        .iter()
        .map(|b| b.map(|v| (v * scale).round().min(127.0) as u8))
        .collect();
    let preview = if cols == 0 {
        Vec::new()
    } else {
        (0..PREVIEW_ENTRIES)
            .map(|j| {
                let a = j * cols / PREVIEW_ENTRIES;
                let b = ((j + 1) * cols / PREVIEW_ENTRIES).clamp(a + 1, cols);
                detail[a..b]
                    .iter()
                    .fold([0u8; 3], |m, d| [0, 1, 2].map(|k| m[k].max(d[k])))
            })
            .collect()
    };
    (detail, preview)
}

/// `(detail, preview)` of the audio, the loudest peak being 127 ([`peaks`], [`quantise`]).
pub fn waveform(samples: &[f32], rate: u32) -> (Vec<[u8; 3]>, Vec<[u8; 3]>) {
    let p = peaks(samples, rate);
    quantise(&p, loudest(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44100;

    #[test]
    fn bands_split_by_frequency() {
        let tone = |hz: f32| -> Vec<f32> {
            (0..RATE as usize * 2)
                .flat_map(|i| {
                    [(std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin() * 0.5; 2]
                })
                .collect()
        };
        let (d, p) = waveform(&tone(60.0), RATE);
        assert_eq!(d.len(), 2 * DETAIL_ENTRIES_PER_SEC as usize);
        assert_eq!(p.len(), PREVIEW_ENTRIES);
        let [low, mid, high] = d[200];
        assert!(
            low > 100 && mid < 40 && high < 5,
            "60 Hz: low {low} mid {mid} high {high}"
        );
        let (d, _) = waveform(&tone(8000.0), RATE);
        let [low, mid, high] = d[200];
        assert!(
            high > 100 && mid < 40 && low < 5,
            "8 kHz: low {low} mid {mid} high {high}"
        );
        let (d, _) = waveform(&tone(1000.0), RATE);
        let [low, mid, high] = d[200];
        assert!(
            mid > 100 && high < 60 && low < 20,
            "1 kHz: low {low} mid {mid} high {high}"
        );
    }
}
