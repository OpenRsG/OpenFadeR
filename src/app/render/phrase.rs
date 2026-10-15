//! The phrase band of the overview and the detail waveform and its colours.
//!
//! Colours come from one table per mood,
//! one entry per phrase name in [`Mood::legend`] order. That entry order follows the Lighting phrase strings, as does the phrase kind → entry mapping ([`Mood::slot`], backed by `analysis::phrase`'s own id tables). An entry
//! with no table value gets `0x72f0fe` (High, Mid) or `0xff99ff` (Low), used for
//! a phrase the mood does not have (stored phrase analysis cannot produce one; our own analysis can only through a bug).

use super::{Axis, Canvas, Rgb, font};
use openfader::analysis::{Analysis, Mood, PhraseKind};
use std::ops::Range;

const HIGH: [u32; 10] = [
    0xc80000, 0xc83200, 0x8c32ff, 0x6932ff, 0x5a32ff, 0x0faa00, 0x00aa7d, 0x9b732d, 0x5087c3,
    0x5f87af,
];
const MID: [u32; 10] = [
    0xe14646, 0x506eff, 0x5055ff, 0x6450ff, 0x7850ff, 0x8c50ff, 0xa050ff, 0xe1d741, 0x78c37d,
    0x738296,
];
const LOW: [u32; 6] = [0xffaab4, 0xa5a0ff, 0xbea0ff, 0xb9e1b9, 0xfffaa5, 0x91a0b4];

fn table(mood: Mood) -> &'static [u32] {
    match mood {
        Mood::High => &HIGH,
        Mood::Mid => &MID,
        Mood::Low => &LOW,
    }
}

fn rgb(c: u32) -> Rgb {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// Band colour of a phrase of `kind` and style number `variant` in a track of `mood`.
pub fn phrase_color(mood: Mood, kind: PhraseKind, variant: u8) -> Rgb {
    rgb(mood.slot(kind, variant).map_or(
        if mood == Mood::Low {
            0xff99ff
        } else {
            0x72f0fe
        },
        |s| table(mood)[s],
    ))
}

/// Opaque phrase band over pixel `rows` of `c`: one colour per phrase, a black separator at each start,
/// the fill-in tail lightened. `label_scale`: also write the phrase name (white, glyph scale) after
/// each start, pinned to the left edge while the start is scrolled out and clipped to the phrase.
pub fn phrase_band(
    c: &mut Canvas,
    axis: &Axis,
    a: &Analysis,
    rows: Range<usize>,
    label_scale: Option<usize>,
) {
    let mood = a.phrases.as_ref().map_or(Mood::Mid, |p| p.mood);
    for ph in a.phrases.iter().flat_map(|p| &p.list) {
        let (s, e) = ph.span_ms(&a.beats);
        let (x0, x1) = (axis.col(s), axis.col(e));
        if x1 <= 0 || x0 >= c.w as i64 {
            continue;
        }
        c.shade(
            x0,
            x1,
            rows.clone(),
            phrase_color(mood, ph.kind, ph.variant),
            1.0,
        );
        if let Some(f) = ph.fill_ms(&a.beats) {
            c.shade(axis.col(f), x1, rows.clone(), [255; 3], 0.35);
        }
        c.vline(x0, rows.clone(), [0; 3]);
        if let Some(scale) = label_scale {
            let y = rows.start + rows.len().saturating_sub(font::H * scale) / 2;
            c.text(x0.max(0) + 2, y, &ph.label(), scale, [255; 3], x1 - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::{Beat, Phrase, Phrases};

    #[test]
    fn every_legend_entry_has_its_own_colour_and_none_is_the_default() {
        for mood in [Mood::High, Mood::Mid, Mood::Low] {
            let (names, colours) = (mood.legend(), table(mood));
            assert_eq!(names.len(), colours.len(), "{mood:?}");
            let mut seen = std::collections::HashSet::new();
            assert!(
                colours.iter().all(|&c| seen.insert(c)),
                "{mood:?} repeats a colour"
            );
            assert!(![0x72f0fe, 0xff99ff].iter().any(|d| colours.contains(d)));
        }
        // the first and last phrase of each mood, through the phrase kind names
        assert_eq!(
            phrase_color(Mood::High, PhraseKind::Intro, 1),
            rgb(0xc80000)
        );
        assert_eq!(phrase_color(Mood::High, PhraseKind::Up, 3), rgb(0x5a32ff));
        assert_eq!(phrase_color(Mood::High, PhraseKind::Down, 0), rgb(0x9b732d));
        assert_eq!(
            phrase_color(Mood::High, PhraseKind::Outro, 2),
            rgb(0x5f87af)
        );
        assert_eq!(phrase_color(Mood::Mid, PhraseKind::Verse, 6), rgb(0xa050ff));
        assert_eq!(
            phrase_color(Mood::Mid, PhraseKind::Bridge, 0),
            rgb(0x78c37d)
        );
        assert_eq!(phrase_color(Mood::Low, PhraseKind::Verse, 2), rgb(0xbea0ff));
        assert_eq!(phrase_color(Mood::Low, PhraseKind::Outro, 0), rgb(0x91a0b4));
        // a phrase the mood does not have takes the out-of-range colour
        assert_eq!(
            phrase_color(Mood::High, PhraseKind::Verse, 1),
            rgb(0x72f0fe)
        );
        assert_eq!(phrase_color(Mood::Mid, PhraseKind::Up, 1), rgb(0x72f0fe));
        assert_eq!(phrase_color(Mood::Low, PhraseKind::Down, 0), rgb(0xff99ff));
    }

    #[test]
    fn phrase_band_colours_spans_and_clips_the_label() {
        // 1 px per 100 ms; beats every 500 ms from 0; two phrases: beats 1..3 (0..1000 ms) and 3..5 (1000..2000 ms)
        let beats: Vec<_> = (0..8)
            .map(|i| Beat {
                bar_pos: 1,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect();
        let ph = |beat, end_beat, kind, fill| Phrase {
            beat,
            end_beat,
            kind,
            variant: 0,
            fill,
        };
        let a = Analysis {
            beats,
            phrases: Some(Phrases {
                mood: Mood::Mid,
                list: vec![
                    ph(1, 3, PhraseKind::Intro, None),
                    ph(3, 5, PhraseKind::Chorus, Some(4)),
                ],
            }),
            ..Default::default()
        };
        let mut c = crate::app::render::black_canvas(40, 12);
        phrase_band(
            &mut c,
            &Axis {
                start_ms: 0.0,
                px_per_ms: 0.01,
            },
            &a,
            0..9,
            Some(1),
        );
        let px = |x: usize, y: usize| c.buf.as_slice()[y * 40 + x];
        let rgb = |x, y| [px(x, y).r, px(x, y).g, px(x, y).b];
        // rows below the band stay black; the band rows (below the 1..8 glyph rows) carry the phrase colour
        assert_eq!(rgb(5, 11), [0; 3]);
        assert_eq!(rgb(5, 8), phrase_color(Mood::Mid, PhraseKind::Intro, 0));
        // the second phrase starts on a black separator column, then its own colour; its fill (from beat 4 = 1500 ms) is lighter
        assert_eq!(rgb(10, 8), [0; 3]);
        assert_eq!(rgb(12, 8), phrase_color(Mood::Mid, PhraseKind::Chorus, 0));
        assert!(px(17, 8).g > px(12, 8).g);
        // the label is white text inside the phrase and is cut before the phrase's last column
        assert!((0..9).any(|y| (11..19).any(|x| rgb(x, y) == [255; 3])));
        assert!((0..9).all(|y| rgb(19, y) != [255; 3] && rgb(20, y) != [255; 3]));
    }
}
