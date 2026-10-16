//! The worker side of a bulk run: a pool of threads takes tracks off one list, analyses each ([`cache`]: a hit skips
//! the decode, an explicit request recomputes), then does what the settings ask of a finished analysis: CUE Analysis
//! stores cues ([`super::cue`]) and Key Analysis writes the key into the file's tag. Everything here
//! runs off the UI thread; results go back over a channel.

use super::{store_cues, summary, Outcome};
use openfader::{
    analysis::{
        auto,
        cache::{self, Source},
        options::Options,
        tag, Analysis, Beat, Phrases,
    },
    library::Track,
    store::Store,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
};

/// What the library already knows about a track, gathered on the UI thread: the grid and phrases cues are placed on (the
/// library's own, not ours: its cues must sit on its grid) and whether it has cues.
#[derive(Default)]
pub struct Guide {
    pub beats: Vec<Beat>,
    pub phrases: Option<Phrases>,
    pub has_cues: bool,
}

pub struct Job {
    pub path: PathBuf,
    /// What the analysis is of: its identity names the stored entry ([`cache`]).
    pub track: Track,
    /// [`openfader::store::key`] of the track.
    pub key: String,
    /// Asked for by the user (the track menu): analysed again whatever the cache holds, and its key is written to the tag
    /// even when the library has one.
    pub explicit: bool,
    /// The library gave the track a key.
    pub library_key: bool,
    pub guide: Guide,
}

/// The key as the file's tag spells it (the user's key notation).
pub type TagText = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// How a run goes.
#[derive(Clone)]
pub struct Settings {
    pub options: Options,
    /// Key Analysis "Write the value to the ID3 tag": `Some` = write, spelling the key this way.
    pub tag: Option<TagText>,
    pub workers: usize,
    /// Analysis Process "Power saving": the workers run at the lowest priority.
    pub low_priority: bool,
}

/// The cues and tag of one analysed track (`a`), the part of [`spawn`]'s work that is not decoding.
fn finish(job: &Job, a: &Analysis, src: Source, s: &Settings, store: &Store) -> Outcome {
    let g = &job.guide;
    let (beats, phrases) = if g.beats.is_empty() {
        (a.beats.as_slice(), a.phrases.clone())
    } else {
        let own = || {
            s.options
                .parts
                .phrase
                .then(|| auto::phrases(&g.beats, &a.three_band_detail))
                .flatten()
        };
        (g.beats.as_slice(), g.phrases.clone().or_else(own))
    };
    // Cues stored after the job was made (a pad pressed meanwhile, cue sync from a device) are newer than this analysis: they stay.
    let arrived = !g.has_cues
        && store
            .edits(&job.key)
            .is_ok_and(|e| e.cues.is_some_and(|c| !c.is_empty()));
    let cues = !arrived
        && auto::for_analysis(beats, phrases.as_ref(), &s.options.cue, g.has_cues).is_some_and(
            |c| match store.system(|| store_cues(store, &job.key, &c)) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("analyze: {}: cues not stored: {e}", job.path.display());
                    false
                }
            },
        );
    let tag = s
        .tag
        .as_ref()
        .filter(|_| job.explicit || !job.library_key)
        .zip(a.key.as_deref())
        .map(|(text, key)| tag::write_key(&job.path, &text(key)));
    Outcome {
        found: summary(a),
        src,
        cues,
        tag,
    }
}

/// Lowers the calling thread's priority: nice 19 on Linux (niceness is per thread there), the background QoS class on macOS, the
/// lowest thread priority on Windows.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        }
        // PRIO_PROCESS = 0 with who = 0 is the calling thread on Linux.
        unsafe { setpriority(0, 0, 19) };
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: acts on the calling thread only.
        unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0) };
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
        };
        // SAFETY: acts on the calling thread only.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
    }
}

fn analyse(job: &Job) -> Result<(Analysis, Source), String> {
    if job.explicit {
        cache::reanalyze(&job.track).map(|a| (a, Source::Computed { ignored: None }))
    } else {
        cache::get_or_analyze(&job.track)
    }
}

/// Starts `s.workers` threads on `jobs`; each result arrives as `(file, outcome or error)`. The workers stop when `tx`'s
/// receiver is gone (after the track they are on).
pub fn spawn(
    jobs: Vec<Job>,
    s: Settings,
    store: Arc<Store>,
    tx: mpsc::Sender<(PathBuf, Result<Outcome, String>)>,
) {
    let (jobs, next) = (Arc::new(jobs), Arc::new(AtomicUsize::new(0)));
    for _ in 0..s.workers.clamp(1, jobs.len().max(1)) {
        let (jobs, next, s, store, tx) = (
            jobs.clone(),
            next.clone(),
            s.clone(),
            store.clone(),
            tx.clone(),
        );
        std::thread::spawn(move || {
            if s.low_priority {
                lower_priority();
            }
            while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                let r = analyse(job).map(|(a, src)| finish(job, &a, src, &s, &store));
                if tx.send((job.path.clone(), r)).is_err() {
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::{auto::CueOptions, Mood, Phrase, PhraseKind};

    fn beats() -> Vec<Beat> {
        (0..256)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect()
    }

    fn analysed() -> Analysis {
        let list = vec![
            (1, PhraseKind::Intro),
            (65, PhraseKind::Chorus),
            (129, PhraseKind::Outro),
        ]
        .into_iter()
        .map(|(beat, kind)| Phrase {
            beat,
            end_beat: beat + 64,
            kind,
            variant: 0,
            fill: None,
        })
        .collect();
        Analysis {
            beats: beats(),
            key: Some("8A".into()),
            phrases: Some(Phrases {
                mood: Mood::High,
                list,
            }),
            ..Default::default()
        }
    }

    fn job(path: &str, guide: Guide) -> Job {
        Job {
            path: path.into(),
            track: Track {
                file: Some(path.into()),
                ..Track::default()
            },
            key: format!("{path}|1"),
            explicit: false,
            library_key: false,
            guide,
        }
    }

    fn settings(tag: bool) -> Settings {
        Settings {
            options: Options::default(),
            tag: tag.then(|| Arc::new(|k: &str| format!("<{k}>")) as TagText),
            workers: 1,
            low_priority: false,
        }
    }

    fn src() -> Source {
        Source::Computed { ignored: None }
    }

    /// A temp dir holding a fake mp3, removed afterwards (never the user's music).
    fn temp_mp3(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rk-job-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp3");
        std::fs::write(&file, [0xff, 0xfb, 0x90, 0x64, 1, 2, 3]).unwrap();
        (dir, file)
    }

    #[test]
    fn cue_analysis_stores_cues_on_the_library_grid_and_marks_the_track() {
        let store = Store::in_memory().unwrap();
        let j = job("/m/a.mp3", Guide::default());
        let out = finish(&j, &analysed(), src(), &settings(false), &store);
        assert!(out.cues && out.tag.is_none());
        let stored = store.edits(&j.key).unwrap().cues.unwrap();
        assert_eq!(stored.iter().filter(|c| c.hot_cue == 0).count(), 1);
        assert_eq!(
            stored
                .iter()
                .filter(|c| c.hot_cue > 0)
                .map(|c| c.time_ms)
                .collect::<Vec<_>>(),
            [0, 32_000, 64_000],
            "phrase starts of the analysis"
        );
        assert!(super::super::analysed(&store).contains(&j.key));
        assert_eq!(out.found, (12000, "8A".into()));

        // the library's own (shifted) grid wins over ours, with the library's phrases or phrases derived from it
        let shifted: Vec<Beat> = beats()
            .into_iter()
            .map(|b| Beat {
                time_ms: b.time_ms + 100,
                ..b
            })
            .collect();
        let j2 = job(
            "/m/b.mp3",
            Guide {
                beats: shifted,
                ..Default::default()
            },
        );
        finish(&j2, &analysed(), src(), &settings(false), &store);
        let on_library_grid = store.edits(&j2.key).unwrap().cues.unwrap();
        assert!(
            on_library_grid
                .iter()
                .filter(|c| c.hot_cue == 0)
                .all(|c| c.time_ms == 100),
            "1.1 of the library grid"
        );
    }

    #[test]
    fn cues_stored_while_the_track_was_analysed_stay() {
        let store = Store::in_memory().unwrap();
        let j = job("/m/a.mp3", Guide::default());
        let mine = vec![openfader::analysis::Cue {
            hot_cue: 1,
            time_ms: 4_000,
            loop_end_ms: None,
            color: None,
            comment: None,
        }];
        store.set_cues(&j.key, Some(&mine)).unwrap();
        assert!(!finish(&j, &analysed(), src(), &settings(false), &store).cues);
        assert_eq!(store.edits(&j.key).unwrap().cues, Some(mine));
    }

    #[test]
    fn prohibit_overwriting_and_zero_counts_leave_the_track_alone() {
        let store = Store::in_memory().unwrap();
        let had = job(
            "/m/a.mp3",
            Guide {
                has_cues: true,
                ..Default::default()
            },
        );
        let keep = Settings {
            options: Options {
                cue: CueOptions {
                    keep_existing: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..settings(false)
        };
        assert!(!finish(&had, &analysed(), src(), &keep, &store).cues);
        assert!(store.edits(&had.key).unwrap().cues.is_none());
        let overwrite = finish(&had, &analysed(), src(), &settings(false), &store);
        assert!(overwrite.cues, "overwriting is allowed unless prohibited");
        let none = Settings {
            options: Options {
                cue: CueOptions {
                    hot: 0,
                    memory: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..settings(false)
        };
        let other = job("/m/c.mp3", Guide::default());
        assert!(!finish(&other, &analysed(), src(), &none, &store).cues);
        assert!(store.edits(&other.key).unwrap().cues.is_none());
        let no_grid = Analysis {
            beats: Vec::new(),
            ..analysed()
        };
        assert!(
            !finish(&other, &no_grid, src(), &settings(false), &store).cues,
            "no grid, no cues"
        );
    }

    #[test]
    fn the_key_is_written_only_when_asked_and_only_to_the_file() {
        let store = Store::in_memory().unwrap();
        let (dir, file) = temp_mp3("tag");
        let path = file.to_str().unwrap();
        let before = std::fs::read(&file).unwrap();
        // off: the file is never touched
        assert!(finish(
            &job(path, Guide::default()),
            &analysed(),
            src(),
            &settings(false),
            &store
        )
        .tag
        .is_none());
        assert_eq!(std::fs::read(&file).unwrap(), before);
        // on: the key in the user's spelling
        let out = finish(
            &job(path, Guide::default()),
            &analysed(),
            src(),
            &settings(true),
            &store,
        );
        assert_eq!(out.tag, Some(Ok(true)));
        assert_eq!(tag::read_key(&file).as_deref(), Some("<8A>"));
        // a library key is respected by background analysis, not by an explicit request
        let mut j = job(path, Guide::default());
        j.library_key = true;
        let analysed_9b = Analysis {
            key: Some("9B".into()),
            ..analysed()
        };
        assert!(finish(&j, &analysed_9b, src(), &settings(true), &store)
            .tag
            .is_none());
        j.explicit = true;
        assert_eq!(
            finish(&j, &analysed_9b, src(), &settings(true), &store).tag,
            Some(Ok(true))
        );
        assert_eq!(tag::read_key(&file).as_deref(), Some("<9B>"));
        // no detected key (the KEY part off): nothing to write
        let no_key = Analysis {
            key: None,
            ..analysed()
        };
        assert!(finish(
            &job(path, Guide::default()),
            &no_key,
            src(),
            &settings(true),
            &store
        )
        .tag
        .is_none());
        // an unsupported file is refused, not damaged
        let m4a = dir.join("a.m4a");
        std::fs::write(&m4a, b"x").unwrap();
        let out = finish(
            &job(m4a.to_str().unwrap(), Guide::default()),
            &analysed(),
            src(),
            &settings(true),
            &store,
        );
        assert!(matches!(out.tag, Some(Err(_))));
        assert_eq!(std::fs::read(&m4a).unwrap(), b"x");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn power_saving_workers_run_at_the_lowest_priority() {
        unsafe extern "C" {
            fn getpriority(which: i32, who: u32) -> i32;
        }
        let nice = std::thread::spawn(|| {
            lower_priority();
            unsafe { getpriority(0, 0) }
        })
        .join()
        .unwrap();
        assert_eq!(nice, 19);
        let other = std::thread::spawn(|| unsafe { getpriority(0, 0) })
            .join()
            .unwrap();
        assert!(
            other < 19 || nice == other,
            "the lowered priority stays with that thread"
        );
    }

    #[test]
    fn the_pool_analyses_every_job_once_and_reports_failures() {
        let (tx, rx) = mpsc::channel();
        let jobs: Vec<Job> = (0..6)
            .map(|i| job(&format!("/nonexistent/{i}.mp3"), Guide::default()))
            .collect();
        spawn(
            jobs,
            Settings {
                workers: 3,
                ..settings(false)
            },
            Arc::new(Store::in_memory().unwrap()),
            tx,
        );
        let mut got: Vec<PathBuf> = rx
            .iter()
            .map(|(p, r)| {
                assert!(r.is_err(), "a missing file is an error, not a panic");
                p
            })
            .collect();
        got.sort();
        assert_eq!(got.len(), 6);
        got.dedup();
        assert_eq!(got.len(), 6, "each job exactly once");
    }
}
