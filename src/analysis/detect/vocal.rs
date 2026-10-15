//! Regions of a track where a voice is likely (Track Analysis Setting "Vocal"). This is our own heuristic, not machine learning: **inferred**, measured on synthetic
//! signals only (the tests), never on real recordings. Expect false positives on other solo harmonic sources
//! (a mono lead) and misses on voices buried in a dense mix.
//!
//! Per 4096-sample frame (hop 2048, mono at about 22 kHz) the strongest harmonic series with a fundamental of
//! 85-450 Hz is looked for among the spectral peaks ([`frame_pitch`]): a voice puts a large share of the
//! 80 Hz - 3.4 kHz power on at least four harmonics of one pitch, and most of its power between 300 Hz and
//! 3.4 kHz (the formant band). A 1.5 s window is vocal when half of its frames are voiced and the pitch moves
//! (vibrato, glides and syllables do; a held synth chord does not) ([`regions`]). Gaps under 1 s are closed,
//! regions under 2 s dropped.

use crate::analysis::{Stft, mono_decimate};

const N: usize = 4096;
const HOP: usize = N / 2;
const TARGET_RATE: u32 = 22050;
/// Fundamentals searched (Hz), geometric step of 0.25 % (4 cents), and how many harmonics are summed.
const F0: (f32, f32) = (85.0, 450.0);
const F0_STEP: f32 = 1.0025;
const HARMONICS: usize = 10;
/// Band whose power the harmonic share is taken of, and the formant band (Hz).
const BAND: (f32, f32) = (80.0, 3400.0);
const FORMANTS: (f32, f32) = (300.0, 3400.0);
/// A frame is voiced when: the harmonic peaks (each at least a tenth of the strongest peak's power) hold at least
/// `MIN_SHARE` of the band's power, there are at least `MIN_PEAKS` of them, and the formant band holds at least
/// `MIN_FORMANT` of the power up to 8 kHz.
const MIN_SHARE: f32 = 0.3;
const MIN_PEAKS: usize = 4;
const MIN_FORMANT: f32 = 0.5;
/// A frame whose strongest bin is below this magnitude (about -60 dBFS for a sine) is silence.
const SILENCE: f32 = 1.0;
/// Window (frames each side of a frame), the voiced share of it and the pitch movement (std of the pitch in cents) it needs.
const HALF_WINDOW: usize = 8;
const MIN_VOICED: f32 = 0.5;
const MIN_CENTS: f32 = 10.0;
/// Gaps closed and regions dropped, in frames (about 1 s and 2 s).
const GAP: usize = 11;
const MIN_REGION: usize = 22;

pub use openfader_plugin::library::VocalRegion;

/// Fundamental (Hz) of the voice-like harmonic series in one frame's magnitude spectrum, `None` when it is not voiced.
fn frame_pitch(mag: &[f32], bin_hz: f32) -> Option<f32> {
    let bin = |hz: f32| (hz / bin_hz).round() as usize;
    let (lo, hi) = (bin(BAND.0), bin(BAND.1).min(mag.len() - 2));
    let pow = |k: usize| mag[k] * mag[k];
    let band: f32 = (lo..=hi).map(pow).sum();
    let loud = (lo..=hi).map(pow).fold(0.0, f32::max);
    if mag[lo..=hi].iter().fold(0.0, |m: f32, &v| m.max(v)) < SILENCE {
        return None;
    }
    let (voice, upto8k): (f32, f32) = (
        (bin(FORMANTS.0)..=hi).map(pow).sum(),
        (lo..=bin(8000.0).min(mag.len() - 1)).map(pow).sum(),
    );
    if voice < MIN_FORMANT * upto8k {
        return None;
    }
    let mut best = (0.0f32, 0usize, 0.0f32);
    let mut f = F0.0;
    while f <= F0.1 {
        let (mut sum, mut peaks) = (0.0, 0);
        for h in 1..=HARMONICS {
            let k = bin(f * h as f32);
            if k > hi {
                break;
            }
            let p = (k.saturating_sub(1)..=k + 1).map(pow).fold(0.0, f32::max);
            if p >= 0.1 * loud {
                sum += p;
                peaks += 1;
            }
        }
        if sum > best.0 {
            best = (sum, peaks, f);
        }
        f *= F0_STEP;
    }
    (best.0 >= MIN_SHARE * band && best.1 >= MIN_PEAKS).then_some(best.2)
}

/// Per frame pitch of the voiced frames of mono `x` at `rate`.
fn pitches(x: &[f32], rate: f32) -> Vec<Option<f32>> {
    let mut stft = Stft::new(N);
    x.windows(N)
        .step_by(HOP)
        .map(|frame| frame_pitch(stft.magnitudes(frame), rate / N as f32))
        .collect()
}

/// Vocal regions from the per-frame pitches (see the module doc); `frame_ms` is the hop in milliseconds.
fn regions(p: &[Option<f32>], frame_ms: f64) -> Vec<VocalRegion> {
    let n = p.len();
    let vocal: Vec<bool> = (0..n)
        .map(|i| {
            let w = &p[i.saturating_sub(HALF_WINDOW)..(i + HALF_WINDOW + 1).min(n)];
            let cents: Vec<f32> = w.iter().flatten().map(|&f| 1200.0 * f.log2()).collect();
            if (cents.len() as f32) < MIN_VOICED * w.len() as f32 {
                return false;
            }
            let mean = cents.iter().sum::<f32>() / cents.len() as f32;
            (cents.iter().map(|c| (c - mean) * (c - mean)).sum::<f32>() / cents.len() as f32).sqrt()
                >= MIN_CENTS
        })
        .collect();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if !vocal[i] {
            i += 1;
            continue;
        }
        let a = i;
        while i < n && vocal[i] {
            i += 1;
        }
        match runs.last_mut() {
            Some(last) if a - last.1 < GAP => last.1 = i,
            _ => runs.push((a, i)),
        }
    }
    let ms = |f: usize| (f as f64 * frame_ms).round() as u32;
    runs.into_iter()
        .filter(|r| r.1 - r.0 >= MIN_REGION)
        .map(|(a, b)| VocalRegion {
            start_ms: ms(a),
            end_ms: ms(b),
        })
        .collect()
}

/// Vocal regions of interleaved stereo f32 PCM (`rate` Hz). CPU light (a few hundred ms for a long track).
pub fn vocal_regions(samples: &[f32], rate: u32) -> Vec<VocalRegion> {
    let d = ((rate + TARGET_RATE / 2) / TARGET_RATE).max(1) as usize;
    let x = mono_decimate(samples, d);
    let r = rate as f32 / d as f32;
    regions(&pitches(&x, r), HOP as f64 / r as f64 * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 44100;

    /// A sung vowel: harmonics of a 220 Hz pitch with 5 Hz vibrato (+-40 cents), shaped by formants at 700 and 1200 Hz.
    fn voice(secs: usize) -> Vec<f32> {
        let mut phase = 0.0f32;
        (0..secs * RATE as usize)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                let f0 = 220.0 * 2f32.powf(0.4 * (TAU * 5.0 * t).sin() / 12.0);
                phase = (phase + TAU * f0 / RATE as f32) % TAU;
                let v: f32 = (1..=12)
                    .map(|h| {
                        let hz = f0 * h as f32;
                        let shape = (-((hz - 700.0) / 250.0).powi(2)).exp()
                            + 0.7 * (-((hz - 1200.0) / 300.0).powi(2)).exp();
                        (phase * h as f32).sin() * shape / h as f32
                    })
                    .sum::<f32>();
                [v * 0.4; 2]
            })
            .collect()
    }

    /// Kick-and-hat groove: 60 Hz thumps and noise-like 8 kHz ticks, 2 beats a second.
    fn drums(secs: usize) -> Vec<f32> {
        (0..secs * RATE as usize)
            .flat_map(|i| {
                let t = (i as f32 / RATE as f32 * 2.0).fract() / 2.0;
                let kick = (-t * 30.0).exp() * (TAU * 60.0 * t).sin();
                let hat = if t > 0.25 {
                    (-(t - 0.25) * 80.0).exp() * (TAU * 8000.0 * t).sin() * 0.3
                } else {
                    0.0
                };
                [(kick + hat) * 0.6; 2]
            })
            .collect()
    }

    /// A held A-minor chord (A2 C4 E4, six harmonics each, detuned pairs): harmonic but its pitch never moves.
    fn chord(secs: usize) -> Vec<f32> {
        (0..secs * RATE as usize)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                let v: f32 = [110.0f32, 261.63, 329.63]
                    .iter()
                    .map(|f| {
                        (1..=6)
                            .map(|h| (TAU * f * h as f32 * t).sin() / h as f32)
                            .sum::<f32>()
                    })
                    .sum();
                [v * 0.15; 2]
            })
            .collect()
    }

    #[test]
    fn a_sung_vowel_between_drums_is_one_region() {
        let track = [drums(8), voice(10), drums(8)].concat();
        let r = vocal_regions(&track, RATE);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(
            r[0].start_ms.abs_diff(8000) < 2000 && r[0].end_ms.abs_diff(18000) < 2000,
            "{r:?}"
        );
    }

    #[test]
    fn drums_and_a_held_chord_have_no_vocal() {
        assert!(vocal_regions(&drums(20), RATE).is_empty());
        assert!(vocal_regions(&chord(20), RATE).is_empty());
        assert!(vocal_regions(&vec![0.0; RATE as usize * 2 * 10], RATE).is_empty());
        assert!(vocal_regions(&[], RATE).is_empty());
    }

    #[test]
    fn regions_close_short_gaps_and_drop_short_runs() {
        // pitch wobbling +-30 cents each frame; None = unvoiced frame
        let wob = |i: usize| Some(220.0 * 2f32.powf(if i % 2 == 0 { 0.025 } else { -0.025 }));
        let mut p: Vec<Option<f32>> = (0..100).map(wob).collect();
        p[40..45].iter_mut().for_each(|f| *f = None); // a 5-frame gap inside
        p.extend((0..30).map(|_| None::<f32>));
        p.extend((0..12).map(wob)); // a run too short to count
        let r = regions(&p, 100.0);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(
            r[0].start_ms < 1000 && r[0].end_ms.abs_diff(10_000) <= 1000,
            "{r:?}"
        );
        let steady: Vec<Option<f32>> = (0..100).map(|_| Some(220.0)).collect();
        assert!(
            regions(&steady, 100.0).is_empty(),
            "a pitch that never moves is not a voice"
        );
    }
}
