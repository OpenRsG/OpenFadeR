//! Track analysis (base): beat grid, cues, waveforms, key, vocal regions and phrases of a track. [`Analysis`] is the SDK's
//! type (one definition for the host and the library plugins, which supply the analysis their libraries hold); this base owns
//! the STFT and decimation that [`key`] and the vocal detector share. Children:
//! - [`colors`]: the neutral colour helpers (cue colour picker, default pad colours, colour-detail entries)
//! - [`detect`]: our own beat grid and 3-band waveforms from decoded PCM ([`analyze`])
//! - [`phrase_edit`]: manual phrase edits (cut, type, boundary, clear) as pure functions on [`Phrases`]
//! - [`key`]: musical key (Camelot) from decoded PCM, also part of [`analyze`]
//! - [`auto`]: our own phrases (from the beat grid + 3-band waveform) and suggested cues
//! - [`cache`]: our analysis results on disk, user data named by the track's identity (moves with the track)
//! - [`options`]: what [`analyze`] does (Preferences > Analysis); [`tag`]: writing the detected key into the audio file's tag
//! - [`similar`]: Track Suggestion's measurements (timbre / energy [`similar::Features`], year and composer tags, a background profile index)

pub mod auto;
pub mod cache;
mod colors;
pub mod detect;
pub mod fingerprint;
pub mod key;
pub mod options;
pub mod phrase_edit;
pub mod similar;
pub mod tag;

pub use colors::{COLORFUL, color_detail_rgbh, picker};
pub use openfader_plugin::library::{
    Analysis, Beat, Cue, DETAIL_ENTRIES_PER_SEC, Kind as PhraseKind, Mood, PREVIEW_ENTRIES, Phrase,
    Phrases,
};

/// Beat grid, 3-band waveforms, musical key, vocal regions and phrases (`beats`, `three_band_detail`,
/// `three_band_preview`, `key`, `vocal`, `phrases` from [`auto::phrases`]) computed from interleaved stereo f32 PCM
/// (`audio::Pcm::samples`, `.rate`) under the current [`options`]; a part switched off stays empty, other fields stay
/// empty. CPU heavy (seconds for a long track): call off the UI thread.
pub fn analyze(samples: &[f32], rate: u32) -> Analysis {
    analyze_with(samples, rate, &options::get())
}

/// [`analyze`] under `o`, with parallel key and vocal detection for interactive loads.
pub fn analyze_with(samples: &[f32], rate: u32, o: &options::Options) -> Analysis {
    analyze_cancellable(samples, rate, o, Execution::Interactive, &|| false).unwrap()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    Interactive,
    /// The caller already owns a worker from the process-wide bulk budget.
    Bulk,
}

/// Cancellation discards the result between detector stages; no partial analysis is returned or stored.
pub fn analyze_cancellable(
    samples: &[f32],
    rate: u32,
    o: &options::Options,
    execution: Execution,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Analysis> {
    if cancelled() {
        return None;
    }
    let key = || {
        if o.parts.key {
            key::detect_cancellable(samples, rate, cancelled)
        } else {
            Some(None)
        }
    };
    let vocal = || {
        if o.parts.vocal {
            detect::vocal_regions_cancellable(samples, rate, cancelled)
        } else {
            Some(Vec::new())
        }
    };
    let waveform_grid = || {
        detect::waveform_and_grid_cancellable(
            samples,
            rate,
            o.parts.grid.then_some(&o.grid),
            cancelled,
        )
    };
    let (three_band_detail, three_band_preview, beats, key, vocal) =
        if execution == Execution::Interactive {
            std::thread::scope(|s| {
                fn join<T>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
                    h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
                }
                let key = s.spawn(key);
                let vocal = s.spawn(vocal);
                let waveform = waveform_grid();
                let key = join(key);
                let vocal = join(vocal);
                let (detail, preview, beats) = waveform?;
                Some((detail, preview, beats, key?, vocal?))
            })
        } else {
            let (detail, preview, beats) = waveform_grid()?;
            if cancelled() {
                return None;
            }
            let key = key()?;
            if cancelled() {
                return None;
            }
            let vocal = vocal()?;
            Some((detail, preview, beats, key, vocal))
        }?;
    if cancelled() {
        return None;
    }
    let phrases = phrases_for(o, &beats, &three_band_detail);
    if cancelled() {
        return None;
    }
    Some(Analysis {
        beats,
        three_band_detail,
        three_band_preview,
        key,
        vocal,
        phrases,
        ..Default::default()
    })
}

/// Our phrases for `beats` and the 3-band detail; none while the Phrase part of `o` is off.
fn phrases_for(o: &options::Options, beats: &[Beat], detail: &[[u8; 3]]) -> Option<Phrases> {
    if o.parts.phrase {
        auto::phrases(beats, detail)
    } else {
        None
    }
}

/// 1-based grid numbers (the phrases' unit) of the bar lines: the beats with `bar_pos == 1`; a grid without any counts
/// every 4th beat from its first.
fn bars(beats: &[Beat]) -> Vec<u32> {
    let n = beats.len() as u32;
    let v: Vec<u32> = (1..=n)
        .filter(|&i| beats[i as usize - 1].bar_pos == 1)
        .collect();
    if v.is_empty() {
        (1..=n).step_by(auto::BAR).collect()
    } else {
        v
    }
}

/// Start time (ms) of each bar line ([`bars`]).
fn bar_times(beats: &[Beat]) -> Vec<u32> {
    bars(beats)
        .into_iter()
        .map(|n| beats[n as usize - 1].time_ms)
        .collect()
}

/// Complex in-place radix-2 FFT (`re.len()` a power of two, `tw` its `n / 2` forward twiddles).
fn fft(re: &mut [f32], im: &mut [f32], tw: &[(f32, f32)]) {
    let n = re.len();
    for i in 1..n {
        let j = i.reverse_bits() >> (usize::BITS - n.trailing_zeros());
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let step = n / len;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (c, sn) = tw[k * step];
                let (a, b) = (s + k, s + k + len / 2);
                let (xr, xi) = (re[b] * c - im[b] * sn, re[b] * sn + im[b] * c);
                (re[b], im[b]) = (re[a] - xr, im[a] - xi);
                (re[a], im[a]) = (re[a] + xr, im[a] + xi);
            }
        }
        len *= 2;
    }
}

/// Hann-windowed magnitude spectra of `n`-sample frames; the tables and buffers are built once per analysis.
pub(crate) struct Stft {
    tw: Vec<(f32, f32)>,
    win: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    mag: Vec<f32>,
}

impl Stft {
    /// `n` must be a power of two.
    pub fn new(n: usize) -> Self {
        use std::f32::consts::TAU;
        Self {
            tw: (0..n / 2)
                .map(|k| {
                    let a = -TAU * k as f32 / n as f32;
                    (a.cos(), a.sin())
                })
                .collect(),
            win: (0..n)
                .map(|i| 0.5 - 0.5 * (TAU * i as f32 / n as f32).cos())
                .collect(),
            re: vec![0.0; n],
            im: vec![0.0; n],
            mag: vec![0.0; n / 2],
        }
    }

    /// Magnitudes of bins `0..n/2` of `frame` (exactly `n` samples), valid until the next call.
    pub fn magnitudes(&mut self, frame: &[f32]) -> &[f32] {
        for (((re, im), x), w) in self
            .re
            .iter_mut()
            .zip(&mut self.im)
            .zip(frame)
            .zip(&self.win)
        {
            (*re, *im) = (x * w, 0.0);
        }
        fft(&mut self.re, &mut self.im, &self.tw);
        for ((m, re), im) in self.mag.iter_mut().zip(&self.re).zip(&self.im) {
            *m = (re * re + im * im).sqrt();
        }
        &self.mag
    }
}

/// Mono, anti-aliased (triangular window of 2d samples) decimation by `d` of interleaved stereo. The mono signal is read on
/// the fly: no full-length copy (key and vocal detection run this at once, on tracks of up to an hour).
fn mono_decimate_cancellable(
    samples: &[f32],
    d: usize,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<f32>> {
    if cancelled() {
        return None;
    }
    let n = samples.len() / 2;
    let mono = |i: usize| (samples[2 * i] + samples[2 * i + 1]) * 0.5;
    let norm = 1.0 / (d * d) as f32;
    let mut out = Vec::with_capacity(n / d);
    for k in 0..n / d {
        if k % 4096 == 0 && cancelled() {
            return None;
        }
        if d == 1 {
            out.push(mono(k));
        } else {
            let lo = (k * d).saturating_sub(d);
            let hi = (k * d + d).min(n);
            out.push(
                (lo..hi)
                    .map(|i| mono(i) * (d - (i as isize - (k * d) as isize).unsigned_abs()) as f32)
                    .sum::<f32>()
                    * norm,
            );
        }
    }
    (!cancelled()).then_some(out)
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn samples() -> Vec<f32> {
        (0..12 * 8000)
            .flat_map(|i| {
                let t = i as f32 / 8000.0;
                let pulse = if i % 4000 < 160 { 0.4 } else { 0.0 };
                let x = (t * std::f32::consts::TAU * 220.0).sin() * 0.2
                    + (t * std::f32::consts::TAU * 329.63).sin() * 0.15
                    + pulse;
                [x, x * 0.8]
            })
            .collect()
    }

    #[test]
    fn cancellable_decimation_matches_the_original_tent_filter_bit_for_bit() {
        let input: Vec<f32> = (0..10001).map(|i| (i as f32 * 0.017).sin()).collect();
        for d in [1, 2, 4, 7] {
            let n = input.len() / 2;
            let mono = |i: usize| (input[2 * i] + input[2 * i + 1]) * 0.5;
            let reference: Vec<f32> = if d == 1 {
                (0..n).map(mono).collect()
            } else {
                let norm = 1.0 / (d * d) as f32;
                (0..n / d)
                    .map(|k| {
                        let lo = (k * d).saturating_sub(d);
                        let hi = (k * d + d).min(n);
                        (lo..hi)
                            .map(|i| {
                                mono(i)
                                    * (d - (i as isize - (k * d) as isize).unsigned_abs()) as f32
                            })
                            .sum::<f32>()
                            * norm
                    })
                    .collect()
            };
            let actual = mono_decimate_cancellable(&input, d, &|| false).unwrap();
            assert_eq!(actual.len(), reference.len());
            assert!(
                actual
                    .iter()
                    .zip(reference)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "decimation factor {d}"
            );
        }
        let calls = AtomicUsize::new(0);
        assert!(
            mono_decimate_cancellable(&input, 1, &|| calls.fetch_add(1, Ordering::Relaxed) >= 2)
                .is_none()
        );
    }

    #[test]
    fn bulk_and_interactive_analysis_produce_identical_results() {
        let samples = samples();
        let all = options::Options::default();
        let no_detectors = options::Options {
            parts: options::Parts {
                key: false,
                vocal: false,
                ..all.parts
            },
            ..all
        };
        let no_grid = options::Options {
            parts: options::Parts {
                grid: false,
                phrase: false,
                ..all.parts
            },
            ..all
        };
        for options in [all, no_detectors, no_grid] {
            let interactive =
                analyze_cancellable(&samples, 8000, &options, Execution::Interactive, &|| false)
                    .unwrap();
            let bulk =
                analyze_cancellable(&samples, 8000, &options, Execution::Bulk, &|| false).unwrap();
            assert_eq!(
                serde_json::to_value(interactive).unwrap(),
                serde_json::to_value(bulk).unwrap()
            );
        }
    }

    #[test]
    fn cancellation_at_each_bulk_boundary_discards_the_whole_result() {
        let samples = samples();
        for stop in 0..=6 {
            let calls = AtomicUsize::new(0);
            let cancelled = || calls.fetch_add(1, Ordering::Relaxed) >= stop;
            assert!(
                analyze_cancellable(
                    &samples,
                    8000,
                    &options::Options::default(),
                    Execution::Bulk,
                    &cancelled
                )
                .is_none()
            );
        }
        assert!(
            analyze_cancellable(
                &samples,
                8000,
                &options::Options::default(),
                Execution::Interactive,
                &|| true
            )
            .is_none()
        );
    }

    #[test]
    #[ignore = "prints same-input bulk and interactive wall times"]
    fn bulk_vs_interactive_analysis_timing() {
        let samples = samples();
        for mode in [Execution::Bulk, Execution::Interactive] {
            let start = std::time::Instant::now();
            for _ in 0..5 {
                std::hint::black_box(
                    analyze_cancellable(
                        &samples,
                        8000,
                        &options::Options::default(),
                        mode,
                        &|| false,
                    )
                    .unwrap(),
                );
            }
            eprintln!(
                "{}: {:?}",
                if mode == Execution::Bulk {
                    "bulk"
                } else {
                    "interactive"
                },
                start.elapsed()
            );
        }
    }
}
