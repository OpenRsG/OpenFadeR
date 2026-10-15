//! Suggested cues: memory cues and hot cues A..H placed from the beat grid and phrases under [`CueOptions`]. The
//! picking is a pure function ([`generate`]) over (grid, phrases, options).
//!
//! Candidate positions ("anchors", most important first):
//! - "Repeat CUE settings per N bars from 1.1 Bars" (Manual only): bar 1.1, then every N bars;
//! - otherwise the start of every phrase whose kind differs from the one before it (a run of Choruses gets one cue),
//!   Intro and Outro first, then the first of each other kind, then the earliest rest;
//! - "Set to 1.1 Bars" puts bar 1.1 in front of either list.
//!
//! Hot cues A.. are the first `hot` anchors in time order. Memory cues are 1.1 plus the anchors, the first `memory` in
//! time order (so one memory cue is the first downbeat, as before the options existed). "Add a comment to CUE" names a
//! cue after its phrase ("Chorus 2") or its bar ("Bar 17").
//!
//! Auto ([`CueMode::Auto`]) follows the user's own cues instead ([`super::Tendency`], learnt from the collection, at
//! least [`super::MIN_TRACKS`] tracks): its first cue's bar, the usual gap between hot cues, and how many hot / memory
//! cues a track gets. Without a tendency, Auto is Manual.

use super::{Tendency, bar_times};
use crate::analysis::{Analysis, Beat, COLORFUL, Cue, PhraseKind, Phrases, options};

/// Most hot cues (pads A-H) and memory cues a track gets.
pub const MAX_HOT: u8 = COLORFUL.len() as u8;
pub const MAX_MEMORY: u8 = 10;

/// "Set CUE during analysis".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CueMode {
    /// Follow the user's own cues ([`CueOptions::tendency`]).
    Auto,
    /// The counts and bar options below.
    #[default]
    Manual,
}

/// CUE Analysis settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CueOptions {
    pub mode: CueMode,
    /// CUE Type: how many hot cues (0..=[`MAX_HOT`]) and memory cues (0..=[`MAX_MEMORY`]) a track gets (Manual).
    pub hot: u8,
    pub memory: u8,
    /// "Set to 1.1 Bars": the first cue is at the first bar.
    pub first_bar: bool,
    /// "Repeat CUE settings per N bars from 1.1 Bars" (Manual only).
    pub repeat: Option<u8>,
    /// "Prohibit overwriting CUEs": a track that has cues keeps them.
    pub keep_existing: bool,
    /// "Add a comment to CUE".
    pub comment: bool,
    /// What Auto follows; `None` until the collection has enough tracks with cues.
    pub tendency: Option<Tendency>,
}

impl Default for CueOptions {
    /// Before the options existed: 1.1 as a memory cue and a hot cue at each phrase change, up to eight.
    fn default() -> Self {
        CueOptions {
            mode: CueMode::Manual,
            hot: MAX_HOT,
            memory: 1,
            first_bar: false,
            repeat: None,
            keep_existing: false,
            comment: false,
            tendency: None,
        }
    }
}

/// The options Auto or Manual boils down to.
struct Plan {
    hot: usize,
    memory: usize,
    /// Bar (0 = 1.1) of the first cue.
    start: usize,
    /// Anchor at `start` even without a repeat.
    first: bool,
    repeat: Option<usize>,
    comment: bool,
}

impl Plan {
    fn of(o: &CueOptions) -> Plan {
        match (o.mode, o.tendency) {
            (CueMode::Auto, Some(t)) => Plan {
                hot: t.hot.min(MAX_HOT) as usize,
                memory: t.memory.min(MAX_MEMORY) as usize,
                start: t.offset_bars,
                first: true,
                repeat: t.spacing_bars.filter(|&n| n > 0),
                comment: o.comment,
            },
            _ => Plan {
                hot: o.hot.min(MAX_HOT) as usize,
                memory: o.memory.min(MAX_MEMORY) as usize,
                start: 0,
                first: o.first_bar,
                repeat: o.repeat.filter(|&n| n > 0).map(usize::from),
                comment: o.comment,
            },
        }
    }
}

/// Indices into `p.list` of the phrases that start a cue, best first (see the module doc).
fn phrase_order(p: &Phrases) -> Vec<usize> {
    let mut seen: Vec<PhraseKind> = Vec::new();
    let mut pick: Vec<(u8, usize)> = Vec::new(); // (priority, phrase), lowest priority first
    for (i, ph) in p.list.iter().enumerate() {
        if i > 0 && p.list[i - 1].kind == ph.kind {
            continue;
        }
        let first = !seen.contains(&ph.kind);
        seen.push(ph.kind);
        pick.push((
            if matches!(ph.kind, PhraseKind::Intro | PhraseKind::Outro) {
                0
            } else if first {
                1
            } else {
                2
            },
            i,
        ));
    }
    pick.sort();
    pick.into_iter().map(|(_, i)| i).collect()
}

/// The cues of a track with grid `beats` and `phrases` under `o`: memory cues first, then hot cues, both in time order;
/// nothing without a grid. Does not look at the track's existing cues ([`for_analysis`] does).
pub fn generate(beats: &[Beat], phrases: Option<&Phrases>, o: &CueOptions) -> Vec<Cue> {
    let bars = bar_times(beats);
    let Some(&down) = bars.first() else {
        return Vec::new();
    };
    let plan = Plan::of(o);
    let mut anchors: Vec<(u32, String)> = Vec::new();
    let mut add = |ms: u32, label: String| {
        if !anchors.iter().any(|a| a.0 == ms) {
            anchors.push((ms, label));
        }
    };
    let start = plan.start.min(bars.len() - 1);
    if plan.first || plan.repeat.is_some() {
        add(bars[start], format!("Bar {}", start + 1));
    }
    if let Some(n) = plan.repeat {
        for i in (start + n..bars.len()).step_by(n) {
            add(bars[i], format!("Bar {}", i + 1));
        }
    } else if let Some(p) = phrases {
        for i in phrase_order(p) {
            add(p.list[i].span_ms(beats).0.round() as u32, p.list[i].label());
        }
    }

    let cue = |hot_cue, (time_ms, label): &(u32, String), color| Cue {
        hot_cue,
        time_ms: *time_ms,
        loop_end_ms: None,
        color,
        comment: plan.comment.then(|| label.clone()),
    };
    let mut memory: Vec<(u32, String)> = vec![
        anchors
            .iter()
            .find(|a| a.0 == down)
            .cloned()
            .unwrap_or((down, "Bar 1".into())),
    ];
    memory.extend(anchors.iter().filter(|a| a.0 != down).cloned());
    memory.truncate(plan.memory);
    memory.sort_by_key(|a| a.0);
    let mut hot: Vec<&(u32, String)> = anchors.iter().take(plan.hot).collect();
    hot.sort_by_key(|a| a.0);
    let mut out: Vec<Cue> = memory.iter().map(|a| cue(0, a, None)).collect();
    out.extend(
        hot.into_iter()
            .enumerate()
            .map(|(n, a)| cue(n as u16 + 1, a, Some(COLORFUL[n]))),
    );
    out
}

/// The cues CUE Analysis stores for a track, or `None` to leave it alone: it has cues (`has_cues`) and "Prohibit
/// overwriting CUEs" is on, or the options give no cue (counts 0, no grid).
pub fn for_analysis(
    beats: &[Beat],
    phrases: Option<&Phrases>,
    o: &CueOptions,
    has_cues: bool,
) -> Option<Vec<Cue>> {
    if has_cues && o.keep_existing {
        return None;
    }
    Some(generate(beats, phrases, o)).filter(|c| !c.is_empty())
}

/// The track's hot cues and memory cue under the current options when it has no cues, else nothing: what a deck shows
/// for a track nobody analysed (session only).
pub fn suggest(a: &Analysis) -> Vec<Cue> {
    if a.cues.is_empty() {
        generate(&a.beats, a.phrases.as_ref(), &options::get().cue)
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Mood, Phrase};

    /// 120 BPM, `bars` bars (2 s each), bar_pos 1..=4.
    fn grid(bars: u32) -> Vec<Beat> {
        (0..bars * 4)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect()
    }

    /// Phrases `(start bar, kind)`, each running to the next, the last to bar 64.
    fn phrases(list: &[(u32, PhraseKind)]) -> Phrases {
        let list = list
            .iter()
            .enumerate()
            .map(|(i, &(bar, kind))| Phrase {
                beat: bar * 4 + 1,
                end_beat: list.get(i + 1).map_or(64 * 4 + 1, |n| n.0 * 4 + 1),
                kind,
                variant: 0,
                fill: None,
            })
            .collect();
        Phrases {
            mood: Mood::High,
            list,
        }
    }

    fn hot(c: &[Cue]) -> Vec<(u16, u32)> {
        c.iter()
            .filter(|c| c.hot_cue > 0)
            .map(|c| (c.hot_cue, c.time_ms))
            .collect()
    }
    fn memory(c: &[Cue]) -> Vec<u32> {
        c.iter()
            .filter(|c| c.hot_cue == 0)
            .map(|c| c.time_ms)
            .collect()
    }

    fn song() -> Phrases {
        use PhraseKind::*;
        phrases(&[
            (4, Intro),
            (12, Up),
            (20, Chorus),
            (36, Down),
            (44, Chorus),
            (60, Outro),
        ])
    }

    #[test]
    fn defaults_are_a_memory_cue_at_1_1_and_hot_cues_at_the_phrase_changes() {
        let c = generate(&grid(64), Some(&song()), &CueOptions::default());
        assert_eq!(memory(&c), [0]);
        assert_eq!(
            hot(&c),
            [
                (1, 8_000),
                (2, 24_000),
                (3, 40_000),
                (4, 72_000),
                (5, 88_000),
                (6, 120_000)
            ]
        );
        assert!(
            c.iter()
                .all(|c| c.comment.is_none() && c.loop_end_ms.is_none())
        );
        assert!(
            c.iter()
                .filter(|c| c.hot_cue > 0)
                .all(|c| c.color.is_some())
                && c[0].color.is_none()
        );
        assert_eq!(
            memory(&generate(&grid(64), None, &CueOptions::default())),
            [0],
            "no phrases: the memory cue only"
        );
        assert!(hot(&generate(&grid(64), None, &CueOptions::default())).is_empty());
        assert!(generate(&[], Some(&song()), &CueOptions::default()).is_empty());
    }

    #[test]
    fn counts_keep_the_most_important_cues_in_time_order() {
        let o = CueOptions {
            hot: 3,
            memory: 2,
            ..Default::default()
        };
        let c = generate(&grid(64), Some(&song()), &o);
        // Intro, Outro, then the first Up
        assert_eq!(hot(&c), [(1, 8_000), (2, 24_000), (3, 120_000)]);
        // 1.1 and the best anchor (Intro at bar 5)
        assert_eq!(memory(&c), [0, 8_000]);
        let none = CueOptions {
            hot: 0,
            memory: 0,
            ..Default::default()
        };
        assert!(generate(&grid(64), Some(&song()), &none).is_empty());
        let many = CueOptions {
            hot: 200,
            memory: 200,
            ..Default::default()
        };
        let c = generate(&grid(64), Some(&song()), &many);
        assert_eq!(hot(&c).len(), 6, "only six phrase changes");
        assert_eq!(memory(&c).len(), 7, "1.1 and the six anchors");
    }

    #[test]
    fn set_to_1_1_puts_the_first_bar_in_front() {
        let o = CueOptions {
            first_bar: true,
            hot: 3,
            ..Default::default()
        };
        let c = generate(&grid(64), Some(&song()), &o);
        assert_eq!(
            hot(&c),
            [(1, 0), (2, 8_000), (3, 120_000)],
            "1.1, Intro, Outro; the Up phrase is pushed out"
        );
        assert_eq!(memory(&c), [0]);
    }

    #[test]
    fn repeat_every_n_bars_from_1_1() {
        let o = CueOptions {
            repeat: Some(16),
            hot: 8,
            memory: 3,
            ..Default::default()
        };
        let c = generate(&grid(64), Some(&song()), &o);
        assert_eq!(hot(&c), [(1, 0), (2, 32_000), (3, 64_000), (4, 96_000)]);
        assert_eq!(memory(&c), [0, 32_000, 64_000]);
        let c = generate(
            &grid(64),
            None,
            &CueOptions {
                repeat: Some(8),
                hot: 2,
                ..o
            },
        );
        assert_eq!(hot(&c), [(1, 0), (2, 16_000)], "repeating needs no phrases");
        assert_eq!(
            generate(
                &grid(3),
                None,
                &CueOptions {
                    repeat: Some(8),
                    ..o
                }
            )
            .len(),
            2,
            "one bar position, as hot and memory cue"
        );
    }

    #[test]
    fn comments_name_the_phrase_or_the_bar() {
        let by = |c: &[Cue], ms| {
            c.iter()
                .find(|c| c.time_ms == ms && c.hot_cue > 0)
                .and_then(|c| c.comment.clone())
        };
        let o = CueOptions {
            comment: true,
            hot: 2,
            ..Default::default()
        };
        let c = generate(&grid(64), Some(&song()), &o);
        assert_eq!(by(&c, 8_000).as_deref(), Some("Intro"));
        assert_eq!(by(&c, 120_000).as_deref(), Some("Outro"));
        assert_eq!(
            c.iter()
                .find(|c| c.hot_cue == 0)
                .unwrap()
                .comment
                .as_deref(),
            Some("Bar 1"),
            "1.1 is no phrase start here"
        );
        let c = generate(
            &grid(64),
            None,
            &CueOptions {
                repeat: Some(16),
                ..o
            },
        );
        assert_eq!(by(&c, 32_000).as_deref(), Some("Bar 17"));
    }

    #[test]
    fn prohibit_overwriting_leaves_tracks_with_cues_alone() {
        let (b, p) = (grid(64), song());
        let keep = CueOptions {
            keep_existing: true,
            ..Default::default()
        };
        assert!(for_analysis(&b, Some(&p), &keep, true).is_none());
        assert!(for_analysis(&b, Some(&p), &keep, false).is_some());
        assert!(
            for_analysis(&b, Some(&p), &CueOptions::default(), true).is_some(),
            "overwriting is allowed by default"
        );
        assert!(
            for_analysis(
                &b,
                Some(&p),
                &CueOptions {
                    hot: 0,
                    memory: 0,
                    ..Default::default()
                },
                false
            )
            .is_none(),
            "nothing to set"
        );
        assert!(
            for_analysis(&[], None, &CueOptions::default(), false).is_none(),
            "no grid"
        );
    }

    #[test]
    fn auto_follows_the_tendency_and_without_one_is_manual() {
        let t = Tendency {
            offset_bars: 4,
            spacing_bars: Some(8),
            hot: 4,
            memory: 2,
            tracks: 40,
        };
        let auto = CueOptions {
            mode: CueMode::Auto,
            tendency: Some(t),
            hot: 8,
            memory: 1,
            ..Default::default()
        };
        let c = generate(&grid(64), Some(&song()), &auto);
        assert_eq!(
            hot(&c),
            [(1, 8_000), (2, 24_000), (3, 40_000), (4, 56_000)],
            "bar 5, then every 8 bars"
        );
        assert_eq!(memory(&c), [0, 8_000]);
        let no_spacing = CueOptions {
            tendency: Some(Tendency {
                spacing_bars: None,
                ..t
            }),
            ..auto
        };
        let c = generate(&grid(64), Some(&song()), &no_spacing);
        assert_eq!(
            hot(&c)[0],
            (1, 8_000),
            "the first cue is at the usual bar, the rest follow the phrases"
        );
        let manual = CueOptions {
            mode: CueMode::Manual,
            ..auto
        };
        assert_eq!(
            generate(&grid(64), Some(&song()), &manual),
            generate(
                &grid(64),
                Some(&song()),
                &CueOptions {
                    tendency: None,
                    mode: CueMode::Auto,
                    ..auto
                }
            )
        );
        assert_eq!(
            generate(&grid(64), Some(&song()), &manual).len(),
            7,
            "Auto without a tendency uses the Manual counts (8 hot, 1 memory; six phrase changes)"
        );
    }
}
