//! ANALYSIS category (Preferences > Analysis: Track Analysis, CUE Analysis; values and defaults are
//! our choice). Typed section [`Analysis`] plus the page rows; [`crate::app::analyze`] hands it to the analysis code
//! ([`openfader::analysis::options`], workers, tags, stored cues) whenever it changes.
//!
//! | item | setting | wired to |
//! | --- | --- | --- |
//! | high precision | `high_precision` | [`openfader::analysis::detect::GridOptions::precision`] (1 ms flux frames, sub-frame onsets) |
//! | tempo changes, BPM range | `detect_tempo_changes`, `bpm_range` | [`openfader::analysis::detect::Mode`] (on = Auto: sections of one tempo each where the tempo changes, one steady grid otherwise; off = Normal), `bpm_range` (octave fold + search band) |
//! | parts | `part_*` | [`openfader::analysis::options::Parts`]: a part off is not computed and left empty |
//! | grid source | `grid_source` | which beat grid and cues a LOAD uses first: the connected library's or ours ([`crate::app::deck`]); the user's own edits win over both |
//! | Auto Analysis | `auto_analysis` | the background run at start ([`crate::app::analyze::wire`]); off = only ANALYZE NOW / the track menu |
//! | write key | `key_write_tag` | [`openfader::analysis::tag::write_key`] after each analysis, never otherwise |
//! | process | `process` | worker threads and their priority ([`crate::app::analyze`]) |
//! | CUE | `cue_*` | [`openfader::analysis::auto::CueOptions`] (pure cue generation), stored through the store overlay |
//! | playlist | `view.panel_cue_analysis` | shared with VIEW; the tracks are [`crate::app::analyze::cue_analysed`] |

use super::{choice, section, Prefs, Row};
use openfader::analysis::auto::{MAX_HOT, MAX_MEMORY, MIN_TRACKS};

choice!(
    /// BPM Range for Normal analysis: the tempo is doubled or halved
    /// into it. Every range spans an octave, so exactly one reading fits.
    BpmRange { R70 = "70-140", R80 = "80-160", #[default] R85 = "85-175", R90 = "90-180", R100 = "100-200", R110 = "110-220" }
);
choice!(
    /// Analysis Process: Performance (as fast as possible) or Power saving (lowest CPU load).
    Process { #[default] Performance = "performance", PowerSaving = "power-saving" }
);
choice!(
    /// Set CUE during analysis.
    CueSetting { Auto = "auto", #[default] Manual = "manual" }
);
choice!(
    /// Where a LOAD takes a track's beat grid and cues from first: the connected library (what it holds, ours fills the gaps)
    /// or our own analysis (the library's grid and cues are ignored). The user's own edits win over both.
    GridSource { #[default] Library = "library", Ours = "ours" }
);
choice!(
    /// Key Analysis detector: the chroma/profile method, or a small neural network (a model downloaded on first use).
    KeyDetector { #[default] Classic = "classic", Neural = "neural" }
);

impl BpmRange {
    /// Lowest and highest BPM of the range.
    pub fn bounds(self) -> (u16, u16) {
        match self {
            BpmRange::R70 => (70, 140),
            BpmRange::R80 => (80, 160),
            BpmRange::R85 => (85, 175),
            BpmRange::R90 => (90, 180),
            BpmRange::R100 => (100, 200),
            BpmRange::R110 => (110, 220),
        }
    }
}

/// The N values of "Repeat CUE settings per N bars from 1.1 Bars".
pub const REPEAT_BARS: [u8; 7] = [1, 2, 4, 8, 16, 32, 64];

section!(
    Analysis {
        /// Auto Analysis of imported / newly seen tracks.
        auto_analysis: bool = true;
        /// Use high precision BeatGrid analysis, Detect tempo changes, BPM range of the grid.
        high_precision: bool = false;
        detect_tempo_changes: bool = true;
        bpm_range: BpmRange = BpmRange::R85;
        /// Track Analysis Setting: BPM / Grid, KEY, Phrase, Vocal.
        part_grid: bool = true;
        part_key: bool = true;
        part_phrase: bool = true;
        part_vocal: bool = true;
        /// Beat grid and cues from the connected library or from our own analysis.
        grid_source: GridSource = GridSource::Library;
        /// Key Analysis > detector.
        key_detector: KeyDetector = KeyDetector::Classic;
        /// Key Analysis > Write the value to the ID3 tag.
        key_write_tag: bool = false;
        /// Analysis Process.
        process: Process = Process::Performance;
        /// CUE Analysis: Set CUE during analysis, CUE Type counts, CUE Setting.
        cue_mode: CueSetting = CueSetting::Manual;
        cue_hot: u8 = 8;
        cue_memory: u8 = 1;
        cue_first_bar: bool = false;
        cue_repeat: bool = false;
        cue_repeat_bars: u8 = 16;
        cue_keep: bool = false;
        cue_comment: bool = false;
    }
);

fn numbers(n: usize) -> Vec<String> {
    (0..=n).map(|i| i.to_string()).collect()
}

pub fn rows(p: &Prefs) -> Vec<Row> {
    let a = &p.analysis;
    let learnt = crate::app::analyze::tendency_status();
    let auto_cue = a.cue_mode == CueSetting::Auto;
    let followed = auto_cue && learnt.tendency.is_some(); // Auto with a tendency overrides the counts and bar options
    let manual_only = |r: Row| r.enabled(!followed);
    let repeat_at = REPEAT_BARS
        .iter()
        .position(|&n| n == a.cue_repeat_bars)
        .unwrap_or(4);
    let mut rows = vec![
        Row::header(tr!("TRACK ANALYSIS")),
        Row::check(
            "high_precision",
            tr!("Use high precision BeatGrid analysis"),
            a.high_precision,
        )
        .hint(tr!("1 ms frames")),
        Row::check(
            "detect_tempo_changes",
            tr!("Detect tempo changes"),
            a.detect_tempo_changes,
        )
        .hint(tr!("a tempo section where the tempo changes; a steady track stays one")),
        Row::of("bpm_range", tr!("BPM RANGE"), a.bpm_range).hint("BPM"),
        Row::header(tr!("TRACK ANALYSIS SETTING")),
        Row::check("part_grid", tr!("BPM / Grid"), a.part_grid),
        Row::check("part_key", tr!("KEY"), a.part_key),
        Row::check("part_phrase", tr!("Phrase"), a.part_phrase).hint(tr!("needs the grid")),
        Row::check("part_vocal", tr!("Vocal"), a.part_vocal).hint(tr!("heuristic")),
        Row::chips(
            "grid_source",
            tr!("BEAT GRID AND CUES FROM"),
            &[tr!("THE CONNECTED LIBRARY"), tr!("OPENFADER ANALYSIS")],
            a.grid_source as usize,
        )
        .hint(tr!("your own edits win over both")),
        Row::header(tr!("AUTO ANALYSIS")),
        Row::check("auto_analysis", tr!("Auto Analysis"), a.auto_analysis)
            .hint(tr!("tracks without a BPM or key")),
        Row::chips(
            "analyze_now",
            tr!("ANALYZE LIBRARY"),
            &[tr!("ANALYZE NOW")],
            usize::MAX,
        ),
        Row::header(tr!("KEY ANALYSIS")),
        Row::chips(
            "key_detector",
            tr!("KEY DETECTOR"),
            &[tr!("CLASSIC"), tr!("NEURAL")],
            a.key_detector as usize,
        )
        .hint(if cfg!(feature = "stems") {
            tr!("NEURAL: a CNN (MIT licence), downloaded on first use, CPU only")
        } else {
            tr!("NEURAL needs a build with ONNX Runtime")
        })
        .enabled(a.part_key && cfg!(feature = "stems")),
        Row::check(
            "key_write_tag",
            tr!("Write the value to the ID3 tag"),
            a.key_write_tag,
        )
        .hint("MP3, WAV, AIFF, FLAC")
        .enabled(a.part_key),
        Row::header(tr!("ANALYSIS PROCESS")),
        Row::of("process", tr!("ANALYSIS PROCESS"), a.process).hint(&tr!(
            "{} worker thread(s)",
            crate::app::analyze::workers(a.process)
        )),
        Row::header(tr!("CUE ANALYSIS")),
        Row::chips(
            "cue_mode",
            tr!("SET CUE DURING ANALYSIS"),
            &[tr!("AUTO"), tr!("MANUAL")],
            a.cue_mode as usize,
        ),
    ];
    if auto_cue {
        rows.push(match &learnt.tendency {
            Some(t) => Row::info(
                tr!("PERSONAL CUE TENDENCY"),
                &tr!(
                    "{} tracks: first cue at bar {}, {}, {} hot / {} memory",
                    t.tracks,
                    t.offset_bars + 1,
                    t.spacing_bars
                        .map_or(tr!("on phrases").to_owned(), |n| tr!("every {} bars", n)),
                    t.hot,
                    t.memory
                ),
            ),
            None => Row::info(
                tr!("PERSONAL CUE TENDENCY"),
                &tr!(
                    "needs {} tracks with cues ({} found): MANUAL settings apply",
                    MIN_TRACKS, learnt.found
                ),
            ),
        });
    }
    rows.extend([
        manual_only(Row::dropdown_of(
            "cue_hot",
            tr!("HOT CUES"),
            numbers(MAX_HOT as usize),
            a.cue_hot.min(MAX_HOT) as usize,
        )),
        manual_only(Row::dropdown_of(
            "cue_memory",
            tr!("MEMORY CUES"),
            numbers(MAX_MEMORY as usize),
            a.cue_memory.min(MAX_MEMORY) as usize,
        )),
        manual_only(Row::check(
            "cue_first_bar",
            tr!("Set to 1.1 Bars"),
            a.cue_first_bar,
        )),
        Row::check(
            "cue_repeat",
            tr!("Repeat CUE settings per N bars from 1.1 Bars"),
            a.cue_repeat && !auto_cue,
        )
        .enabled(!auto_cue),
        Row::dropdown_of(
            "cue_repeat_bars",
            tr!("N BARS"),
            REPEAT_BARS.iter().map(|n| n.to_string()).collect(),
            repeat_at,
        )
        .enabled(!auto_cue && a.cue_repeat),
        Row::check("cue_keep", tr!("Prohibit overwriting CUEs"), a.cue_keep),
        Row::check("cue_comment", tr!("Add a comment to CUE"), a.cue_comment)
            .hint(tr!("phrase or bar")),
        Row::check(
            "panel_cue_analysis",
            tr!("Display the CUE Analysis playlist"),
            p.view.panel_cue_analysis,
        ),
    ]);
    rows
}

pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    let a = &mut p.analysis;
    match key {
        "cue_hot" => a.cue_hot = i.min(MAX_HOT as usize) as u8,
        "cue_memory" => a.cue_memory = i.min(MAX_MEMORY as usize) as u8,
        "cue_repeat_bars" => {
            a.cue_repeat_bars = REPEAT_BARS.get(i).copied().unwrap_or(a.cue_repeat_bars)
        }
        "analyze_now" => super::run_action(key),
        // the same setting as VIEW > Layout > Browser panel > CUE Analysis Playlist
        "panel_cue_analysis" => p.view.panel_cue_analysis = i == 1,
        _ => return a.pick(key, i),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(p: &Prefs, key: &str) -> Row {
        rows(p).into_iter().find(|r| r.key == key).unwrap()
    }

    #[test]
    fn tempo_change_detection_is_on_and_a_checkbox() {
        let mut p = Prefs::default();
        assert!(p.analysis.detect_tempo_changes && row(&p, "detect_tempo_changes").current == 1);
        assert!(choose(&mut p, "detect_tempo_changes", 0));
        assert!(!p.analysis.detect_tempo_changes);
    }

    #[test]
    fn numbers_are_clamped_and_bar_steps_are_a_list() {
        let mut p = Prefs::default();
        choose(&mut p, "cue_hot", 99);
        choose(&mut p, "cue_memory", 99);
        assert_eq!((p.analysis.cue_hot, p.analysis.cue_memory), (8, 10));
        choose(&mut p, "cue_repeat_bars", 2);
        assert_eq!(p.analysis.cue_repeat_bars, 4);
        choose(&mut p, "cue_repeat_bars", 99);
        assert_eq!(p.analysis.cue_repeat_bars, 4);
        p.analysis.cue_hot = 200; // a hand-edited file: the dropdown still shows a valid entry
        assert_eq!(row(&p, "cue_hot").current, 8);
    }

    #[test]
    fn dependent_rows_follow_their_switches() {
        let mut p = Prefs::default();
        assert!(
            !row(&p, "cue_repeat_bars").enabled,
            "N follows the repeat checkbox"
        );
        choose(&mut p, "cue_repeat", 1);
        assert!(row(&p, "cue_repeat_bars").enabled && row(&p, "cue_repeat").enabled);
        choose(&mut p, "cue_mode", 0);
        assert!(
            !row(&p, "cue_repeat").enabled,
            "the bar repeat is Manual only"
        );
        assert!(
            rows(&p).iter().any(|r| r.label == "PERSONAL CUE TENDENCY"),
            "Auto explains what it follows"
        );
        p.analysis.part_key = false;
        assert!(
            !row(&p, "key_write_tag").enabled,
            "no key analysis, no key to write"
        );
    }
}
