//! Mood and phrase kinds from each segment's band levels.
//!
//! Measured on a 24-track test set of phrase-analysed tracks (all High mood), by mean band level relative to the track (`level`, 90th percentile = 1):
//! Chorus: low 0.92, total 1.16 of the track mean; Up and Down: low 0.35, total 0.73; Intro 0.55, Outro 0.85. What
//! tells Up from Down is the neighbours: an Up is followed by louder music than preceded it (the 4 bars after vs the
//! 4 before: 1.02 vs 0.81), a Down by quieter (0.78 vs 1.08). The rule below applies this to the neighbouring phrases'
//! means, which was not measured as such; its end-to-end effect is the kind score of `tests/phrase_auto_usb.rs`.
//! So: first phrase Intro, last Outro, loud with bass Chorus, else Up if the next
//! phrase is louder than the previous one, else Down. Variants ("Intro 2", "Outro 1", "Up 1") are the commonest
//! ones in that set.
//!
//! Mid/Low mood (Verse/Bridge) has no real data here: the same loudness split gives Chorus (loud), Bridge (quiet),
//! Verse (between), and a track's mood is High when the low band holds at least a fifth of the waveform's height, Mid
//! from a tenth, else Low. Based on the phrase names only.

use super::{
    super::{Mood, PhraseKind},
    Bars,
};

/// Chorus: mean low level and total level (relative to the track) at least these.
const CHORUS_LOW: f32 = 0.6;
const CHORUS_TOTAL: f32 = 0.85;
/// Mid/Low mood: below this total level (relative to the track) a phrase is a Bridge.
const BRIDGE_TOTAL: f32 = 0.7;
/// Share of the waveform's height the low band must hold for High / Mid mood.
const HIGH_LOW_SHARE: f32 = 0.2;
const MID_LOW_SHARE: f32 = 0.1;

fn mood(bars: &Bars) -> Mood {
    let low = bars.absolute[0] / bars.absolute.iter().sum::<f32>().max(1e-3);
    if low >= HIGH_LOW_SHARE {
        Mood::High
    } else if low >= MID_LOW_SHARE {
        Mood::Mid
    } else {
        Mood::Low
    }
}

/// `(kind, variant)` of each segment; `cuts` are the segments' start bars.
pub(super) fn classify(bars: &Bars, cuts: &[usize]) -> (Mood, Vec<(PhraseKind, u8)>) {
    let mood = mood(bars);
    let n = bars.level.len();
    let mean: Vec<[f32; 3]> = (0..cuts.len())
        .map(|i| bars.mean(cuts[i], cuts.get(i + 1).copied().unwrap_or(n)))
        .collect();
    let total = |m: &[f32; 3]| m.iter().sum::<f32>();
    let track = total(&bars.mean(0, n)).max(1e-3);
    let last = cuts.len() - 1;
    let high = mood == Mood::High;
    let kinds = (0..cuts.len())
        .map(|i| {
            let rel = total(&mean[i]) / track;
            match i {
                0 => (PhraseKind::Intro, if high { 2 } else { 0 }),
                _ if i == last => (PhraseKind::Outro, high as u8),
                _ if high && mean[i][0] >= CHORUS_LOW && rel >= CHORUS_TOTAL => {
                    (PhraseKind::Chorus, 1)
                }
                _ if high && total(&mean[i + 1]) > total(&mean[i - 1]) => (PhraseKind::Up, 1),
                _ if high => (PhraseKind::Down, 0),
                _ if rel >= CHORUS_TOTAL => (PhraseKind::Chorus, 0),
                _ if rel < BRIDGE_TOTAL => (PhraseKind::Bridge, 0),
                _ => (PhraseKind::Verse, 1),
            }
        })
        .collect();
    (mood, kinds)
}
