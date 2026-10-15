//! Preferences > Analysis as the analysis code takes it: [`openfader::analysis::options::Options`] (parts, grid, cue
//! options) and the worker count of Analysis Process.

use crate::app::prefs::analysis::{Analysis, CueSetting, Process, TrackMode};
use openfader::analysis::{
    auto::{CueMode, CueOptions, MAX_HOT, MAX_MEMORY, Tendency},
    detect::{GridOptions, Mode},
    options::{Options, Parts},
};

/// The analysis options for `a`; `tendency` is what CUE Analysis Auto follows (`None` = it falls back to Manual).
pub fn options(a: &Analysis, tendency: Option<Tendency>) -> Options {
    Options {
        parts: Parts {
            grid: a.part_grid,
            key: a.part_key,
            phrase: a.part_phrase,
            vocal: a.part_vocal,
        },
        grid: GridOptions {
            mode: match a.mode() {
                TrackMode::Normal => Mode::Normal,
                TrackMode::Dynamic => Mode::Dynamic,
                TrackMode::Auto => Mode::Auto,
            },
            precision: a.high_precision,
            bpm_range: a.bpm_range.bounds(),
        },
        cue: CueOptions {
            mode: match a.cue_mode {
                CueSetting::Auto => CueMode::Auto,
                CueSetting::Manual => CueMode::Manual,
            },
            hot: a.cue_hot.min(MAX_HOT),
            memory: a.cue_memory.min(MAX_MEMORY),
            first_bar: a.cue_first_bar,
            repeat: Some(a.cue_repeat_bars).filter(|&n| a.cue_repeat && n > 0),
            keep_existing: a.cue_keep,
            comment: a.cue_comment,
            tendency,
        },
    }
}

/// Analysis Process: Performance runs one worker per core but one (the UI keeps a core), at most
/// [`MAX_WORKERS`]; Power saving runs a single worker at low priority ([`super::job`]). Both counts are our choice.
pub fn workers(process: Process, cores: usize) -> usize {
    match process {
        Process::Performance => cores.saturating_sub(1).clamp(1, MAX_WORKERS),
        Process::PowerSaving => 1,
    }
}

/// Each worker holds a whole decoded track plus its analysis (about 0.5 GB for 10 minutes, measured 2026-10-15), and each
/// analysis already runs on three threads (grid, key, vocal): more workers would mostly add memory.
const MAX_WORKERS: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::analysis::BpmRange;

    #[test]
    fn settings_map_onto_the_analysis_options() {
        let a = Analysis {
            high_precision: true,
            track_mode: TrackMode::Auto,
            bpm_range: BpmRange::R70,
            part_key: false,
            cue_hot: 99,
            cue_repeat: true,
            cue_repeat_bars: 8,
            cue_keep: true,
            cue_comment: true,
            ..Default::default()
        };
        let o = options(&a, None);
        assert_eq!(
            (o.grid.mode, o.grid.precision, o.grid.bpm_range),
            (Mode::Auto, true, (70, 140))
        );
        assert_eq!(
            (o.parts.grid, o.parts.key, o.parts.phrase, o.parts.vocal),
            (true, false, true, true)
        );
        assert_eq!(
            (
                o.cue.hot,
                o.cue.repeat,
                o.cue.keep_existing,
                o.cue.comment,
                o.cue.mode
            ),
            (MAX_HOT, Some(8), true, true, CueMode::Manual)
        );
        let no_repeat = Analysis {
            cue_repeat: false,
            ..a.clone()
        };
        assert_eq!(
            options(&no_repeat, None).cue.repeat,
            None,
            "the N value counts only with its checkbox"
        );
        let no_precision = Analysis {
            high_precision: false,
            ..a
        };
        assert_eq!(
            options(&no_precision, None).grid.mode,
            Mode::Normal,
            "Auto without high precision is Normal"
        );
    }

    #[test]
    fn workers_follow_the_process_setting() {
        assert_eq!(workers(Process::Performance, 4), 3);
        assert_eq!(
            workers(Process::Performance, 16),
            4,
            "capped: each worker holds a whole track"
        );
        assert_eq!(workers(Process::Performance, 2), 1);
        assert_eq!(workers(Process::Performance, 1), 1);
        assert_eq!(workers(Process::PowerSaving, 16), 1);
    }
}
