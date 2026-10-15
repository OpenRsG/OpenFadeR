//! Our own phrase analysis and cue suggestions for tracks without stored phrases (folder and XML
//! libraries, or a device track that was never analysed). The design is ours.
//!
//! Input is what [`super::analyze`] already produces: the beat grid and the 3-band detail waveform
//! (`[low, mid, high]`, 150 columns/s), so a track whose library has both gets phrases without decoding.
//!
//! This base owns [`Bars`] (per-bar band levels, shared by the children) and [`phrases`]. Children, one per stage:
//! - [`segment`]: bar boundaries from band-level changes (least-squares segmentation with a bar-length prior)
//! - [`kind`]: mood and phrase kinds from each segment's band levels
//! - [`cues`]: memory and hot cues from the grid and phrases under the CUE Analysis options
//! - [`tendency`]: the user's own cue habits (for CUE Analysis Auto)
//!
//! Evidence: the length prior and the kind rules were measured on 24 phrase-analysed tracks
//! (`tests/phrase_auto_usb.rs` reports the agreement). That set is all High mood and electronic, so the
//! Mid/Low-mood labelling (Verse/Bridge) is untested.

mod cues;
mod kind;
mod segment;
mod tendency;

pub use cues::{CueMode, CueOptions, MAX_HOT, MAX_MEMORY, for_analysis, generate, suggest as cues};
pub use tendency::{MIN_TRACKS, Tendency, derive as derive_tendency};

use super::{Analysis, Beat, DETAIL_ENTRIES_PER_SEC, Phrase, Phrases, bar_times, options};

/// Beats per bar (grids are 4/4: `bar_pos` 1..=4).
pub const BAR: usize = 4;

/// The track cut into whole bars, each bar's mean `[low, mid, high]` level scaled so the track's 90th percentile is 1.
pub struct Bars {
    /// 0-based grid index of each bar's first beat (`bar_pos == 1`).
    pub start: Vec<usize>,
    pub level: Vec<[f32; 3]>,
    /// Mean `[low, mid, high]` height of the whole waveform, unscaled (0..=127): how much of each band the track has.
    pub absolute: [f32; 3],
}

impl Bars {
    /// `None` when the grid has no whole bar or the waveform is empty.
    pub fn new(beats: &[Beat], detail: &[[u8; 3]]) -> Option<Bars> {
        let col = |i: usize| {
            (beats[i].time_ms as usize * DETAIL_ENTRIES_PER_SEC as usize / 1000).min(detail.len())
        };
        let (mut start, mut level) = (Vec::new(), Vec::new());
        for i in (0..(beats.len() + 1).saturating_sub(BAR)).filter(|&i| beats[i].bar_pos == 1) {
            let (a, b) = (
                col(i),
                if i + BAR < beats.len() {
                    col(i + BAR)
                } else {
                    detail.len()
                },
            );
            if a >= b {
                continue;
            }
            let mut m = [0f32; 3];
            for d in &detail[a..b] {
                (0..3).for_each(|k| m[k] += d[k] as f32);
            }
            start.push(i);
            level.push(m.map(|v| v / (b - a) as f32));
        }
        if level.is_empty() {
            return None;
        }
        let mut absolute = [0f32; 3];
        for d in detail {
            (0..3).for_each(|k| absolute[k] += d[k] as f32 / detail.len() as f32);
        }
        for k in 0..3 {
            let mut v: Vec<f32> = level.iter().map(|l| l[k]).collect();
            v.sort_by(f32::total_cmp);
            let top = v[(v.len() - 1) * 9 / 10].max(1e-3);
            level.iter_mut().for_each(|l| l[k] /= top);
        }
        Some(Bars {
            start,
            level,
            absolute,
        })
    }

    /// Mean level of bars `a..b`.
    pub fn mean(&self, a: usize, b: usize) -> [f32; 3] {
        let mut m = [0f32; 3];
        for l in &self.level[a..b] {
            (0..3).for_each(|k| m[k] += l[k]);
        }
        m.map(|v| v / (b - a) as f32)
    }
}

/// Phrases of a track from its grid and 3-band detail waveform: segments of whole bars (`variant`s and `fill`s are
/// ours, see [`kind`]); `None` when the track is too short for two phrases or lacks a grid or waveform.
pub fn phrases(beats: &[Beat], detail: &[[u8; 3]]) -> Option<Phrases> {
    let bars = Bars::new(beats, detail)?;
    let cuts = segment::cut(&bars.level);
    let (mood, kinds) = kind::classify(&bars, &cuts);
    let end = bars.start[bars.start.len() - 1] + BAR + 1; // 1-based beat after the last whole bar
    let list = (0..cuts.len())
        .map(|i| Phrase {
            beat: if i == 0 {
                1
            } else {
                bars.start[cuts[i]] as u32 + 1
            },
            end_beat: cuts
                .get(i + 1)
                .map_or(end as u32, |&c| bars.start[c] as u32 + 1),
            kind: kinds[i].0,
            variant: kinds[i].1,
            fill: None,
        })
        .collect::<Vec<_>>();
    (list.len() > 1).then_some(Phrases { mood, list })
}

/// Gives `a` our phrases when it has none, from its grid and 3-band waveform, unless the Phrase part of
/// Track Analysis Setting is off ([`options`]).
pub fn fill(a: &mut Analysis) {
    if a.phrases.is_none() {
        a.phrases = super::phrases_for(&options::get(), &a.beats, &a.three_band_detail);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Mood, PhraseKind::*};
    use super::*;

    /// 120 BPM grid (500 ms per beat, 300 detail columns per bar) and a waveform that is, in bars:
    /// 0-8 quiet intro, 8-16 build, 16-32 full, 32-40 breakdown without bass, 40-56 full, 56-64 outro.
    fn track() -> Analysis {
        let sections: [(usize, [u8; 3]); 6] = [
            (8, [20, 30, 20]),
            (8, [20, 70, 50]),
            (16, [110, 100, 80]),
            (8, [10, 60, 50]),
            (16, [105, 100, 80]),
            (8, [70, 60, 50]),
        ];
        let detail = sections
            .iter()
            .flat_map(|&(bars, h)| vec![h; bars * 300])
            .collect();
        let beats = (0..256)
            .map(|i| Beat {
                bar_pos: i % 4 + 1,
                bpm_x100: 12000,
                time_ms: i as u32 * 500,
            })
            .collect();
        Analysis {
            beats,
            three_band_detail: detail,
            ..Default::default()
        }
    }

    #[test]
    fn phrases_follow_the_sections() {
        let mut a = track();
        fill(&mut a);
        let p = a.phrases.as_ref().unwrap();
        assert_eq!(p.mood, Mood::High);
        let got: Vec<_> = p.list.iter().map(|ph| (ph.beat, ph.kind)).collect();
        assert_eq!(
            got,
            [
                (1, Intro),
                (33, Up),
                (65, Chorus),
                (129, Down),
                (161, Chorus),
                (225, Outro)
            ]
        );
        assert_eq!(p.list.last().unwrap().end_beat, 257);
        assert!(p.list.windows(2).all(|w| w[0].end_beat == w[1].beat));
    }

    #[test]
    fn real_phrases_are_kept_and_unusable_input_gives_none() {
        let mut a = track();
        fill(&mut a);
        let before = a.phrases.clone();
        a.three_band_detail.clear();
        fill(&mut a);
        assert_eq!(a.phrases, before);
        assert!(phrases(&[], &[[1, 1, 1]; 10]).is_none());
        assert!(phrases(&track().beats, &[]).is_none());
        assert!(
            phrases(&track().beats[..4], &track().three_band_detail).is_none(),
            "one bar is one phrase"
        );
    }

    #[test]
    fn cues_sit_on_phrase_changes() {
        let mut a = track();
        assert_eq!(cues(&a).len(), 1, "no phrases yet: only the memory cue");
        fill(&mut a);
        let c = cues(&a);
        assert_eq!((c[0].hot_cue, c[0].time_ms), (0, 0));
        let hot: Vec<_> = c[1..].iter().map(|c| (c.hot_cue, c.time_ms)).collect();
        assert_eq!(
            hot,
            [
                (1, 0),
                (2, 16_000),
                (3, 32_000),
                (4, 64_000),
                (5, 80_000),
                (6, 112_000)
            ]
        );
        assert!(c[1..].iter().all(|c| c.color.is_some()));
        a.cues = c;
        assert!(cues(&a).is_empty(), "a track with cues is left alone");
    }

    #[test]
    fn a_track_without_bass_gets_song_labels() {
        let mut a = track();
        a.three_band_detail.iter_mut().for_each(|d| d[0] /= 20);
        fill(&mut a);
        let p = a.phrases.unwrap();
        assert_eq!(p.mood, Mood::Low);
        let kinds: Vec<_> = p.list.iter().map(|ph| (ph.kind, ph.variant)).collect();
        assert_eq!((kinds[0], *kinds.last().unwrap()), ((Intro, 0), (Outro, 0)));
        assert!(
            kinds[1..kinds.len() - 1]
                .iter()
                .all(|k| matches!(k.0, Verse | Bridge | Chorus)),
            "{kinds:?}"
        );
        assert!(
            kinds.iter().any(|k| k.0 == Chorus) && kinds.iter().any(|k| k.0 == Bridge),
            "{kinds:?}"
        );
    }

    #[test]
    fn more_than_eight_changes_keep_intro_outro_and_first_of_each_kind() {
        let kinds = [
            Intro, Up, Chorus, Down, Chorus, Down, Chorus, Down, Chorus, Up, Chorus, Down, Outro,
        ];
        let list = kinds
            .iter()
            .enumerate()
            .map(|(i, &kind)| Phrase {
                beat: 1 + 32 * i as u32,
                end_beat: 33 + 32 * i as u32,
                kind,
                variant: 0,
                fill: None,
            })
            .collect();
        let beats = (0..13 * 32)
            .map(|i| Beat {
                bar_pos: i % 4 + 1,
                bpm_x100: 12000,
                time_ms: i as u32 * 500,
            })
            .collect();
        let a = Analysis {
            beats,
            phrases: Some(Phrases {
                mood: Mood::High,
                list,
            }),
            ..Default::default()
        };
        // Phrase i starts at 16 s * i. Intro (0) and Outro (12) first, then the first Up, Chorus, Down (1, 2, 3), then the earliest of the rest (4, 5, 6).
        let hot: Vec<_> = cues(&a)[1..].iter().map(|c| c.time_ms / 16_000).collect();
        assert_eq!(hot, [0, 1, 2, 3, 4, 5, 6, 12]);
    }
}
