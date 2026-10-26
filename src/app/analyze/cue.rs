//! CUE Analysis glue: finding the library's grid and cues of a track and learning the user's cue habits from
//! the collection. The cue generation itself is a pure function in [`openfader::analysis::auto`]; storing its result
//! and the CUE Analysis playlist marker are in the store ([`super::cue_analysed`]).

use openfader::{
    analysis::{
        Beat, Cue,
        auto::{self, Tendency},
    },
    library::{Library, Track},
    store,
};
use std::collections::HashSet;

/// Tracks looked at, and tracks with cues kept, when learning on a pool worker.
const SCAN: usize = 1500;
const SAMPLE: usize = 200;

/// What was learnt from the collection: the tendency, or why not (fewer than [`auto::MIN_TRACKS`] tracks with a grid and
/// cues; `found` of them were seen, up to the sample size).
#[derive(Clone, Debug, Default)]
pub struct Learnt {
    pub tendency: Option<Tendency>,
    pub found: usize,
}

/// The cues the library has for `t`; none without an analysis.
pub fn library_cues(lib: &dyn Library, t: &Track) -> Vec<Cue> {
    lib.analysis(t).map(|a| a.cues).unwrap_or_default()
}

/// The user's cue tendency from the cues the library and the store already hold. Tracks whose cues CUE Analysis set
/// itself (`marked`) do not count: Auto must follow the user, not its own output.
pub fn learn(
    tracks: &[Track],
    read: &dyn Fn(&Track) -> Option<openfader::analysis::Analysis>,
    marked: &HashSet<String>,
) -> Learnt {
    let mut have: Vec<(Vec<Beat>, Vec<Cue>)> = Vec::new();
    for t in tracks.iter().take(SCAN) {
        if have.len() >= SAMPLE {
            break;
        }
        if !marked.is_empty() && marked.contains(&store::key(t)) {
            continue;
        }
        if let Some(a) = read(t).filter(|a| !a.beats.is_empty() && !a.cues.is_empty()) {
            have.push((a.beats, a.cues));
        }
    }
    Learnt {
        found: have.len(),
        tendency: auto::derive_tendency(have.iter().map(|(b, c)| (b.as_slice(), c.as_slice()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::Analysis;
    use std::collections::HashMap;

    struct Fake(Vec<Track>, HashMap<std::path::PathBuf, Analysis>);

    impl Library for Fake {
        fn kind(&self) -> &str {
            "fake"
        }
        fn playlists(&self) -> Result<Vec<openfader::library::Playlist>, String> {
            Ok(Vec::new())
        }
        fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
            Ok(self.0.clone())
        }
        fn analysis(&self, t: &Track) -> Option<Analysis> {
            self.1.get(t.file.as_ref()?).cloned()
        }
        fn analysis_reader(&self) -> Option<openfader::library::AnalysisReader> {
            let source = Fake(self.0.clone(), self.1.clone());
            Some(std::sync::Arc::new(move |t| source.analysis(t)))
        }
    }

    fn cue(hot_cue: u16, bar: u32) -> Cue {
        Cue {
            hot_cue,
            time_ms: bar * 2000,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    /// `n` tracks (`/m/<i>.mp3`), each with a 64-bar 120 BPM grid and hot cues at bars 4, 20, 36.
    fn library(n: usize) -> Fake {
        let beats: Vec<Beat> = (0..256)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect();
        let (mut tracks, mut analysis) = (Vec::new(), HashMap::new());
        for i in 0..n {
            let file = std::path::PathBuf::from(format!("/m/{i}.mp3"));
            tracks.push(Track {
                file: Some(file.clone()),
                ..Default::default()
            });
            analysis.insert(
                file,
                Analysis {
                    beats: beats.clone(),
                    cues: vec![cue(1, 4), cue(2, 20), cue(3, 36)],
                    ..Default::default()
                },
            );
        }
        Fake(tracks, analysis)
    }

    #[test]
    fn learns_from_thirty_tracks_and_reports_fewer() {
        let lib = library(29);
        let few = learn(&lib.0, &|t| lib.analysis(t), &HashSet::new());
        assert_eq!((few.found, few.tendency), (29, None));
        let lib = library(40);
        let t = learn(&lib.0, &|t| lib.analysis(t), &HashSet::new())
            .tendency
            .unwrap();
        assert_eq!(
            (t.offset_bars, t.spacing_bars, t.hot, t.memory),
            (4, Some(16), 3, 0)
        );
    }

    #[test]
    fn tracks_cue_analysis_set_itself_do_not_teach_it() {
        let lib = library(40);
        // 11 of them were set by CUE Analysis: 29 left
        let marked: HashSet<String> = lib.0.iter().take(11).map(store::key).collect();
        let learnt = learn(&lib.0, &|t| lib.analysis(t), &marked);
        assert_eq!((learnt.found, learnt.tendency), (29, None));
    }
}
