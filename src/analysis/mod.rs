//! Track analysis (base): beat grid, cues, waveforms, key, vocal regions and phrases of a track. [`Analysis`] is the SDK's
//! type (one definition for the host and the library plugins, which supply the analysis their libraries hold); this base owns
//! the STFT and decimation that [`key`] and the vocal detector share. Children:
//! - [`colors`]: the neutral colour helpers (cue colour picker, default pad colours, colour-detail entries)
//! - [`detect`]: our own beat grid and 3-band waveforms from decoded PCM ([`analyze`])
//! - [`phrase_edit`]: manual phrase edits (cut, type, boundary, clear) as pure functions on [`Phrases`]
//! - [`key`]: musical key (Camelot) from decoded PCM, also part of [`analyze`]
//! - [`auto`]: our own phrases (from the beat grid + 3-band waveform) and suggested cues
//! - [`cache`]: the on-disk analysis cache
//! - [`options`]: what [`analyze`] does (Preferences > Analysis); [`tag`]: writing the detected key into the audio file's tag
//! - [`similar`]: Track Suggestion's measurements (timbre / energy [`similar::Features`], year and composer tags, a background profile index)

pub mod auto;
pub mod cache;
mod colors;
pub mod detect;
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

/// [`analyze`] under `o`. Key and vocal detection run on their own threads while this one does waveform, grid and phrases.
pub fn analyze_with(samples: &[f32], rate: u32, o: &options::Options) -> Analysis {
    std::thread::scope(|s| {
        fn join<T>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
            h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
        }
        let key = s.spawn(|| {
            if o.parts.key {
                key::detect(samples, rate)
            } else {
                None
            }
        });
        let vocal = s.spawn(|| {
            if o.parts.vocal {
                detect::vocal_regions(samples, rate)
            } else {
                Vec::new()
            }
        });
        let (three_band_detail, three_band_preview) = detect::waveform(samples, rate);
        let beats = if o.parts.grid {
            detect::beat_grid_with(samples, rate, &o.grid)
        } else {
            Vec::new()
        };
        let phrases = phrases_for(o, &beats, &three_band_detail);
        Analysis {
            beats,
            three_band_detail,
            three_band_preview,
            key: join(key),
            vocal: join(vocal),
            phrases,
            ..Default::default()
        }
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
fn mono_decimate(samples: &[f32], d: usize) -> Vec<f32> {
    let n = samples.len() / 2;
    let mono = |i: usize| (samples[2 * i] + samples[2 * i + 1]) * 0.5;
    if d == 1 {
        return (0..n).map(mono).collect();
    }
    let norm = 1.0 / (d * d) as f32;
    (0..n / d)
        .map(|k| {
            let lo = (k * d).saturating_sub(d);
            let hi = (k * d + d).min(n);
            (lo..hi)
                .map(|i| mono(i) * (d - (i as isize - (k * d) as isize).unsigned_abs()) as f32)
                .sum::<f32>()
                * norm
        })
        .collect()
}
