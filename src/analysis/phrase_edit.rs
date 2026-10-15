//! Manual phrase editing (EXPORT mode GRID panel): click [CUT] to light up, then click on
//! the phrase to be divided; click the type icon to change the phrase type; [CLEAR] deletes all phrases; click a phrase
//! and drag the divided position to adjust it. Pure functions on [`Phrases`]; the store keeps the
//! result ([`crate::store`], `phrase_edit`) and the overlay lays it over the analysis.
//!
//! The rules below are ours:
//! - a list is contiguous: every phrase ends where the next starts (see [`super::phrase`]); the last one ends at the
//!   tag's `end_beat`;
//! - cut and boundary positions are bar lines of the beat grid (the beats with `bar_pos == 1`, else every 4th beat), as our own
//!   phrase analysis cuts; a boundary keeps at least one beat to each neighbour;
//! - the kind cycles through [`Mood::legend`], the names the mood has (so the result is always a valid phrase list);
//! - CLEAR leaves an empty list with the mood kept, which means "no phrases" (not "no edit"); a CUT on an empty list starts from one
//!   phrase over the whole grid ([`blank`]).

use super::{Beat, Mood, Phrase, PhraseKind, Phrases, auto::BAR, bars};

/// The bar line nearest `ms` as a 1-based beat number; `None` without a grid.
pub fn bar_at(beats: &[Beat], ms: f64) -> Option<u32> {
    bars(beats).into_iter().min_by(|&a, &b| {
        let d = |n: u32| (f64::from(beats[n as usize - 1].time_ms) - ms).abs();
        d(a).total_cmp(&d(b))
    })
}

/// One phrase (the mood's first legend entry) over the whole grid, from the first bar line to the end of the last whole bar: what a
/// CUT starts from after CLEAR; `None` without a whole bar.
pub fn blank(beats: &[Beat], mood: Mood) -> Option<Phrases> {
    let b = bars(beats);
    let (first, last) = (*b.first()?, *b.last()?);
    let end = (last + BAR as u32).min(beats.len() as u32 + 1);
    let (kind, variant) = entry(mood, 0);
    (end > first).then(|| Phrases {
        mood,
        list: vec![Phrase {
            beat: first,
            end_beat: end,
            kind,
            variant,
            fill: None,
        }],
    })
}

/// Index of the phrase containing the 1-based beat `n`.
pub fn at(p: &Phrases, n: u32) -> Option<usize> {
    p.list.iter().position(|ph| ph.beat <= n && n < ph.end_beat)
}

/// Divides the phrase containing `beat` in two, the second starting at `beat` (same kind; the fill-in stays with the half it lies
/// in). `false` when `beat` is not strictly inside a phrase.
pub fn cut(p: &mut Phrases, beat: u32) -> bool {
    let Some(i) = at(p, beat).filter(|&i| p.list[i].beat < beat) else {
        return false;
    };
    let head = &mut p.list[i];
    let mut tail = head.clone();
    (tail.beat, head.end_beat) = (beat, beat);
    match head.fill {
        Some(f) if f >= beat => head.fill = None,
        _ => tail.fill = None,
    }
    p.list.insert(i + 1, tail);
    true
}

/// Moves the boundary between phrase `i` and `i + 1` to the bar line of `beats` nearest `beat`, kept strictly between the
/// neighbours' own boundaries (a fill-in cut off by the move is dropped). `false` when there is no such boundary or no room.
pub fn move_boundary(p: &mut Phrases, i: usize, beat: u32, beats: &[Beat]) -> bool {
    let (Some(a), Some(b)) = (p.list.get(i), p.list.get(i + 1)) else {
        return false;
    };
    let (lo, hi) = (a.beat + 1, b.end_beat - 1);
    if lo > hi {
        return false;
    }
    let n = bars(beats)
        .into_iter()
        .filter(|n| (lo..=hi).contains(n))
        .min_by_key(|n| n.abs_diff(beat))
        .unwrap_or_else(|| beat.clamp(lo, hi));
    let [a, b] = &mut p.list[i..=i + 1] else {
        return false;
    };
    (a.end_beat, b.beat) = (n, n);
    a.fill = a.fill.filter(|&f| f < n);
    b.fill = b.fill.filter(|&f| f >= n);
    true
}

/// Phrase kind and style number of entry `slot` of the mood's legend.
fn entry(mood: Mood, slot: usize) -> (PhraseKind, u8) {
    let name = mood.legend()[slot % mood.legend().len()];
    let (kind, num) = name.split_once(' ').unwrap_or((name, ""));
    let kind = match kind {
        "Intro" => PhraseKind::Intro,
        "Verse" => PhraseKind::Verse,
        "Chorus" => PhraseKind::Chorus,
        "Bridge" => PhraseKind::Bridge,
        "Outro" => PhraseKind::Outro,
        "Up" => PhraseKind::Up,
        _ => PhraseKind::Down,
    };
    (kind, num.parse().unwrap_or(0))
}

/// Phrase `i` takes the mood's legend entry `slot` (wraps). `false` when there is no phrase `i`.
pub fn set_kind(p: &mut Phrases, i: usize, slot: usize) -> bool {
    let mood = p.mood;
    let Some(ph) = p.list.get_mut(i) else {
        return false;
    };
    (ph.kind, ph.variant) = entry(mood, slot);
    true
}

/// The type icon: phrase `i` takes the next entry of the mood's legend (after the last, the first; an entry the mood does not have
/// counts as before the first).
pub fn next_kind(p: &mut Phrases, i: usize) -> bool {
    let Some(ph) = p.list.get(i) else {
        return false;
    };
    let slot = p.mood.slot(ph.kind, ph.variant).map_or(0, |s| s + 1);
    set_kind(p, i, slot)
}

/// CLEAR: no phrases left (the mood stays for a later CUT).
pub fn clear(p: &mut Phrases) {
    p.list.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` beats every 500 ms in 4/4.
    fn grid(n: u32) -> Vec<Beat> {
        (0..n)
            .map(|i| Beat {
                bar_pos: (i % 4) as u16 + 1,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect()
    }

    fn song() -> Phrases {
        let ph = |beat, end_beat, kind, variant, fill| Phrase {
            beat,
            end_beat,
            kind,
            variant,
            fill,
        };
        Phrases {
            mood: Mood::High,
            list: vec![
                ph(1, 17, PhraseKind::Intro, 1, None),
                ph(17, 49, PhraseKind::Up, 1, Some(45)),
                ph(49, 65, PhraseKind::Outro, 1, None),
            ],
        }
    }

    fn contiguous(p: &Phrases) -> bool {
        p.list.windows(2).all(|w| w[0].end_beat == w[1].beat)
            && p.list.iter().all(|ph| ph.beat < ph.end_beat)
    }

    #[test]
    fn bar_lines_are_the_downbeats_and_snap_to_the_nearest() {
        let g = grid(16);
        assert_eq!(bars(&g), [1, 5, 9, 13]);
        assert_eq!(bar_at(&g, 2100.0), Some(5), "beat 5 = 2000 ms");
        assert_eq!(bar_at(&g, 3900.0), Some(9), "beat 9 = 4000 ms");
        assert_eq!(bar_at(&[], 0.0), None);
        let no_pos: Vec<Beat> = grid(10)
            .into_iter()
            .map(|b| Beat { bar_pos: 0, ..b })
            .collect();
        assert_eq!(
            bars(&no_pos),
            [1, 5, 9],
            "a grid without bar positions counts every 4th beat"
        );
    }

    #[test]
    fn cut_splits_the_phrase_and_keeps_the_list_contiguous() {
        let mut p = song();
        assert!(cut(&mut p, 33));
        assert_eq!(
            p.list
                .iter()
                .map(|ph| (ph.beat, ph.end_beat))
                .collect::<Vec<_>>(),
            [(1, 17), (17, 33), (33, 49), (49, 65)]
        );
        assert_eq!(
            (p.list[1].kind, p.list[2].kind),
            (PhraseKind::Up, PhraseKind::Up)
        );
        assert_eq!(
            (p.list[1].fill, p.list[2].fill),
            (None, Some(45)),
            "the fill-in stays with the half it lies in"
        );
        assert!(contiguous(&p));
        // Not inside a phrase / on a start: refused, nothing changes.
        let before = p.clone();
        assert!(!cut(&mut p, 17) && !cut(&mut p, 1) && !cut(&mut p, 65) && !cut(&mut p, 200));
        assert_eq!(p, before);
    }

    #[test]
    fn a_boundary_moves_to_a_bar_between_its_neighbours() {
        let g = grid(64);
        let mut p = song();
        assert!(move_boundary(&mut p, 0, 22, &g), "beat 22 snaps to bar 21");
        assert_eq!((p.list[0].end_beat, p.list[1].beat), (21, 21));
        // Far past the next boundary: the last bar line before the next phrase's end (49); boundaries stay sorted.
        assert!(move_boundary(&mut p, 0, 60, &g));
        assert_eq!((p.list[0].end_beat, p.list[1].beat), (45, 45));
        assert_eq!(
            p.list[1].fill,
            Some(45),
            "the fill-in lies on the new start: kept"
        );
        assert!(move_boundary(&mut p, 1, 62, &g));
        assert_eq!((p.list[1].end_beat, p.list[2].beat), (61, 61));
        // Moving a start past a fill-in drops it.
        assert!(move_boundary(&mut p, 0, 50, &g));
        assert_eq!((p.list[0].end_beat, p.list[1].fill), (49, None));
        assert!(contiguous(&p));
        assert!(
            !move_boundary(&mut p, 2, 5, &g),
            "the last phrase has no boundary after it"
        );
        // Without a bar line in reach the beat itself is used, still between the neighbours.
        let mut tiny = Phrases {
            mood: Mood::Mid,
            list: song().list[..2].to_vec(),
        };
        (
            tiny.list[0].end_beat,
            tiny.list[1].beat,
            tiny.list[1].end_beat,
        ) = (3, 3, 5);
        assert!(move_boundary(&mut tiny, 0, 100, &g));
        assert_eq!(tiny.list[0].end_beat, 4);
    }

    #[test]
    fn the_kind_cycles_through_the_moods_legend_only() {
        for mood in [Mood::High, Mood::Mid, Mood::Low] {
            let mut p = Phrases {
                mood,
                list: song().list[..1].to_vec(),
            };
            set_kind(&mut p, 0, 0);
            let first = (p.list[0].kind, p.list[0].variant);
            for step in 1..=mood.legend().len() {
                assert!(next_kind(&mut p, 0));
                let ph = &p.list[0];
                assert!(
                    mood.slot(ph.kind, ph.variant).is_some(),
                    "{mood:?} step {step}: {}",
                    ph.label()
                );
            }
            assert_eq!(
                (p.list[0].kind, p.list[0].variant),
                first,
                "{mood:?}: a full turn is back at the start"
            );
        }
        let mut p = song();
        assert!(next_kind(&mut p, 0));
        assert_eq!(p.list[0].label(), "Intro 2");
        assert!(set_kind(&mut p, 0, 7) && p.list[0].label() == "Down");
        assert!(!next_kind(&mut p, 9) && !set_kind(&mut p, 9, 0));
    }

    #[test]
    fn clear_leaves_no_phrases_and_a_cut_restarts_from_the_grid() {
        let mut p = song();
        clear(&mut p);
        assert!(p.list.is_empty() && p.mood == Mood::High);
        assert!(!cut(&mut p, 9));
        let mut b = blank(&grid(16), Mood::High).unwrap();
        assert_eq!(
            b.list[0].label(),
            "Intro 1",
            "the first legend entry: legal in the mood"
        );
        assert_eq!((b.list[0].beat, b.list[0].end_beat), (1, 17));
        assert!(cut(&mut b, 9) && contiguous(&b));
        assert!(blank(&[], Mood::Mid).is_none());
    }
}
