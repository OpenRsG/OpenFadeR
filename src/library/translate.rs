//! Translation layer: fits a collection to what a library format can hold ([`Capabilities`]) before it is written, and counts
//! what did not fit ([`Loss`]) so the user is told. Pure: no I/O. The rules, per track analysis:
//!
//! * hot cues above slot `hot_cues` are dropped; of the memory cues the `memory_cues` earliest stay;
//! * without `loops`, a loop becomes a plain cue at its start;
//! * with a `cue_colors` palette, a cue colour becomes the nearest palette entry (squared RGB distance);
//! * without `tempo_changes`, the grid becomes one steady tempo, that of the first beat, from the first beat on (same
//!   number of beats, so phrase beat numbers still point at the same bars);
//! * a rating above `rating_max` is rescaled to it (0: cleared);
//! * for a `.mp3` file, every cue and beat time moves by `mp3_offset_ms` (not a loss, so not reported).

use super::Track;
use crate::analysis::{Analysis, Beat, Cue};
use crate::paths::has_extension;
use openfader_plugin::library::{Capabilities, Snapshot};
use std::fmt;

/// What a [`fit`] changed, by kind (counts over the whole collection).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loss {
    pub hot_dropped: usize,
    pub memory_dropped: usize,
    pub loops_flattened: usize,
    pub colors_snapped: usize,
    /// Tracks whose tempo changes were replaced by one tempo.
    pub grids_collapsed: usize,
    pub ratings: usize,
    /// The limits the counts above are about, for the text.
    pub hot_limit: u16,
    pub memory_limit: u16,
    pub rating_max: u8,
}

impl Loss {
    pub fn is_empty(&self) -> bool {
        self.hot_dropped
            + self.memory_dropped
            + self.loops_flattened
            + self.colors_snapped
            + self.grids_collapsed
            + self.ratings
            == 0
    }

    /// "3 cues dropped (more than 8 hot cues) · 12 colours snapped"; empty when nothing was lost.
    pub fn text(&self) -> String {
        let s = |n: usize| if n == 1 { "" } else { "s" };
        let mut p: Vec<String> = Vec::new();
        if self.hot_dropped > 0 {
            let n = self.hot_dropped;
            p.push(format!(
                "{n} cue{} dropped (more than {} hot cues)",
                s(n),
                self.hot_limit
            ));
        }
        if self.memory_dropped > 0 {
            let n = self.memory_dropped;
            p.push(format!(
                "{n} cue{} dropped (more than {} memory cues)",
                s(n),
                self.memory_limit
            ));
        }
        if self.loops_flattened > 0 {
            let n = self.loops_flattened;
            p.push(format!("{n} loop{} made plain cue{}", s(n), s(n)));
        }
        if self.colors_snapped > 0 {
            let n = self.colors_snapped;
            p.push(format!("{n} colour{} snapped", s(n)));
        }
        if self.grids_collapsed > 0 {
            let n = self.grids_collapsed;
            p.push(format!("{n} beat grid{} set to one tempo", s(n)));
        }
        if self.ratings > 0 {
            let n = self.ratings;
            p.push(match self.rating_max {
                0 => format!("{n} rating{} dropped", s(n)),
                m => format!("{n} rating{} rescaled to {m} stars", s(n)),
            });
        }
        p.join(" · ")
    }
}

impl fmt::Display for Loss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

/// Whether `caps` leaves every collection as it is (so a copy to fit is not needed).
pub fn is_noop(caps: &Capabilities) -> bool {
    caps.hot_cues.is_none()
        && caps.memory_cues.is_none()
        && caps.loops
        && caps.cue_colors.is_empty()
        && caps.rating_max >= 5
        && caps.tempo_changes
        && caps.mp3_offset_ms == 0
}

/// Fits the tracks of `s` to `caps`; what changed is in the result.
pub fn fit(s: &mut Snapshot, caps: &Capabilities) -> Loss {
    fit_tracks(&mut s.tracks, caps)
}

/// [`fit`] for tracks that are not (yet) in a wire snapshot.
pub fn fit_tracks(tracks: &mut [(Track, Option<Analysis>)], caps: &Capabilities) -> Loss {
    let mut loss = Loss {
        hot_limit: caps.hot_cues.unwrap_or(0),
        memory_limit: caps.memory_cues.unwrap_or(0),
        rating_max: caps.rating_max,
        ..Loss::default()
    };
    for (t, a) in tracks {
        if caps.rating_max < 5 && t.rating > 0 {
            let r = if caps.rating_max == 0 {
                0
            } else {
                ((u16::from(t.rating) * u16::from(caps.rating_max) + 2) / 5).max(1) as u8
            };
            if r != t.rating {
                t.rating = r;
                loss.ratings += 1;
            }
        }
        let Some(a) = a else { continue };
        fit_cues(&mut a.cues, caps, &mut loss);
        if !caps.tempo_changes && collapse(&mut a.beats) {
            loss.grids_collapsed += 1;
        }
        let off = i64::from(caps.mp3_offset_ms);
        if off != 0 && t.file.as_deref().is_some_and(|f| has_extension(f, &["mp3"])) {
            let shift = |ms: &mut u32| *ms = (i64::from(*ms) + off).clamp(0, i64::from(u32::MAX)) as u32;
            for c in &mut a.cues {
                shift(&mut c.time_ms);
                c.loop_end_ms.iter_mut().for_each(shift);
            }
            for b in &mut a.beats {
                shift(&mut b.time_ms);
            }
        }
    }
    loss
}

fn fit_cues(cues: &mut Vec<Cue>, caps: &Capabilities, loss: &mut Loss) {
    if let Some(n) = caps.hot_cues {
        let before = cues.len();
        cues.retain(|c| c.hot_cue <= n);
        loss.hot_dropped += before - cues.len();
    }
    if let Some(n) = caps.memory_cues {
        let mut memory: Vec<(u32, usize)> = (cues.iter().enumerate())
            .filter(|(_, c)| c.hot_cue == 0)
            .map(|(i, c)| (c.time_ms, i))
            .collect();
        if memory.len() > n as usize {
            memory.sort_unstable();
            let mut keep = vec![true; cues.len()];
            for &(_, i) in &memory[n as usize..] {
                keep[i] = false;
            }
            loss.memory_dropped += memory.len() - n as usize;
            let mut i = 0;
            cues.retain(|_| {
                i += 1;
                keep[i - 1]
            });
        }
    }
    for c in cues {
        if !caps.loops && c.loop_end_ms.take().is_some() {
            loss.loops_flattened += 1;
        }
        if let Some(rgb) = &mut c.color
            && let Some(n) = nearest(*rgb, &caps.cue_colors)
            && n != *rgb
        {
            *rgb = n;
            loss.colors_snapped += 1;
        }
    }
}

/// The palette entry closest to `c`; `None` for an empty palette.
fn nearest(c: [u8; 3], palette: &[[u8; 3]]) -> Option<[u8; 3]> {
    let d = |p: &[u8; 3]| -> i32 { (0..3).map(|i| (i32::from(p[i]) - i32::from(c[i])).pow(2)).sum() };
    palette.iter().copied().min_by_key(d)
}

/// One steady tempo, the first beat's, from the first beat on; `false` when the grid already is one.
fn collapse(beats: &mut [Beat]) -> bool {
    let Some(first) = beats.first().copied() else {
        return false;
    };
    if beats.iter().all(|b| b.bpm_x100 == first.bpm_x100) {
        return false;
    }
    let step = 6_000_000.0 / f64::from(first.bpm_x100.max(1));
    for (i, b) in beats.iter_mut().enumerate() {
        b.time_ms = (f64::from(first.time_ms) + i as f64 * step).round() as u32;
        b.bpm_x100 = first.bpm_x100;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cue(hot: u16, ms: u32) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    fn beat(pos: u16, bpm: u16, ms: u32) -> Beat {
        Beat {
            bar_pos: pos,
            bpm_x100: bpm,
            time_ms: ms,
        }
    }

    fn snap(file: &str, rating: u8, a: Analysis) -> Snapshot {
        let t = Track {
            file: Some(PathBuf::from(file)),
            rating,
            ..Track::default()
        };
        Snapshot {
            tracks: vec![(t, Some(a))],
            ..Snapshot::default()
        }
    }

    fn analysis(s: &Snapshot) -> &Analysis {
        s.tracks[0].1.as_ref().unwrap()
    }

    #[test]
    fn default_capabilities_change_nothing() {
        let a = Analysis {
            cues: vec![cue(9, 1), cue(0, 2)],
            beats: vec![beat(1, 12000, 0), beat(2, 9000, 500)],
            ..Analysis::default()
        };
        let mut s = snap("a.mp3", 5, a);
        let before = s.tracks.clone();
        let loss = fit(&mut s, &Capabilities::default());
        assert!(loss.is_empty() && loss.text().is_empty());
        assert_eq!(s.tracks[0].0, before[0].0);
        assert_eq!(analysis(&s).cues, before[0].1.as_ref().unwrap().cues);
        assert!(is_noop(&Capabilities::default()));
    }

    #[test]
    fn hot_cues_above_the_slot_limit_are_dropped() {
        let a = Analysis {
            cues: vec![cue(1, 10), cue(8, 20), cue(9, 30), cue(0, 40)],
            ..Analysis::default()
        };
        let mut s = snap("a.flac", 0, a);
        let caps = Capabilities {
            hot_cues: Some(8),
            ..Capabilities::default()
        };
        let loss = fit(&mut s, &caps);
        let kept: Vec<_> = analysis(&s).cues.iter().map(|c| c.time_ms).collect();
        assert_eq!(kept, [10, 20, 40]);
        assert_eq!(loss.text(), "1 cue dropped (more than 8 hot cues)");
    }

    #[test]
    fn memory_cues_keep_the_earliest() {
        let a = Analysis {
            cues: vec![cue(0, 300), cue(1, 5), cue(0, 100), cue(0, 200)],
            ..Analysis::default()
        };
        let mut s = snap("a.flac", 0, a);
        let caps = Capabilities {
            memory_cues: Some(2),
            ..Capabilities::default()
        };
        let loss = fit(&mut s, &caps);
        let kept: Vec<_> = analysis(&s).cues.iter().map(|c| c.time_ms).collect();
        assert_eq!(kept, [5, 100, 200], "order kept, the hot cue untouched");
        assert_eq!(loss.memory_dropped, 1);
        assert_eq!(loss.text(), "1 cue dropped (more than 2 memory cues)");
    }

    #[test]
    fn loops_become_plain_cues_without_loop_support() {
        let mut l = cue(1, 1000);
        l.loop_end_ms = Some(2000);
        let mut s = snap(
            "a.flac",
            0,
            Analysis {
                cues: vec![l, cue(2, 5)],
                ..Analysis::default()
            },
        );
        let caps = Capabilities {
            loops: false,
            ..Capabilities::default()
        };
        let loss = fit(&mut s, &caps);
        assert!(analysis(&s).cues.iter().all(|c| c.loop_end_ms.is_none()));
        assert_eq!(analysis(&s).cues[0].time_ms, 1000, "the start stays");
        assert_eq!(loss.text(), "1 loop made plain cue");
    }

    #[test]
    fn colors_snap_to_the_nearest_palette_entry() {
        let mut a = cue(1, 0);
        a.color = Some([250, 10, 10]);
        let mut b = cue(2, 0);
        b.color = Some([0, 255, 0]); // already in the palette
        let mut c = cue(3, 0);
        c.color = Some([10, 10, 240]);
        let mut s = snap(
            "a.flac",
            0,
            Analysis {
                cues: vec![a, b, c, cue(4, 0)],
                ..Analysis::default()
            },
        );
        let caps = Capabilities {
            cue_colors: vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]],
            ..Capabilities::default()
        };
        let loss = fit(&mut s, &caps);
        let colors: Vec<_> = analysis(&s).cues.iter().map(|c| c.color).collect();
        assert_eq!(
            colors,
            [Some([255, 0, 0]), Some([0, 255, 0]), Some([0, 0, 255]), None]
        );
        assert_eq!(loss.text(), "2 colours snapped");
    }

    #[test]
    fn tempo_changes_collapse_to_the_first_beats_tempo() {
        let beats = vec![
            beat(1, 12000, 100),
            beat(2, 12000, 600),
            beat(3, 9000, 1100),
            beat(4, 9000, 1767),
        ];
        let mut s = snap(
            "a.flac",
            0,
            Analysis {
                beats,
                ..Analysis::default()
            },
        );
        let caps = Capabilities {
            tempo_changes: false,
            ..Capabilities::default()
        };
        let loss = fit(&mut s, &caps);
        let got: Vec<_> = analysis(&s)
            .beats
            .iter()
            .map(|b| (b.bar_pos, b.bpm_x100, b.time_ms))
            .collect();
        assert_eq!(
            got,
            [(1, 12000, 100), (2, 12000, 600), (3, 12000, 1100), (4, 12000, 1600)]
        );
        assert_eq!(loss.text(), "1 beat grid set to one tempo");
        assert!(fit(&mut s, &caps).is_empty(), "steady grids are left alone");
    }

    #[test]
    fn ratings_are_rescaled_or_cleared() {
        let mut s = snap("a.flac", 4, Analysis::default());
        let three = Capabilities {
            rating_max: 3,
            ..Capabilities::default()
        };
        let loss = fit(&mut s, &three);
        assert_eq!(s.tracks[0].0.rating, 2);
        assert_eq!(loss.text(), "1 rating rescaled to 3 stars");

        let none = Capabilities {
            rating_max: 0,
            ..Capabilities::default()
        };
        assert_eq!(fit(&mut s, &none).text(), "1 rating dropped");
        assert_eq!(s.tracks[0].0.rating, 0);
        assert!(fit(&mut s, &none).is_empty(), "no rating, nothing to drop");
    }

    #[test]
    fn mp3_offset_moves_only_mp3_files() {
        let a = || Analysis {
            cues: vec![{
                let mut c = cue(1, 1000);
                c.loop_end_ms = Some(2000);
                c
            }],
            beats: vec![beat(1, 12000, 20)],
            ..Analysis::default()
        };
        let caps = Capabilities {
            mp3_offset_ms: 50,
            ..Capabilities::default()
        };
        let mut mp3 = snap("/m/Song.MP3", 0, a());
        let loss = fit(&mut mp3, &caps);
        let x = analysis(&mp3);
        assert_eq!((x.cues[0].time_ms, x.cues[0].loop_end_ms), (1050, Some(2050)));
        assert_eq!(x.beats[0].time_ms, 70);
        assert!(loss.is_empty(), "a shift is not a loss");

        let mut flac = snap("/m/song.flac", 0, a());
        fit(&mut flac, &caps);
        assert_eq!(analysis(&flac).cues[0].time_ms, 1000);
        assert_eq!(analysis(&flac).beats[0].time_ms, 20);

        let early = Capabilities {
            mp3_offset_ms: -50,
            ..Capabilities::default()
        };
        let mut s = snap("a.mp3", 0, a());
        fit(&mut s, &early);
        assert_eq!(analysis(&s).beats[0].time_ms, 0, "never before the start");
    }

    #[test]
    fn mp3_offset_never_wraps_past_the_top() {
        let mut s = snap("a.mp3", 0, Analysis { beats: vec![beat(1, 12000, u32::MAX - 10)], ..Analysis::default() });
        let late = Capabilities { mp3_offset_ms: 50, ..Capabilities::default() };
        fit(&mut s, &late);
        assert_eq!(analysis(&s).beats[0].time_ms, u32::MAX);
    }

    #[test]
    fn text_joins_the_kinds() {
        let loss = Loss {
            hot_dropped: 3,
            hot_limit: 8,
            colors_snapped: 12,
            ..Loss::default()
        };
        assert_eq!(
            loss.to_string(),
            "3 cues dropped (more than 8 hot cues) · 12 colours snapped"
        );
    }
}
