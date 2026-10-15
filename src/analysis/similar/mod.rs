//! Track similarity from our own analysis, for Track Suggestion (Collection Radar, Mood). No model is loaded: every number here is a
//! measurement of our own 3-band waveform ([`super::detect`]), grid, vocal regions and phrases, and the weights below are
//! **ours (inferred)**.
//!
//! Base: [`Features`] (what one track sounds like, from an [`Analysis`]) and [`Features::similarity`]. Children: [`meta`] (year
//! and composer from the audio file's tags), [`index`] (a background cache of [`Profile`]s, one per audio file).

mod index;
mod meta;

pub use index::{Index, index};
pub use meta::{Tags, read_tags, year_of};

use super::{Analysis, DETAIL_ENTRIES_PER_SEC, Mood, auto, options};
use std::path::Path;

/// Slices of the waveform the dynamics are measured over (length independent).
const SLICES: usize = 64;
/// Height of the loudest waveform column ([`super::detect`], `0..=127`).
const TOP: f32 = 127.0;
/// Mean level from / below which a track without phrase analysis counts as High / Low mood (**inferred**). Measured: the
/// stored 3-band waveform of a 24-track test set (all High mood phrases, loud electronic) measures 0.21..=0.32, our own
/// waveform of four of them 0.24..=0.29; the thresholds sit under both.
const HIGH: f32 = 0.20;
const LOW: f32 = 0.12;
/// Share of a track with a voice from which it counts as having vocals (**inferred**).
pub const VOCAL_MIN: f32 = 0.15;

/// What a track sounds like, from the 3-band waveform (detail, else the 1200-column preview).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Features {
    /// Share of the waveform's energy in `[low, mid, high]`; sums to 1. The timbre.
    pub bands: [f32; 3],
    /// Mean column height over the loudest one, `0..=1`: how dense and loud the track is.
    pub energy: f32,
    /// Spread of the level over the track (relative standard deviation over [`SLICES`] slices): breakdowns and builds raise it.
    pub dynamics: f32,
    /// Share of the track with a voice; `None` when no vocal analysis exists for it.
    pub vocal: Option<f32>,
    /// The phrase analysis' mood (stored phrases, else [`auto::phrases`]); `None` without a grid.
    pub phrase_mood: Option<Mood>,
}

impl Features {
    /// `None` when `a` has no waveform. `vocal_known`: an empty `a.vocal` means "no voice", not "never analysed".
    pub fn of(a: &Analysis, vocal_known: bool) -> Option<Features> {
        let (wave, detail) = match (&a.three_band_detail, &a.three_band_preview) {
            (d, _) if !d.is_empty() => (d, true),
            (_, p) if !p.is_empty() => (p, false),
            _ => return None,
        };
        let mut sum = [0f64; 3];
        let level = |c: &[u8; 3]| c.iter().map(|&h| f32::from(h)).sum::<f32>() / (3.0 * TOP);
        for c in wave {
            (0..3).for_each(|k| sum[k] += f64::from(c[k]));
        }
        let total = sum.iter().sum::<f64>();
        if total <= 0.0 {
            return None;
        }
        let bands = sum.map(|s| (s / total) as f32);
        let energy = ((total / (3.0 * f64::from(TOP) * wave.len() as f64)) as f32).min(1.0);
        let step = (wave.len() / SLICES).max(1);
        let slices: Vec<f32> = wave
            .chunks(step)
            .map(|s| s.iter().map(level).sum::<f32>() / s.len() as f32)
            .collect();
        let mean = slices.iter().sum::<f32>() / slices.len() as f32;
        let var = slices.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / slices.len() as f32;
        let phrase_mood = a
            .phrases
            .as_ref()
            .map(|p| p.mood)
            .or_else(|| auto::phrases(&a.beats, &a.three_band_detail).map(|p| p.mood));
        let seconds = if detail {
            wave.len() as f32 / DETAIL_ENTRIES_PER_SEC as f32
        } else {
            a.beats.last().map_or(0.0, |b| b.time_ms as f32 / 1000.0)
        };
        let voiced = a
            .vocal
            .iter()
            .map(|r| r.end_ms.saturating_sub(r.start_ms) as f32 / 1000.0)
            .sum::<f32>();
        Some(Features {
            bands,
            energy,
            dynamics: if mean > 0.0 { var.sqrt() / mean } else { 0.0 },
            vocal: (vocal_known && seconds > 0.0).then(|| (voiced / seconds).min(1.0)),
            phrase_mood,
        })
    }

    /// The mood class: the phrase analysis' when there is one, else from the energy.
    pub fn mood(&self) -> Mood {
        self.phrase_mood.unwrap_or(match self.energy {
            e if e >= HIGH => Mood::High,
            e if e < LOW => Mood::Low,
            _ => Mood::Mid,
        })
    }

    /// `0..=1`, 1 for the same sound: closeness of the band shares, the energy and the dynamics.
    pub fn similarity(&self, o: &Features) -> f32 {
        let bands: f32 = (0..3usize)
            .map(|k| (self.bands[k] - o.bands[k]).abs())
            .sum::<f32>()
            / 2.0;
        let dist = 2.0 * bands
            + 2.5 * (self.energy - o.energy).abs()
            + (self.dynamics - o.dynamics).abs().min(1.0);
        (-1.5 * dist).exp()
    }

    /// Whether the track has vocals: `None` when it has no vocal analysis.
    pub fn has_vocal(&self) -> Option<bool> {
        self.vocal.map(|v| v >= VOCAL_MIN)
    }
}

/// Everything Track Suggestion reads off one audio file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub features: Option<Features>,
    pub tags: Tags,
}

/// The library's own analysis of a track, read on the worker ([`Profile`] of one audio file).
pub type Theirs = Box<dyn FnOnce() -> Option<Analysis> + Send>;

/// The [`Profile`] of `audio`: features from the library's own analysis (`theirs`) when it has a waveform, else from our
/// analysis cache; the vocal share always from our cache (a library's analysis may carry thresholds, not regions); tags
/// from the file. Reads files only; CPU light.
///
/// The two waveforms are not the same measurement (measured on four real tracks: ours puts 3..8 points more of the energy
/// in the bass), so a library is ranked on one source wherever it can be: the library's own.
pub fn profile(audio: &Path, theirs: Option<Theirs>) -> Profile {
    let o = options::get();
    let ours = match super::cache::load_for(audio, &o).ok().flatten() {
        Some(a) => Features::of(&a, o.parts.vocal),
        None => super::cache::load(audio)
            .ok()
            .flatten()
            .and_then(|a| Features::of(&a, !a.vocal.is_empty())),
    };
    let theirs = theirs
        .and_then(|read| read())
        .and_then(|a| Features::of(&a, false));
    let features = match (theirs, ours) {
        (Some(t), Some(o)) => Some(Features {
            vocal: o.vocal,
            ..t
        }),
        (t, o) => t.or(o),
    };
    Profile {
        features,
        tags: read_tags(audio),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Phrases, detect::VocalRegion};

    fn analysis(wave: Vec<[u8; 3]>) -> Analysis {
        Analysis {
            three_band_detail: wave,
            ..Default::default()
        }
    }

    #[test]
    fn bands_are_the_share_of_energy_and_sum_to_one() {
        let f = Features::of(&analysis(vec![[100, 50, 50]; 300]), false).unwrap();
        assert!((f.bands.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!((f.bands[0] - 0.5).abs() < 1e-5);
        assert!(Features::of(&analysis(vec![]), false).is_none());
        assert!(Features::of(&analysis(vec![[0; 3]; 10]), false).is_none());
    }

    #[test]
    fn energy_mood_follows_the_level() {
        let mood = |h: u8| {
            Features::of(&analysis(vec![[h; 3]; 600]), false)
                .unwrap()
                .mood()
        };
        assert_eq!(
            (mood(100), mood(22), mood(10)),
            (Mood::High, Mood::Mid, Mood::Low)
        );
    }

    #[test]
    fn a_phrase_analysis_overrules_the_energy() {
        let mut a = analysis(vec![[100; 3]; 600]);
        a.phrases = Some(Phrases {
            mood: Mood::Low,
            list: Vec::new(),
        });
        assert_eq!(Features::of(&a, false).unwrap().mood(), Mood::Low);
    }

    #[test]
    fn similarity_is_one_for_itself_and_falls_with_a_different_sound() {
        let a = Features::of(&analysis(vec![[110, 60, 30]; 600]), false).unwrap();
        let near = Features::of(&analysis(vec![[105, 62, 32]; 600]), false).unwrap();
        let far = Features::of(&analysis(vec![[10, 30, 100]; 600]), false).unwrap();
        assert!((a.similarity(&a) - 1.0).abs() < 1e-6);
        assert!(a.similarity(&near) > a.similarity(&far));
        assert!((a.similarity(&near) - near.similarity(&a)).abs() < 1e-6);
    }

    #[test]
    fn dynamics_see_a_breakdown() {
        let flat = Features::of(&analysis(vec![[80; 3]; 6400]), false).unwrap();
        let mut w = vec![[80; 3]; 6400];
        w[2000..4000].fill([10; 3]);
        let broken = Features::of(&analysis(w), false).unwrap();
        assert!(flat.dynamics < 0.01 && broken.dynamics > 0.3);
    }

    #[test]
    fn vocal_share_needs_a_vocal_analysis() {
        let mut a = analysis(vec![[80; 3]; 150 * 100]); // 100 s
        a.vocal = vec![VocalRegion {
            start_ms: 0,
            end_ms: 30_000,
        }];
        let known = Features::of(&a, true).unwrap();
        assert!((known.vocal.unwrap() - 0.3).abs() < 1e-3);
        assert_eq!(known.has_vocal(), Some(true));
        assert_eq!(Features::of(&a, false).unwrap().vocal, None);
        a.vocal.clear();
        assert_eq!(Features::of(&a, true).unwrap().has_vocal(), Some(false));
    }
}
