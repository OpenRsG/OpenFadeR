//! Tracks whose stored analysis was made by an older version of the analysis (a new cache layout or detector after an update):
//! found at start by their cache entry ([`cache::age`]), then PREFS > ANALYSIS "When an update changes the analysis" decides.
//! ASK shows a notice with the number of tracks and the time it takes (measured on this machine, with the settings in use),
//! IN THE BACKGROUND re-analyses them quietly, WHEN A TRACK IS LOADED leaves each one to its next load (the old way).
//!
//! Whatever the choice, a re-analysis only replaces the track's cache entry: its own cues, grid, phrases and tags and the
//! analysis lock are never touched ([`Job::refresh`], and locked tracks are not listed).

use super::{detected, Job};
use crate::app::{
    prefs::{self, analysis::AnalysisUpdate},
    AnalysisNoticeState, App,
};
use openfader::{
    analysis::{
        cache::{self, Age},
        options::{self, Options},
        Execution,
    },
    library::Track,
    store,
};
use slint::ComponentHandle;
use std::{cell::RefCell, collections::HashSet, path::PathBuf, time::Duration, time::Instant};

thread_local! {
    /// What RE-ANALYSE NOW will run (UI thread).
    static STALE: RefCell<Vec<Track>> = const { RefCell::new(Vec::new()) };
}

/// Audio seconds assumed for a track the library gives no length.
const TYPICAL_SECS: i64 = 240;

/// Of `tracks`, those with a file, an older analysis ([`Age::Older`]), no analysis lock, and not in `skip` (Auto Analysis
/// analyses those anyway).
fn find(
    tracks: &[Track],
    age: &dyn Fn(&Track) -> Age,
    locked: &dyn Fn(&Track) -> bool,
    skip: &HashSet<PathBuf>,
) -> Vec<Track> {
    tracks
        .iter()
        .filter(|t| t.file.as_ref().is_some_and(|f| !skip.contains(f)))
        .filter(|t| age(t) == Age::Older && !locked(t))
        .cloned()
        .collect()
}

/// How long analysing `tracks` takes when one second of audio costs `factor` seconds of work and `workers` run at once;
/// a track with no length counts as the average of those that have one.
fn estimate(tracks: &[Track], factor: f64, workers: usize) -> Duration {
    let known: Vec<i64> = tracks
        .iter()
        .map(|t| t.length_s)
        .filter(|&s| s > 0)
        .collect();
    let typical = known
        .iter()
        .sum::<i64>()
        .checked_div(known.len() as i64)
        .unwrap_or(TYPICAL_SECS);
    let audio: f64 = tracks
        .iter()
        .map(|t| if t.length_s > 0 { t.length_s } else { typical } as f64)
        .sum();
    Duration::from_secs_f64(audio * factor / workers.max(1) as f64)
}

/// "under a minute", "about 12 min", "about 2 h 5 min".
fn how_long(d: Duration) -> String {
    let min = (d.as_secs_f64() / 60.0).round() as u64;
    match min {
        0 => tr!("under a minute").into(),
        1..=89 => tr!("about {} min", min),
        _ => tr!("about {} h {} min", min / 60, min % 60),
    }
}

/// The text of the notice: how many tracks, how long (`None` = not measured) and what stays as it is.
fn notice(n: usize, took: Option<Duration>) -> String {
    let count = tr_n!(
        "{n} track was analysed by an older version of the analysis.",
        "{n} tracks were analysed by an older version of the analysis.",
        n
    );
    let time = match took {
        Some(d) => tr!(
            "RE-ANALYSE NOW redoes it in the background: {} at the speed measured on this computer.",
            how_long(d)
        ),
        None => tr!("RE-ANALYSE NOW redoes it in the background; the time could not be measured.")
            .to_owned(),
    };
    let kept = tr!(
        "Your own cues, grids and tracks with the analysis lock are not changed. LATER asks again at the next start."
    );
    format!("{count} {time} {kept}")
}

/// Work seconds per audio second of this machine: `audio` seconds of audio analysed in `took`; `None` for a clip too short
/// to say anything.
fn factor(audio: f64, took: Duration) -> Option<f64> {
    (audio >= 1.0).then(|| took.as_secs_f64() / audio)
}

/// Decodes and analyses the first of `tracks` that decodes, under `o`, and times it: the real work, nothing stored. Blocks
/// for as long as the analysis of one track takes: run it on a worker thread.
fn measure(tracks: &[Track], o: &Options) -> Option<f64> {
    tracks.iter().take(3).find_map(|t| {
        let start = Instant::now();
        let (pcm, _) =
            openfader::audio::decode_track16_cancellable(t.file.as_deref()?, &|| false).ok()?;
        let audio = pcm.frames() as f64 / f64::from(pcm.rate.max(1));
        openfader::analysis::analyze_cancellable(&pcm, pcm.rate, o, Execution::Bulk, &|| false)?;
        factor(audio, start.elapsed())
    })
}

/// At start, after the background run for tracks lacking a BPM or key: lists the tracks an older analysis made and does what
/// the setting says.
pub(super) fn check() {
    let policy = prefs::with(|p| p.analysis.analysis_update);
    let Some((app, lib)) = super::context().filter(|_| policy != AnalysisUpdate::OnLoad) else {
        return;
    };
    let Ok(tracks) = lib.tracks_all() else { return };
    let skip: HashSet<PathBuf> = if prefs::with(|p| p.analysis.auto_analysis) {
        detected::pending(&tracks, &super::detected(), &options::get().parts)
            .into_iter()
            .filter_map(|t| t.file.clone())
            .collect()
    } else {
        HashSet::new()
    };
    let (weak, o) = (app.as_weak(), options::get());
    std::thread::spawn(move || {
        let store = store::global();
        let locked = |t: &Track| store.analysis_locked(&store::key(t)).unwrap_or(false);
        let stale = find(&tracks, &cache::age, &locked, &skip);
        if stale.is_empty() {
            return;
        }
        let factor = (policy == AnalysisUpdate::Ask)
            .then(|| measure(&stale, &o))
            .flatten();
        let _ = weak.upgrade_in_event_loop(move |app| found(&app, stale, policy, factor));
    });
}

/// The answer of [`check`] on the UI thread.
fn found(app: &App, stale: Vec<Track>, policy: AnalysisUpdate, factor: Option<f64>) {
    eprintln!(
        "analysis: {} tracks were analysed by an older version",
        stale.len()
    );
    match policy {
        AnalysisUpdate::Background => refresh(app, &stale),
        AnalysisUpdate::Ask => {
            let workers = super::workers(prefs::with(|p| p.analysis.process));
            let took = factor.map(|f| estimate(&stale, f, workers));
            let s = app.global::<AnalysisNoticeState>();
            s.set_text(notice(stale.len(), took).into());
            s.set_visible(true);
            STALE.set(stale);
        }
        AnalysisUpdate::OnLoad => {}
    }
}

/// Starts the re-analysis of `tracks`: their cache entries are replaced, nothing else of theirs is.
fn refresh(app: &App, tracks: &[Track]) {
    let Some((_, lib)) = super::context() else {
        return;
    };
    let refs: Vec<&Track> = tracks.iter().collect();
    let jobs = super::jobs(&*lib, &refs, false)
        .into_iter()
        .map(|j| Job { refresh: true, ..j })
        .collect();
    super::start_run(app, jobs, prefs::with(super::settings));
}

/// Connects the notice's RE-ANALYSE NOW and LATER.
pub(super) fn wire(app: &App) {
    let s = app.global::<AnalysisNoticeState>();
    let weak = app.as_weak();
    s.on_reanalyse(move || {
        let Some(app) = weak.upgrade() else { return };
        app.global::<AnalysisNoticeState>().set_visible(false);
        refresh(&app, &STALE.take());
    });
    let weak = app.as_weak();
    s.on_later(move || {
        STALE.take();
        if let Some(app) = weak.upgrade() {
            app.global::<AnalysisNoticeState>().set_visible(false);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(file: &str, length_s: i64) -> Track {
        Track {
            file: (!file.is_empty()).then(|| file.into()),
            length_s,
            ..Default::default()
        }
    }

    #[test]
    fn only_unlocked_tracks_with_an_older_analysis_are_listed() {
        let tracks = vec![
            track("/m/old.mp3", 100),
            track("/m/current.mp3", 100),
            track("/m/none.mp3", 100),
            track("/m/locked.mp3", 100),
            track("/m/queued.mp3", 100),
            track("", 100),
        ];
        let age = |t: &Track| match t.file.as_deref().and_then(|f| f.to_str()) {
            Some("/m/current.mp3") => Age::Current,
            Some("/m/none.mp3") => Age::None,
            _ => Age::Older,
        };
        let locked = |t: &Track| t.file.as_deref().is_some_and(|f| f.ends_with("locked.mp3"));
        let skip = HashSet::from([PathBuf::from("/m/queued.mp3")]);
        let found = find(&tracks, &age, &locked, &skip);
        let files: Vec<_> = found.iter().filter_map(|t| t.file.clone()).collect();
        assert_eq!(files, [PathBuf::from("/m/old.mp3")]);
    }

    #[test]
    fn the_estimate_is_audio_time_times_the_measured_cost_over_the_workers() {
        let list = [track("a", 300), track("b", 100), track("c", 0)];
        // 100 s of audio costs 20 s of work: 0.2; the track without a length counts as the 200 s average.
        let one = estimate(&list, 0.2, 1);
        assert_eq!(one.as_secs(), 120);
        assert_eq!(estimate(&list, 0.2, 4).as_secs(), 30);
        assert_eq!(
            estimate(&list, 0.2, 0).as_secs(),
            120,
            "no workers counts as one"
        );
        assert_eq!(
            estimate(&[track("a", 0)], 1.0, 1).as_secs(),
            240,
            "nothing known: a typical track"
        );
        assert_eq!(estimate(&[], 1.0, 2), Duration::ZERO);
    }

    #[test]
    fn a_measurement_needs_a_clip_that_says_something() {
        assert_eq!(factor(60.0, Duration::from_secs(15)), Some(0.25));
        assert_eq!(factor(0.5, Duration::from_secs(1)), None);
    }

    #[test]
    fn times_read_as_minutes_and_hours() {
        let secs = |s| how_long(Duration::from_secs(s));
        assert_eq!(secs(20), "under a minute");
        assert_eq!(secs(12 * 60 + 10), "about 12 min");
        assert_eq!(secs(125 * 60), "about 2 h 5 min");
    }

    #[test]
    fn the_notice_says_how_many_how_long_and_what_stays() {
        let one = notice(1, Some(Duration::from_secs(300)));
        assert!(one.starts_with("1 track was analysed"), "{one}");
        assert!(
            one.contains("about 5 min") && one.contains("analysis lock"),
            "{one}"
        );
        let many = notice(52_000, None);
        assert!(many.starts_with("52000 tracks were"), "{many}");
        assert!(many.contains("could not be measured"), "{many}");
    }
}
