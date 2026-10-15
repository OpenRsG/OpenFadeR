//! Collection Radar: tracks that sound like the anchor, inside the Radar conditions (BPM, Key,
//! Vocal; "Streaming service" is online and not offered). The score is:
//! 60 % timbre / energy ([`Features::similarity`]), 25 % tempo closeness, 15 % key (same 1, compatible 0.7).

use super::{Item, Verdict, closeness};
use crate::app::camelot::{compatible, name};
use openfader::analysis::similar::Features;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BpmRange {
    /// ±6 %, the window of the browser's BPM hint.
    Near,
    /// ±12 %.
    Wide,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRule {
    /// Compatible at the traffic-light range chosen in PREFS.
    Related,
    Same,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VocalRule {
    Any,
    With,
    Without,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conditions {
    pub bpm: BpmRange,
    pub key: KeyRule,
    pub vocal: VocalRule,
}

impl Default for Conditions {
    fn default() -> Self {
        Conditions {
            bpm: BpmRange::Near,
            key: KeyRule::Related,
            vocal: VocalRule::Any,
        }
    }
}

impl Conditions {
    /// The chips' texts: BPM, KEY, VOCAL.
    pub fn labels(&self) -> [&'static str; 3] {
        [
            match self.bpm {
                BpmRange::Near => "BPM ±6%",
                BpmRange::Wide => "BPM ±12%",
                BpmRange::Any => "BPM ANY",
            },
            match self.key {
                KeyRule::Related => "KEY RELATED",
                KeyRule::Same => "KEY SAME",
                KeyRule::Any => "KEY ANY",
            },
            match self.vocal {
                VocalRule::Any => "VOCAL ANY",
                VocalRule::With => "VOCAL WITH",
                VocalRule::Without => "NO VOCAL",
            },
        ]
    }

    /// Steps chip `i` (0 BPM, 1 KEY, 2 VOCAL) to its next choice.
    pub fn step(&mut self, i: usize) {
        match i {
            0 => {
                self.bpm = match self.bpm {
                    BpmRange::Near => BpmRange::Wide,
                    BpmRange::Wide => BpmRange::Any,
                    BpmRange::Any => BpmRange::Near,
                }
            }
            1 => {
                self.key = match self.key {
                    KeyRule::Related => KeyRule::Same,
                    KeyRule::Same => KeyRule::Any,
                    KeyRule::Any => KeyRule::Related,
                }
            }
            2 => {
                self.vocal = match self.vocal {
                    VocalRule::Any => VocalRule::With,
                    VocalRule::With => VocalRule::Without,
                    VocalRule::Without => VocalRule::Any,
                }
            }
            _ => {}
        }
    }
}

pub(super) fn score(a: &Item, c: &Item, cond: &Conditions) -> Verdict {
    let (Some(fa), Some(fc)) = (a.features(), c.features()) else {
        return Verdict::NoData;
    };
    let window = match cond.bpm {
        BpmRange::Near => 0.06,
        BpmRange::Wide => 0.12,
        BpmRange::Any => 0.5,
    };
    let gap = if a.bpm > 0.0 && c.bpm > 0.0 {
        Some((c.bpm - a.bpm) / a.bpm)
    } else {
        None
    };
    if cond.bpm != BpmRange::Any && !gap.is_some_and(|g| g.abs() <= window) {
        return Verdict::Out;
    }
    let key = match (a.camelot, c.camelot) {
        (Some(x), Some(y)) if x == y => 1.0,
        (Some(x), Some(y)) if compatible(x, y) => 0.7,
        _ => 0.0,
    };
    match (cond.key, key) {
        (KeyRule::Same, k) if k < 1.0 => return Verdict::Out,
        (KeyRule::Related, k) if k == 0.0 => return Verdict::Out,
        _ => {}
    }
    match (cond.vocal, Features::has_vocal(fc)) {
        (VocalRule::With, Some(true)) | (VocalRule::Without, Some(false)) | (VocalRule::Any, _) => {
        }
        (_, None) => return Verdict::NoData,
        _ => return Verdict::Out,
    }
    let timbre = fa.similarity(fc);
    let tempo = closeness(gap.map_or(window, f64::abs), window.max(0.12));
    let score = 0.6 * timbre + 0.25 * tempo + 0.15 * key;
    let bpm = gap.map_or(String::new(), |g| format!(" · {:+.1} BPM", g * a.bpm));
    let key = match (a.camelot, c.camelot) {
        (Some(x), Some(y)) => format!(" · {} → {}", name(x), name(y)),
        _ => String::new(),
    };
    Verdict::Score(
        score,
        format!(
            "{:.0}% · sound {:.0}%{bpm}{key}",
            score * 100.0,
            timbre * 100.0
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Item, Links, Mode, rank, testing::*};
    use super::*;
    use openfader::analysis::similar::Profile;

    const TECHNO: [f32; 3] = [0.55, 0.30, 0.15];
    const BRIGHT: [f32; 3] = [0.15, 0.30, 0.55];

    /// Anchor 126 BPM 8A, techno-like.
    fn fixture() -> (Vec<openfader::library::Track>, Vec<Profile>) {
        let tracks = vec![
            track("anchor", 126.0, "8A"),
            track("twin", 127.0, "8A"),          // same sound, same key
            track("neighbour-key", 125.0, "9A"), // same sound, compatible key
            track("bright", 126.0, "8A"),        // other sound
            track("fast", 140.0, "8A"),          // too fast for ±6 %
            track("clash", 126.0, "3B"),         // key does not mix
            track("unanalysed", 126.0, "8A"),
        ];
        let p = |b, e| profile(Some(feat(b, e)), None);
        let profiles = vec![
            p(TECHNO, 0.6),
            p(TECHNO, 0.6),
            p(TECHNO, 0.58),
            p(BRIGHT, 0.3),
            p(TECHNO, 0.6),
            p(TECHNO, 0.6),
            profile(None, None),
        ];
        (tracks, profiles)
    }

    fn items<'a>(t: &'a [openfader::library::Track], p: &'a [Profile]) -> Vec<Item<'a>> {
        t.iter()
            .zip(p)
            .map(|(t, p)| Item::new(t, Some(p), &t.title))
            .collect()
    }

    #[test]
    fn similar_sound_in_a_mixable_key_and_tempo_comes_first() {
        let (t, p) = fixture();
        let pool = items(&t, &p);
        let o = rank(
            Mode::Radar,
            &pool[0],
            &pool,
            &Conditions::default(),
            &Links::default(),
        );
        assert_eq!(titles(&o, &pool), ["twin", "neighbour-key", "bright"]);
        assert_eq!(o.no_data, 1); // "unanalysed" cannot be judged
        assert!(o.picks[0].note.contains("8A → 8A") && o.picks[0].note.contains("+1.0 BPM"));
    }

    #[test]
    fn conditions_widen_and_narrow_the_list() {
        let (t, p) = fixture();
        let pool = items(&t, &p);
        let rank_with = |c: Conditions| {
            let o = rank(Mode::Radar, &pool[0], &pool, &c, &Links::default());
            titles(&o, &pool)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let any = Conditions {
            bpm: BpmRange::Any,
            key: KeyRule::Any,
            vocal: VocalRule::Any,
        };
        assert!(
            rank_with(any).contains(&"fast".to_owned())
                && rank_with(any).contains(&"clash".to_owned())
        );
        let same = Conditions {
            key: KeyRule::Same,
            ..Conditions::default()
        };
        assert_eq!(rank_with(same), ["twin", "bright"]);
    }

    #[test]
    fn the_vocal_condition_needs_a_vocal_analysis() {
        let (t, mut p) = fixture();
        for (i, v) in [(1usize, 0.5f32), (2, 0.0)] {
            p[i].features.as_mut().unwrap().vocal = Some(v);
        }
        let pool = items(&t, &p);
        let with = Conditions {
            vocal: VocalRule::With,
            ..Conditions::default()
        };
        let o = rank(Mode::Radar, &pool[0], &pool, &with, &Links::default());
        assert_eq!(titles(&o, &pool), ["twin"]);
        assert_eq!(o.no_data, 2); // "bright" has no vocal analysis, nor has "unanalysed"
        let without = Conditions {
            vocal: VocalRule::Without,
            ..Conditions::default()
        };
        let o = rank(Mode::Radar, &pool[0], &pool, &without, &Links::default());
        assert_eq!(titles(&o, &pool), ["neighbour-key"]);
    }

    #[test]
    fn an_anchor_without_analysis_blocks_the_radar() {
        let (t, p) = fixture();
        let pool = items(&t, &p);
        let o = rank(
            Mode::Radar,
            &pool[6],
            &pool,
            &Conditions::default(),
            &Links::default(),
        );
        assert!(o.picks.is_empty() && o.blocked.is_some());
    }

    #[test]
    fn chips_step_through_their_choices_and_back() {
        let mut c = Conditions::default();
        for i in 0..3 {
            let first = c.labels()[i];
            (0..3).for_each(|_| c.step(i));
            assert_eq!(c.labels()[i], first);
        }
    }
}
