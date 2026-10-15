//! Track Suggestion: the browser's list ranked for one track by a recommended condition. The
//! five kinds are Collection Radar, Era, Mood, Association and Streaming Radar (not offered). Everything here is **offline and ours**: the
//! measurements are [`openfader::analysis::similar`]'s (3-band waveform, grid, phrases, vocal regions, tags); no external
//! model is loaded, and no model is read from a user path.
//!
//! - [`radar`] (Collection Radar): similar sound (timbre + energy) inside the Radar conditions BPM / Key / Vocal.
//! - [`era`]: nearest release year (file tag).
//! - [`mood`]: the phrase analysis' mood (High / Mid / Low, else from the energy), energy, timbre, genre.
//! - [`association`]: same label / composer / artist / album, and how often two tracks sit together in a playlist or play
//!   history ([`Links`]).
//! - [`view`]: the Slint side: the SUGGEST tab's state, the anchor track, the table notes.
//!
//! **Excluded: Streaming Radar.** It ranks tracks of an online streaming service (Beatport, TIDAL, SoundCloud, Beatsource) and
//! needs the server-side Radar analysis; both are online-only, so the condition is not offered. "Radar Analysis" upload
//! (Preferences > Analysis) is excluded for the same reason; Collection Radar needs no upload here because our features
//! are measured locally ([`openfader::analysis::similar`]).
//!
//! Ranking is a pure function ([`rank`]) over [`Item`]s so the tests need no window and no files.

mod association;
mod era;
mod mood;
mod radar;
mod view;

pub use radar::Conditions;
pub use view::{State, wire};

use openfader::{analysis::similar::Profile, library::Track};
use std::collections::HashMap;

use super::table::bpm_key;
use crate::app::camelot::{Key, camelot};

/// The recommended conditions of the Track Suggestion window that work offline, in the order of its chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Radar,
    Era,
    Mood,
    Association,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Radar, Mode::Era, Mode::Mood, Mode::Association];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&m| m == self).unwrap_or(0)
    }
}

/// One track as the rankers see it: the library values plus what the index measured.
pub struct Item<'a> {
    pub track: &'a Track,
    pub profile: Option<&'a Profile>,
    /// [`openfader::store::key`]: identity for [`Links`].
    pub key: &'a str,
    /// BPM (library value, else bulk analysis); 0 unknown.
    pub bpm: f64,
    pub camelot: Option<Key>,
}

impl<'a> Item<'a> {
    pub fn new(track: &'a Track, profile: Option<&'a Profile>, key: &'a str) -> Self {
        let (bpm_x100, k) = bpm_key(track);
        Item {
            track,
            profile,
            key,
            bpm: bpm_x100 as f64 / 100.0,
            camelot: camelot(&k),
        }
    }

    pub fn features(&self) -> Option<&openfader::analysis::similar::Features> {
        self.profile?.features.as_ref()
    }

    pub fn year(&self) -> Option<u16> {
        self.profile?.tags.year
    }

    /// Library label, else the tag's.
    pub fn label(&self) -> &str {
        match (self.track.label.as_str(), self.profile) {
            ("", Some(p)) => &p.tags.label,
            (l, _) => l,
        }
    }

    pub fn composer(&self) -> &str {
        self.profile.map_or("", |p| &p.tags.composer)
    }
}

/// What a ranker says about one candidate.
pub enum Verdict {
    /// Suggested, `0..` (higher first), with the reason shown in the table.
    Score(f32, String),
    /// Not suggested (a condition excludes it, or nothing ties it to the anchor).
    Out,
    /// The data this mode needs is missing for the candidate (not analysed, no year tag).
    NoData,
}

/// Candidate `at` of the pool, ranked.
pub struct Pick {
    pub at: usize,
    pub score: f32,
    pub note: String,
}

pub struct Outcome {
    pub picks: Vec<Pick>,
    /// Candidates the mode could not judge ([`Verdict::NoData`]).
    pub no_data: usize,
    /// Why nothing can be ranked at all (the anchor lacks what the mode needs).
    pub blocked: Option<&'static str>,
}

/// How often tracks appear together with the anchor: pair weight by track key. Each playlist or history that holds the anchor
/// adds `1 / sqrt(length)` to every other track in it (a long list says less than a short one), doubled for a track right
/// next to the anchor (played after it, or listed beside it). A list longer than `max_len` (a playlist of the whole collection)
/// says nothing about two tracks and is skipped. The weights are ours.
#[derive(Default)]
pub struct Links(HashMap<String, f32>);

/// The longest list that counts when `pool` tracks are searched: half the pool, at least 10, at most 100.
pub fn max_list(pool: usize) -> usize {
    (pool / 2).clamp(10, 100)
}

impl Links {
    pub fn from_lists(anchor: &str, lists: &[Vec<String>], max_len: usize) -> Links {
        let mut w: HashMap<String, f32> = HashMap::new();
        for list in lists.iter().filter(|l| l.len() <= max_len) {
            let at: Vec<usize> = (0..list.len()).filter(|&i| list[i] == anchor).collect();
            if at.is_empty() {
                continue;
            }
            let unit = 1.0 / (list.len() as f32).sqrt();
            let mut seen: HashMap<&str, f32> = HashMap::new();
            for (i, k) in list.iter().enumerate().filter(|(_, k)| *k != anchor) {
                let near = at.iter().any(|&a| a.abs_diff(i) == 1);
                let v = seen.entry(k).or_default();
                *v = v.max(if near { 2.0 * unit } else { unit });
            }
            for (k, v) in seen {
                *w.entry(k.to_owned()).or_default() += v;
            }
        }
        Links(w)
    }

    pub fn weight(&self, key: &str) -> f32 {
        self.0.get(key).copied().unwrap_or(0.0)
    }
}

/// `pool` ranked for `anchor` under `mode`, best first. The anchor itself (same `key`) is never in the result.
pub fn rank(mode: Mode, anchor: &Item, pool: &[Item], cond: &Conditions, links: &Links) -> Outcome {
    let blocked = match mode {
        Mode::Radar | Mode::Mood if anchor.features().is_none() => {
            Some("the track has no analysis data yet (ANALYZE TRACK)")
        }
        Mode::Era if anchor.year().is_none() => Some("the track has no year tag"),
        _ => None,
    };
    let mut out = Outcome {
        picks: Vec::new(),
        no_data: 0,
        blocked,
    };
    if blocked.is_some() {
        return out;
    }
    for (at, c) in pool.iter().enumerate().filter(|(_, c)| c.key != anchor.key) {
        match match mode {
            Mode::Radar => radar::score(anchor, c, cond),
            Mode::Era => era::score(anchor, c),
            Mode::Mood => mood::score(anchor, c),
            Mode::Association => association::score(anchor, c, links),
        } {
            Verdict::Score(score, note) => out.picks.push(Pick { at, score, note }),
            Verdict::Out => {}
            Verdict::NoData => out.no_data += 1,
        }
    }
    out.picks
        .sort_by(|a, b| b.score.total_cmp(&a.score).then(a.at.cmp(&b.at)));
    out
}

/// `1` at `d == 0`, falling to `0` at `window`.
fn closeness(d: f64, window: f64) -> f32 {
    (1.0 - d / window).clamp(0.0, 1.0) as f32
}

/// Case-insensitive, non-empty equality of two text fields.
fn same(a: &str, b: &str) -> bool {
    !a.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim())
}

/// Test fixtures shared by the rankers' tests.
#[cfg(test)]
pub(super) mod testing {
    use super::*;
    use openfader::analysis::similar::{Features, Tags};

    pub fn track(title: &str, bpm: f64, key: &str) -> Track {
        Track {
            title: title.into(),
            key: key.into(),
            bpm_x100: (bpm * 100.0) as i64,
            ..Default::default()
        }
    }

    pub fn feat(bands: [f32; 3], energy: f32) -> Features {
        Features {
            bands,
            energy,
            dynamics: 0.2,
            vocal: None,
            phrase_mood: None,
        }
    }

    pub fn profile(f: Option<Features>, year: Option<u16>) -> Profile {
        Profile {
            features: f,
            tags: Tags {
                year,
                ..Default::default()
            },
        }
    }

    /// Titles of the picks, in rank order.
    pub fn titles<'a>(o: &Outcome, pool: &'a [Item]) -> Vec<&'a str> {
        o.picks
            .iter()
            .map(|p| pool[p.at].track.title.as_str())
            .collect()
    }
}
