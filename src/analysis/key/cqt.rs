//! The NEURAL key detector's front end (own code): the constant-Q spectrogram the network was trained on.
//!
//! Input of the published network (documented by its training scripts, MIT): mono audio at 44.1 kHz, a constant-Q
//! transform of 105 bins at 24 per octave from 65 Hz (a quarter-tone grid, 65 Hz to 1.3 kHz), one frame per 8820 samples
//! (0.2 s, frames centred on `t * hop`), Hann windows of `Q = 1 / (2^(1/24) - 1)` periods, magnitude `sqrt(len) * |sum x w e^-jwt| / sum w`
//! (so a sine of amplitude `A` reads `sqrt(len) A / 2`), then `ln(1 + magnitude)`.
//!
//! Own approach: every bin is a direct windowed correlation (no octave-wise resampling and no FFT kernels). The audio is
//! first decimated by an integer to about 11 kHz (the top bin is 1.3 kHz) with the triangular filter [`super::super::mono_decimate_cancellable`] uses, and each kernel
//! is divided by that filter's gain at its own frequency; the kernels are built for the decimated rate, so a 48 kHz or
//! 96 kHz track needs no non-integer resampling. The last frame (the one at the end of the audio) is not produced.

use crate::analysis::mono_decimate_cancellable;
use crate::audio::Frames;
use std::f32::consts::{PI, TAU};

/// Frequency bins of one frame.
pub const BINS: usize = 105;
const PER_OCTAVE: f32 = 24.0;
const FMIN: f32 = 65.0;
/// Seconds between frames.
const HOP_S: f32 = 0.2;
/// The audio is decimated to the highest integer division that stays above this rate.
const TARGET_RATE: u32 = 11_000;
/// Slower audio has no room for the highest bin.
const MIN_RATE: u32 = 4_000;

/// The spectrogram of a track: `data[bin * frames + t]`, bin 0 = 65 Hz.
pub struct Spec {
    pub frames: usize,
    pub data: Vec<f32>,
    /// The largest magnitude before the logarithm: 0 for silence.
    pub peak: f32,
}

/// One bin's windowed complex exponential (conjugated), centred on `half` samples before the frame's centre.
struct Kernel {
    half: usize,
    re: Vec<f32>,
    im: Vec<f32>,
}

/// Gain of the triangular `d`-fold decimation filter at `f` Hz of a `rate` Hz signal.
fn decimation_gain(f: f32, rate: f32, d: usize) -> f32 {
    if d == 1 {
        return 1.0;
    }
    let g = (PI * f * d as f32 / rate).sin() / (d as f32 * (PI * f / rate).sin());
    g * g
}

fn kernels(rate: u32, d: usize) -> Vec<Kernel> {
    let (full, rate) = (rate as f32, (rate / d as u32) as f32);
    let q = 1.0 / (2f32.powf(1.0 / PER_OCTAVE) - 1.0);
    (0..BINS)
        .map(|k| {
            let f = FMIN * 2f32.powf(k as f32 / PER_OCTAVE);
            let len = (q * rate / f).ceil() as usize;
            let half = len / 2;
            let w: Vec<f32> = (0..len)
                .map(|n| 0.5 - 0.5 * (TAU * n as f32 / len as f32).cos())
                .collect();
            let scale = (len as f32).sqrt() / w.iter().sum::<f32>() / decimation_gain(f, full, d);
            let (mut re, mut im) = (Vec::with_capacity(len), Vec::with_capacity(len));
            for (n, w) in w.iter().enumerate() {
                let (s, c) = (TAU * f / rate * (n as f32 - half as f32)).sin_cos();
                (re.push(w * c * scale), im.push(-w * s * scale));
            }
            Kernel { half, re, im }
        })
        .collect()
}

/// Dot product in eight lanes (a plain sum does not vectorise: float addition is not associative).
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca
        .remainder()
        .iter()
        .zip(cb.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + tail
}

/// The log constant-Q spectrogram of stereo audio at `rate` Hz; `None` when `cancelled`. No frames for audio shorter than
/// one hop or a `rate` below [`MIN_RATE`].
pub fn log_cqt<S: Frames + ?Sized>(
    samples: &S,
    rate: u32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Spec> {
    let d = (rate / TARGET_RATE).max(1) as usize;
    let none = Spec {
        frames: 0,
        data: Vec::new(),
        peak: 0.0,
    };
    if rate < MIN_RATE {
        return Some(none);
    }
    let x = mono_decimate_cancellable(samples, d, cancelled)?;
    let hop = (HOP_S * rate as f32 / d as f32).round() as usize;
    let frames = x.len() / hop;
    if frames == 0 {
        return Some(none);
    }
    let ker = kernels(rate, d);
    let (mut data, mut peak) = (vec![0.0f32; BINS * frames], 0.0f32);
    for t in 0..frames {
        if t % 16 == 0 && cancelled() {
            return None;
        }
        let centre = t * hop;
        for (bin, k) in ker.iter().enumerate() {
            let start = centre as isize - k.half as isize;
            let (lo, hi) = (
                start.max(0) as usize,
                ((start + k.re.len() as isize) as usize).min(x.len()),
            );
            let (seg, off) = (&x[lo..hi], (lo as isize - start) as usize);
            let m = dot(seg, &k.re[off..]).hypot(dot(seg, &k.im[off..]));
            peak = peak.max(m);
            data[bin * frames + t] = m.ln_1p();
        }
    }
    Some(Spec { frames, data, peak })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stereo sine of `hz` and amplitude `a`, `secs` long.
    fn sine(hz: f32, a: f32, rate: u32, secs: f32) -> Vec<f32> {
        (0..(secs * rate as f32) as usize)
            .flat_map(|i| {
                let v = a * (TAU * hz * i as f32 / rate as f32).sin();
                [v, v]
            })
            .collect()
    }

    fn column(s: &Spec, t: usize) -> Vec<f32> {
        (0..BINS).map(|b| s.data[b * s.frames + t]).collect()
    }

    #[test]
    fn a_sine_lands_on_its_bin_with_the_training_scale_at_any_sample_rate() {
        // 437.3 Hz is bin 66 of the grid: 65 * 2^(66/24).
        let f = FMIN * 2f32.powf(66.0 / PER_OCTAVE);
        for rate in [44_100, 48_000, 22_050] {
            let s = log_cqt(&sine(f, 0.5, rate, 4.0), rate, &|| false).unwrap();
            assert_eq!(
                s.frames, 20,
                "one frame per 0.2 s, the last one dropped, at {rate}"
            );
            let c = column(&s, 10);
            let top = (0..BINS).max_by(|&a, &b| c[a].total_cmp(&c[b])).unwrap();
            assert_eq!(top, 66, "{rate}");
            // sqrt(len) * A / 2 with len = ceil(Q rate' / f) samples of the decimated signal.
            let d = (rate / TARGET_RATE) as f32;
            let q = 1.0 / (2f32.powf(1.0 / PER_OCTAVE) - 1.0);
            let want = (q * rate as f32 / d / f).ceil().sqrt() * 0.25;
            let got = c[66].exp_m1();
            assert!((got / want - 1.0).abs() < 0.03, "{rate}: {got} vs {want}");
            assert!(c[66] > 8.0 * c[40], "far bins stay quiet");
        }
    }

    #[test]
    fn silence_and_too_short_audio_have_no_energy_or_no_frames() {
        let s = log_cqt(&vec![0.0f32; 2 * 44_100 * 3], 44_100, &|| false).unwrap();
        assert_eq!((s.frames, s.peak), (15, 0.0));
        assert_eq!(
            log_cqt(&sine(440.0, 0.5, 44_100, 0.1), 44_100, &|| false)
                .unwrap()
                .frames,
            0
        );
        assert!(log_cqt(&sine(440.0, 0.5, 44_100, 3.0), 44_100, &|| true).is_none());
    }
}
