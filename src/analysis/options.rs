//! What analysis does, set from Preferences > Analysis: which parts run (Track Analysis Setting), how
//! the beat grid is detected ([`GridOptions`]) and how CUE Analysis places cues ([`CueOptions`]).
//!
//! The app copies preferences here with [`set`]. Interactive entry points read [`get`]; bulk runs capture their own
//! options so later preference changes cannot alter queued work. Tests use the `*_with` functions without changing
//! the global.

use super::{auto::CueOptions, detect::GridOptions, detect::Mode, key::Detector, Analysis};
use std::sync::RwLock;

/// Track Analysis Setting: the parts of an analysis that run; a disabled part is left empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    /// BPM / Grid.
    pub grid: bool,
    /// KEY.
    pub key: bool,
    /// Phrase (our phrases are derived from the grid and waveform, so they need the grid too).
    pub phrase: bool,
    /// Vocal regions ([`super::detect::vocal_regions`]).
    pub vocal: bool,
}

impl Default for Parts {
    fn default() -> Self {
        Parts {
            grid: true,
            key: true,
            phrase: true,
            vocal: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Options {
    pub parts: Parts,
    pub grid: GridOptions,
    pub cue: CueOptions,
    /// What the key part runs. The caller sets what can run: a [`Detector::Neural`] whose model is missing is asked as
    /// [`Detector::Classic`] ([`super::key::neural_ready`]).
    pub key_detector: Detector,
}

/// What a stored analysis was made with: which parts it has and (when it has a grid) the grid detector's settings.
/// The analysis cache keeps one next to each result so a changed setting is not answered from an old result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile(pub u64);

const GRID: u64 = 1;
const KEY: u64 = 2;
const VOCAL: u64 = 4;
/// With KEY: the key came from the neural detector. A result of one detector never answers a request for the other.
const NEURAL: u64 = 8;
const PARTS: u64 = 0xff;

impl Options {
    pub fn profile(&self) -> Profile {
        let p = &self.parts;
        let mut bits = [(p.grid, GRID), (p.key, KEY), (p.vocal, VOCAL)]
            .iter()
            .filter(|x| x.0)
            .fold(0, |a, x| a | x.1);
        if p.key && self.key_detector == Detector::Neural {
            bits |= NEURAL;
        }
        if p.grid {
            let g = &self.grid;
            let mode = match g.mode {
                Mode::Normal => 0,
                Mode::Dynamic => 1,
                Mode::Auto => 2,
            };
            bits |= (mode
                | (g.precision as u64) << 2
                | (g.bpm_range.0 as u64) << 3
                | (g.bpm_range.1 as u64) << 15)
                << 8;
        }
        Profile(bits)
    }

    /// Empties the parts of `a` that are switched off (a cached analysis may hold more than is asked for now).
    pub fn restrict(&self, a: &mut Analysis) {
        if !self.parts.grid {
            a.beats.clear();
        }
        if !self.parts.key {
            a.key = None;
        }
        if !self.parts.vocal {
            a.vocal.clear();
        }
        if !self.parts.phrase || !self.parts.grid {
            a.phrases = None;
        }
    }
}

impl Profile {
    /// An analysis with this profile answers a request for `want`: it has every part `want` runs, and when `want` runs
    /// the grid detector, it ran it with the same settings.
    pub fn covers(self, want: Profile) -> bool {
        let (have, ask) = (self.0, want.0);
        ask & PARTS & !have == 0
            && (ask & KEY == 0 || ask & NEURAL == have & NEURAL)
            && (ask >> 8 == 0 || ask >> 8 == have >> 8)
    }
}

static CURRENT: RwLock<Option<Options>> = RwLock::new(None);

/// The current options (the defaults until [`set`] was called).
pub fn get() -> Options {
    CURRENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or_default()
}

pub fn set(o: Options) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(o);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_analysis_answers_only_requests_it_covers() {
        let all = Options::default();
        let no_key = Options {
            parts: Parts {
                key: false,
                ..Parts::default()
            },
            ..all
        };
        assert!(
            all.profile().covers(no_key.profile()),
            "more than asked for is fine"
        );
        assert!(
            !no_key.profile().covers(all.profile()),
            "a part that never ran must be computed"
        );
        let precise = Options {
            grid: GridOptions {
                precision: true,
                ..all.grid
            },
            ..all
        };
        assert!(
            !all.profile().covers(precise.profile()) && !precise.profile().covers(all.profile()),
            "another grid setting is another grid"
        );
        let wide = Options {
            grid: GridOptions {
                bpm_range: (110, 220),
                ..all.grid
            },
            ..all
        };
        assert!(!all.profile().covers(wide.profile()));
        let no_grid = Options {
            parts: Parts {
                grid: false,
                ..Parts::default()
            },
            ..all
        };
        assert!(
            precise.profile().covers(no_grid.profile()),
            "a request without the grid ignores grid settings"
        );
        assert!(!no_grid.profile().covers(all.profile()));
        // a grid made without tempo-change detection is not the grid detection would make
        let steady = Options {
            grid: GridOptions {
                mode: Mode::Normal,
                ..all.grid
            },
            ..all
        };
        assert!(!steady.profile().covers(all.profile()) && !all.profile().covers(steady.profile()));
    }

    #[test]
    fn a_key_of_one_detector_never_answers_a_request_for_the_other() {
        let classic = Options::default();
        let neural = Options {
            key_detector: Detector::Neural,
            ..classic
        };
        assert!(!classic.profile().covers(neural.profile()));
        assert!(!neural.profile().covers(classic.profile()));
        assert!(neural.profile().covers(neural.profile()));
        let no_key = Options {
            parts: Parts {
                key: false,
                ..Parts::default()
            },
            ..neural
        };
        assert_eq!(
            no_key.profile(),
            Options {
                key_detector: Detector::Classic,
                ..no_key
            }
            .profile(),
            "without the key part the detector is no part of the request"
        );
        assert!(
            classic.profile().covers(no_key.profile()) && neural.profile().covers(no_key.profile())
        );
    }

    #[test]
    fn restrict_empties_the_switched_off_parts() {
        let a = Analysis {
            beats: vec![super::super::Beat {
                bar_pos: 1,
                bpm_x100: 12000,
                time_ms: 0,
            }],
            key: Some("8A".into()),
            vocal: vec![super::super::detect::VocalRegion {
                start_ms: 0,
                end_ms: 5000,
            }],
            ..Default::default()
        };
        let mut b = a.clone();
        Options::default().restrict(&mut b);
        assert_eq!(
            (b.beats.len(), b.key.as_deref(), b.vocal.len()),
            (1, Some("8A"), 1)
        );
        let o = Options {
            parts: Parts {
                grid: false,
                key: false,
                phrase: true,
                vocal: false,
            },
            ..Default::default()
        };
        let mut c = a.clone();
        o.restrict(&mut c);
        assert!(c.beats.is_empty() && c.key.is_none() && c.vocal.is_empty());
    }
}
