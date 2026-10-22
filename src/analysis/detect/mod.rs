//! Our own analysis for audio without stored analysis: beat grid and 3-band waveforms from decoded PCM.
//! The design is ours.
//!
//! Input is stereo audio read through [`Frames`] (interleaved f32 as in [`crate::audio::Pcm`] `samples`, or a `Track16`) and its `rate`.
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
pub(crate) use vocal::vocal_regions_cancellable;
pub use vocal::{VocalRegion, vocal_regions};

use super::{DETAIL_ENTRIES_PER_SEC, PREVIEW_ENTRIES};
use crate::audio::{Frames, mono};

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

fn bands<S: Frames + ?Sized>(samples: &S, rate: u32) -> impl Iterator<Item = [f32; 3]> {
    let mut s = Split::new(rate);
    samples
        .frames_in(0..samples.frame_count())
        .map(move |f| s.push(mono(f)))
}

/// Mean-square energy per hop of each band `[low, mid, high]` at about `hz` hops per second, and the exact hops per second.
fn hops<S: Frames + ?Sized>(samples: &S, rate: u32, hz: u32) -> ([Vec<f32>; 3], f64) {
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
pub fn peaks<S: Frames + ?Sized>(samples: &S, rate: u32) -> Vec<[f32; 3]> {
    let n = samples.frame_count();
    peaks_of(samples.frames_in(0..n), n, rate)
}

/// [`peaks`] of `n` stereo frames read one by one (a mix computed on the fly needs no full-length buffer).
pub fn peaks_of(frames: impl Iterator<Item = [f32; 2]>, n: usize, rate: u32) -> Vec<[f32; 3]> {
    peaks_of_cancellable(frames, n, rate, &|| false).expect("uncancelled waveform peaks")
}

fn peaks_of_cancellable(
    frames: impl Iterator<Item = [f32; 2]>,
    n: usize,
    rate: u32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<[f32; 3]>> {
    if cancelled() {
        return None;
    }
    let cols = (n * DETAIL_ENTRIES_PER_SEC as usize).div_ceil(rate as usize);
    let mut peak = vec![[0f32; 3]; cols];
    let mut s = Split::new(rate);
    for (i, f) in frames.take(n).enumerate() {
        if i & 4095 == 0 && cancelled() {
            return None;
        }
        let b = s.push((f[0] + f[1]) * 0.5);
        let c = &mut peak[(i as u64 * DETAIL_ENTRIES_PER_SEC as u64 / rate as u64) as usize];
        for k in 0..3 {
            c[k] = c[k].max(b[k].abs());
        }
    }
    (!cancelled()).then_some(peak)
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
pub fn waveform<S: Frames + ?Sized>(samples: &S, rate: u32) -> (Vec<[u8; 3]>, Vec<[u8; 3]>) {
    let (detail, preview, _) = waveform_and_grid(samples, rate, None);
    (detail, preview)
}

/// One band split supplies waveform peaks and, when requested, grid hop energies.
pub(crate) fn waveform_and_grid<S: Frames + ?Sized>(
    samples: &S,
    rate: u32,
    opt: Option<&GridOptions>,
) -> (Vec<[u8; 3]>, Vec<[u8; 3]>, Vec<super::Beat>) {
    waveform_and_grid_cancellable(samples, rate, opt, &|| false)
        .expect("uncancelled waveform and grid analysis")
}

pub(crate) fn waveform_and_grid_cancellable<S: Frames + ?Sized>(
    samples: &S,
    rate: u32,
    opt: Option<&GridOptions>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<(Vec<[u8; 3]>, Vec<[u8; 3]>, Vec<super::Beat>)> {
    if cancelled() {
        return None;
    }
    let Some(opt) = opt else {
        let n = samples.frame_count();
        let peak = peaks_of_cancellable(samples.frames_in(0..n), n, rate, cancelled)?;
        let (detail, preview) = quantise(&peak, loudest(&peak));
        return (!cancelled()).then_some((detail, preview, Vec::new()));
    };
    let frames = samples.frame_count();
    let cols = (frames * DETAIL_ENTRIES_PER_SEC as usize).div_ceil(rate as usize);
    let mut peak = vec![[0f32; 3]; cols];
    let h = (rate / opt.flux_hz()).max(1) as usize;
    let mut energy = std::array::from_fn(|_| Vec::with_capacity(frames / h));
    let (mut acc, mut n) = ([0f32; 3], 0);
    for (i, b) in bands(samples, rate).enumerate() {
        if i & 4095 == 0 && cancelled() {
            return None;
        }
        let c = &mut peak[(i as u64 * DETAIL_ENTRIES_PER_SEC as u64 / rate as u64) as usize];
        for k in 0..3 {
            c[k] = c[k].max(b[k].abs());
            acc[k] += b[k] * b[k];
        }
        n += 1;
        if n == h {
            for k in 0..3 {
                energy[k].push(acc[k] / h as f32);
            }
            (acc, n) = ([0.0; 3], 0);
        }
    }
    if cancelled() {
        return None;
    }
    let (detail, preview) = quantise(&peak, loudest(&peak));
    let beats =
        grid::from_hops_cancellable(energy, rate as f64 / h as f64, frames, rate, opt, cancelled)?;
    (!cancelled()).then_some((detail, preview, beats))
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

    #[test]
    fn fused_waveform_grid_matches_independent_passes() {
        for rate in [8000, 44100, 48000] {
            let frames = rate as usize * 12 + 37;
            let mut samples: Vec<f32> = (0..frames)
                .flat_map(|i| {
                    let t = i as f32 / rate as f32;
                    let click = if i % (rate as usize / 2) < 20 {
                        0.8
                    } else {
                        0.0
                    };
                    let tone = (std::f32::consts::TAU * 713.0 * t).sin() * 0.07;
                    [tone + click, tone * 0.6 + click]
                })
                .collect();
            samples.push(0.9);
            let expected = waveform(&samples, rate);
            for mode in [Mode::Normal, Mode::Dynamic, Mode::Auto] {
                for precision in [false, true] {
                    let opt = GridOptions {
                        mode,
                        precision,
                        ..Default::default()
                    };
                    let (detail, preview, beats) = waveform_and_grid(&samples, rate, Some(&opt));
                    assert_eq!((&detail, &preview), (&expected.0, &expected.1));
                    let fields = |beats: Vec<super::super::Beat>| {
                        beats
                            .into_iter()
                            .map(|b| (b.time_ms, b.bpm_x100, b.bar_pos))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(fields(beats), fields(beat_grid_with(&samples, rate, &opt)));
                }
            }
            let (detail, preview, beats) = waveform_and_grid(&samples, rate, None);
            assert_eq!((detail, preview), expected);
            assert!(beats.is_empty());
        }
        for samples in [vec![], vec![0.5], vec![0.0; 91]] {
            let (detail, preview, beats) =
                waveform_and_grid(&samples, RATE, Some(&GridOptions::default()));
            assert_eq!((detail, preview), waveform(&samples, RATE));
            assert!(beats.is_empty());
        }
    }

    #[test]
    fn fused_band_scan_cancels_with_or_without_grid() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let samples = vec![0.0; 40_000];
        let opt = GridOptions::default();
        for grid in [None, Some(&opt)] {
            let polls = AtomicUsize::new(0);
            let cancelled = || polls.fetch_add(1, Ordering::Relaxed) >= 3;
            assert!(waveform_and_grid_cancellable(&samples, 8000, grid, &cancelled).is_none());
            assert!(polls.load(Ordering::Relaxed) > 1);
            assert!(waveform_and_grid_cancellable(&samples, 8000, grid, &|| true).is_none());
        }
    }

    #[test]
    #[ignore = "scaling benchmark; run optimized with --ignored --nocapture"]
    fn benchmark_fused_band_pass_scaling() {
        use std::{hint::black_box, time::Instant};
        for secs in [15, 60, 240] {
            let samples: Vec<f32> = (0..RATE as usize * secs)
                .flat_map(|i| {
                    let phase = i % (RATE as usize / 2);
                    let click = if phase < 90 {
                        (std::f32::consts::TAU * 1000.0 * phase as f32 / RATE as f32).sin()
                    } else {
                        0.0
                    };
                    [click, click]
                })
                .collect();
            for precision in [false, true] {
                let opt = GridOptions {
                    precision,
                    ..Default::default()
                };
                let start = Instant::now();
                let waveform = waveform(black_box(&samples), RATE);
                let beats = beat_grid_with(black_box(&samples), RATE, &opt);
                let independent_time = start.elapsed();
                let start = Instant::now();
                let fused = waveform_and_grid(black_box(&samples), RATE, Some(&opt));
                let fused_time = start.elapsed();
                assert_eq!((&fused.0, &fused.1), (&waveform.0, &waveform.1));
                let fields = |beats: Vec<super::super::Beat>| {
                    beats
                        .into_iter()
                        .map(|b| (b.time_ms, b.bpm_x100, b.bar_pos))
                        .collect::<Vec<_>>()
                };
                assert_eq!(fields(fused.2), fields(beats));
                eprintln!(
                    "band-pass secs={secs} precision={precision} independent={independent_time:?} fused={fused_time:?}"
                );
            }
        }
    }
}
