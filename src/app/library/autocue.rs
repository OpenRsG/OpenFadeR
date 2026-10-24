//! Track menu > Make Auto Cue point: the selected tracks get their automatic cues. The cue point (a memory cue) goes on the
//! first sound at or above the Auto Cue Level (Preferences > Controller > Deck), the hot cues A.. where CUE Analysis puts
//! them (Preferences > Analysis: CUE Type counts, phrase changes on the beat grid, [`openfader::analysis::auto::generate`]),
//! plus the extra memory cues CUE Analysis asks for. A track's "cue point" is its earliest memory cue that is not a loop.
//! A track that has a cue point or a hot cue is asked about first: Replace moves its cue point and swaps its hot cues for
//! the automatic ones; Keep existing only fills what is missing (a cue point when it has none, the empty pads). Preferences >
//! Analysis > Prohibit overwriting CUEs means Keep, without asking. Loops and the other memory cues stay either way.
//!
//! One track at a time on a thread (decode, find the level, drop the PCM), progress in the status line; choosing the item
//! again while it runs stops it. The cues are stored as the user's own edit, in one undo step.

use super::{CTX, Confirm, Ctx, prefs};
use crate::app::{BrowserState, deck::first_sound, device, prefs::analysis::GridSource};
use openfader::{
    analysis::{self, Analysis, Cue, auto, cache},
    audio::decode_cancellable,
    library::Track,
    store::{self, Store},
};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// A track with the cues its source library gives it (what counts when the store holds no edit).
pub type Job = (Track, Vec<Cue>);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Replace,
    Keep,
}

#[derive(Default)]
struct Tally {
    /// Tracks whose cues changed.
    set: Vec<Track>,
    kept: usize,
    silent: usize,
    /// Tracks without a beat grid: no hot cues to place.
    no_grid: usize,
    failed: usize,
}

thread_local! {
    /// The stop flag of the batch that runs (UI thread).
    static RUN: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// The earliest memory cue that is not a loop.
fn cue_point(cues: &[Cue]) -> Option<usize> {
    (0..cues.len())
        .filter(|&i| cues[i].hot_cue == 0 && cues[i].loop_end_ms.is_none())
        .min_by_key(|&i| cues[i].time_ms)
}

/// Whether the track has cues the batch would touch (a cue point or a hot cue).
fn has_cues(cues: &[Cue]) -> bool {
    cue_point(cues).is_some() || cues.iter().any(|c| c.hot_cue > 0)
}

/// `cues` with the automatic ones: the cue point at `first` (the first sound; `None` = the earliest suggested memory cue),
/// the suggested hot cues and the suggested memory cues after the first. `None` when nothing changes.
fn with_auto(cues: &[Cue], first: Option<u32>, suggested: &[Cue], mode: Mode) -> Option<Vec<Cue>> {
    let mut memory: Vec<&Cue> = suggested
        .iter()
        .filter(|c| c.hot_cue == 0 && c.loop_end_ms.is_none())
        .collect();
    memory.sort_by_key(|c| c.time_ms);
    let at = first.or(memory.first().map(|c| c.time_ms));
    let had_point = cue_point(cues).is_some();
    let mut out: Vec<Cue> = match mode {
        Mode::Replace => cues.iter().filter(|c| c.hot_cue == 0).cloned().collect(),
        Mode::Keep => cues.to_vec(),
    };
    match (cue_point(&out), at, mode) {
        (Some(i), Some(at), Mode::Replace) => out[i].time_ms = at,
        (None, Some(at), _) => out.push(Cue {
            hot_cue: 0,
            time_ms: at,
            loop_end_ms: None,
            color: None,
            comment: None,
        }),
        _ => {}
    }
    for h in suggested.iter().filter(|c| c.hot_cue > 0) {
        if !out.iter().any(|c| c.hot_cue == h.hot_cue) {
            out.push(h.clone());
        }
    }
    if mode == Mode::Replace || !had_point {
        for m in memory.iter().skip(1) {
            if !out.iter().any(|c| c.hot_cue == 0 && c.time_ms == m.time_ms) {
                out.push((*m).clone());
            }
        }
    }
    (out != cues).then_some(out)
}

/// What the track shows: the user's stored list, else the library's.
fn current(store: &Store, key: &str, library: &[Cue]) -> Vec<Cue> {
    store
        .edits(key)
        .ok()
        .and_then(|e| e.cues)
        .unwrap_or_else(|| library.to_vec())
}

/// The batch: `grid(track)` gives the track's beat grid and phrases (with the user's edits), `progress(n)` comes before the
/// n-th track; `stop` ends it between tracks (and inside a decode).
fn work(
    store: &Store,
    jobs: &[Job],
    mode: Mode,
    db: f32,
    grid: &dyn Fn(&Track) -> Analysis,
    options: &auto::CueOptions,
    stop: &AtomicBool,
    progress: impl Fn(usize),
) -> Tally {
    let mut t = Tally::default();
    store.action(&tr!("Make Auto Cue point"), || {
        for (n, (track, library)) in jobs.iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            progress(n + 1);
            let key = store.key_of(track);
            let cues = current(store, &key, library);
            let a = grid(track);
            if a.beats.is_empty() {
                t.no_grid += 1;
            }
            let suggested = auto::generate(&a.beats, a.phrases.as_ref(), options);
            if mode == Mode::Keep && with_auto(&cues, Some(0), &suggested, mode).is_none() {
                t.kept += 1; // nothing missing: no need to decode
                continue;
            }
            let Some(file) = &track.file else {
                t.failed += 1;
                continue;
            };
            let first = match decode_cancellable(file, &|| stop.load(Ordering::Relaxed)) {
                Ok(pcm) => first_sound(&pcm.samples, pcm.rate, db),
                Err(_) if stop.load(Ordering::Relaxed) => break,
                Err(_) => {
                    t.failed += 1;
                    continue;
                }
            };
            if first.is_none() {
                t.silent += 1;
            }
            match with_auto(&cues, first, &suggested, mode) {
                Some(new) => match store.set_cues(&key, Some(&new)) {
                    Ok(()) => t.set.push(track.clone()),
                    Err(_) => t.failed += 1,
                },
                None => t.kept += 1,
            }
        }
    });
    t
}

/// The track's grid and phrases as a LOAD sees them: the library's (ours instead per Preferences > Analysis > grid source, or
/// when the library has none), the user's edits on top, phrases derived when there are none. Unlike a LOAD, a library grid
/// stays when we never analysed the track (a stick's copy): nothing here runs the analysis that would fill it.
fn grid_of(reader: Option<&openfader::library::AnalysisReader>, ours: bool, t: &Track) -> Analysis {
    let mut a = reader.and_then(|r| r(t)).unwrap_or_default();
    if t.file.is_some()
        && (ours || a.beats.is_empty())
        && let Some(stored) = cache::load_for(t, &analysis::options::get()).ok().flatten()
    {
        cache::take_ours(&mut a, Some(stored));
    }
    store::apply(
        &mut a,
        &store::global().edits(&store::key(t)).unwrap_or_default(),
    );
    auto::fill(&mut a);
    a
}

/// The menu item: asks when a track has a cue point, else starts. Chosen again while a batch runs, it stops it.
pub fn start(c: &Rc<Ctx>, tracks: Vec<Track>) {
    if let Some(stop) = RUN.with(|r| r.borrow().clone()) {
        stop.store(true, Ordering::Relaxed);
        return;
    }
    if tracks.is_empty() {
        return c.say(tr!("choose a track first"));
    }
    let jobs: Vec<Job> = tracks
        .into_iter()
        .map(|t| {
            let library = c.lib.analysis(&t).map(|a| a.cues).unwrap_or_default();
            (t, library)
        })
        .collect();
    let have: Vec<&Job> = jobs
        .iter()
        .filter(|(t, l)| has_cues(&current(&c.store, &c.store.key_of(t), l)))
        .collect();
    if have.is_empty() {
        return run(c, jobs, Mode::Replace, false);
    }
    if prefs::with(|p| p.analysis.cue_keep) {
        return run(c, jobs, Mode::Keep, true);
    }
    let text = match have.as_slice() {
        [(t, _)] if jobs.len() == 1 => tr!(
            "“{}” already has cues. Replace its cue point and hot cues with the automatic ones?",
            t.title
        ),
        _ => tr!(
            "{} of {} tracks already have cues. Replace their cue point and hot cues with the automatic ones?",
            have.len(),
            jobs.len()
        ),
    };
    c.confirm(
        Confirm::AutoCue(jobs),
        tr!("MAKE AUTO CUE POINT"),
        &text,
        tr!("Replace"),
        tr!("Add missing"),
    );
}

/// Runs the batch. `forced`: Keep because Prohibit overwriting CUEs is on (the summary says so).
pub fn run(c: &Rc<Ctx>, jobs: Vec<Job>, mode: Mode, forced: bool) {
    let db = prefs::with(|p| p.controller.auto_cue_db());
    let ours = prefs::with(|p| p.analysis.grid_source) == GridSource::Ours;
    let (reader, options) = (c.lib.analysis_reader(), analysis::options::get().cue);
    let (store, total) = (c.store.clone(), jobs.len());
    let stop = Arc::new(AtomicBool::new(false));
    RUN.with(|r| *r.borrow_mut() = Some(stop.clone()));
    let flag = stop.clone();
    c.spawn(
        move || {
            let grid = |t: &Track| grid_of(reader.as_ref(), ours, t);
            work(&store, &jobs, mode, db, &grid, &options, &stop, |n| {
                let flag = flag.clone();
                let sent = slint::invoke_from_event_loop(move || {
                    if let Some(c) = CTX.with(|c| c.borrow().clone()) {
                        let msg = tr!("Auto cue {} of {}…", n, total);
                        c.app().global::<BrowserState>().set_status(msg.into());
                    }
                });
                if sent.is_err() {
                    flag.store(true, Ordering::Relaxed); // the window is gone
                }
            })
        },
        move |c, t| finished(c, t, total, db, forced),
    );
}

/// Back on the UI thread: decks and the device link read the new cues, the browser its rows, the status line the summary.
fn finished(c: &Rc<Ctx>, t: Tally, total: usize, db: f32, forced: bool) {
    let stopped = RUN
        .with(|r| r.borrow_mut().take())
        .is_some_and(|s| s.load(Ordering::Relaxed));
    if !t.set.is_empty() {
        for d in c.decks.iter() {
            d.reread_edits(&*c.lib);
        }
        for track in &t.set {
            device::track_edited(track);
        }
        c.reload(None);
    }
    let mut msg = if stopped {
        tr!(
            "Auto cue stopped: cues set on {} of {} tracks",
            t.set.len(),
            total
        )
    } else {
        tr!(
            "Automatic cues set on {} tracks ({} kept, {} without sound above {} dB)",
            t.set.len(),
            t.kept,
            t.silent,
            db
        )
    };
    if t.no_grid > 0 {
        msg = tr!(
            "{}; {} without a beat grid got no hot cues: analyse them first",
            msg,
            t.no_grid
        );
    }
    if t.failed > 0 {
        msg = tr!("{}; {} could not be read", msg, t.failed);
    }
    if forced && t.kept > 0 {
        msg = tr!(
            "{}; existing cues kept: Prohibit overwriting CUEs is on",
            msg
        );
    }
    c.say(&msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn cue(hot_cue: u16, time_ms: u32, loop_end_ms: Option<u32>) -> Cue {
        Cue {
            hot_cue,
            time_ms,
            loop_end_ms,
            color: None,
            comment: None,
        }
    }

    /// What CUE Analysis suggests in these tests: 1.1 as memory cue, hot cues A and B, one more memory cue.
    fn suggested() -> Vec<Cue> {
        vec![
            cue(0, 100, None),
            cue(1, 100, None),
            cue(2, 4000, None),
            cue(0, 16_000, None),
        ]
    }

    #[test]
    fn a_track_without_cues_gets_the_cue_point_on_the_first_sound_and_the_hot_cues() {
        for mode in [Mode::Replace, Mode::Keep] {
            assert_eq!(
                with_auto(&[], Some(1500), &suggested(), mode).unwrap(),
                [
                    cue(0, 1500, None),
                    cue(1, 100, None),
                    cue(2, 4000, None),
                    cue(0, 16_000, None)
                ]
            );
        }
    }

    #[test]
    fn replace_moves_the_cue_point_swaps_the_hot_cues_and_keeps_loops_and_memories() {
        let mut point = cue(0, 2000, None);
        point.comment = Some("drop".into());
        let cues = [
            cue(1, 9, None),
            cue(0, 8000, None),
            point.clone(),
            cue(0, 500, Some(900)),
            cue(5, 7000, None),
        ];
        assert_eq!(cue_point(&cues), Some(2), "a loop is not a cue point");
        assert_eq!(
            with_auto(&cues, Some(1500), &suggested(), Mode::Replace).unwrap(),
            [
                cue(0, 8000, None),
                Cue {
                    time_ms: 1500,
                    ..point
                },
                cue(0, 500, Some(900)),
                cue(1, 100, None),
                cue(2, 4000, None),
                cue(0, 16_000, None)
            ]
        );
    }

    #[test]
    fn keep_only_fills_the_empty_pads_of_a_track_with_a_cue_point() {
        let cues = [cue(0, 2000, None), cue(2, 9, None)];
        let mut want = cues.to_vec();
        want.push(cue(1, 100, None));
        assert_eq!(
            with_auto(&cues, Some(1500), &suggested(), Mode::Keep),
            Some(want.clone())
        );
        assert_eq!(
            with_auto(&want, Some(1500), &suggested(), Mode::Keep),
            None,
            "nothing missing"
        );
    }

    #[test]
    fn without_sound_the_cue_point_is_the_suggested_one_and_without_a_grid_only_the_cue_point() {
        assert_eq!(
            with_auto(&[], None, &suggested(), Mode::Replace).unwrap()[0],
            cue(0, 100, None)
        );
        assert_eq!(
            with_auto(&[], Some(1500), &[], Mode::Replace),
            Some(vec![cue(0, 1500, None)])
        );
        assert_eq!(with_auto(&[], None, &[], Mode::Replace), None);
    }

    /// A mono 16-bit WAV at 8 kHz: `quiet_ms` of silence, then a 440 Hz tone for `tone_ms`.
    fn wav(path: &Path, quiet_ms: u32, tone_ms: u32) {
        const RATE: u32 = 8000;
        let n = |ms: u32| (RATE * ms / 1000) as usize;
        let samples: Vec<i16> = (0..n(quiet_ms) + n(tone_ms))
            .map(|i| match i >= n(quiet_ms) {
                true => {
                    ((i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 16000.0)
                        as i16
                }
                false => 0,
            })
            .collect();
        let data = samples.len() as u32 * 2;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend([1, 0, 1, 0]); // PCM, mono
        b.extend(RATE.to_le_bytes());
        b.extend((RATE * 2).to_le_bytes());
        b.extend([2, 0, 16, 0]);
        b.extend(b"data");
        b.extend(data.to_le_bytes());
        samples.iter().for_each(|s| b.extend(s.to_le_bytes()));
        std::fs::write(path, b).unwrap();
    }

    fn track(file: &Path) -> Track {
        Track {
            title: file.file_stem().unwrap().to_string_lossy().into_owned(),
            file: Some(file.to_owned()),
            ..Default::default()
        }
    }

    /// 120 BPM, and a waveform with an intro, a build, a chorus, a breakdown, a chorus and an outro: CUE Analysis puts hot
    /// cues on the phrase changes.
    fn grid(_: &Track) -> Analysis {
        let sections: [(usize, [u8; 3]); 6] = [
            (8, [20, 30, 20]),
            (8, [20, 70, 50]),
            (16, [110, 100, 80]),
            (8, [10, 60, 50]),
            (16, [105, 100, 80]),
            (8, [70, 60, 50]),
        ];
        let mut a = Analysis {
            beats: (0..256)
                .map(|i| analysis::Beat {
                    bar_pos: i % 4 + 1,
                    bpm_x100: 12000,
                    time_ms: i as u32 * 500,
                })
                .collect(),
            three_band_detail: sections
                .iter()
                .flat_map(|&(bars, h)| vec![h; bars * 300])
                .collect(),
            ..Default::default()
        };
        auto::fill(&mut a);
        a
    }

    #[test]
    fn the_batch_sets_the_cue_point_and_the_hot_cues_and_counts_the_rest() {
        let dir: PathBuf = std::env::temp_dir().join(format!("of-autocue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (lead, mine, quiet, gone) = (
            dir.join("lead.wav"),
            dir.join("mine.wav"),
            dir.join("quiet.wav"),
            dir.join("gone.wav"),
        );
        wav(&lead, 1500, 1000);
        wav(&mine, 700, 1000);
        wav(&quiet, 1000, 0);
        let store = Store::in_memory().unwrap();
        let (tl, tm, tq, tg) = (track(&lead), track(&mine), track(&quiet), track(&gone));
        store
            .set_cues(
                &store.key_of(&tm),
                Some(&[cue(0, 5000, None), cue(2, 9, None)]),
            )
            .unwrap();
        let jobs: Vec<Job> = [&tl, &tm, &tq, &tg]
            .into_iter()
            .map(|t| (t.clone(), Vec::new()))
            .collect();
        let (stop, o) = (AtomicBool::new(false), auto::CueOptions::default());
        let seen = std::cell::Cell::new(0);
        let cues_of = |t: &Track| store.edits(&store.key_of(t)).unwrap().cues.unwrap();
        let hot = |c: &[Cue]| c.iter().filter(|c| c.hot_cue > 0).count();

        let t = work(&store, &jobs, Mode::Keep, -60.0, &grid, &o, &stop, |n| {
            seen.set(n)
        });
        assert_eq!(
            (t.set.len(), t.kept, t.silent, t.failed, seen.get()),
            (3, 0, 1, 1, 4)
        );
        let l = cues_of(&tl);
        assert!(
            (1500..=1510).contains(&l[0].time_ms) && l[0].hot_cue == 0,
            "the cue point is within one window of the tone: {:?}",
            l[0]
        );
        assert!(hot(&l) > 1, "hot cues on the phrase changes: {l:?}");
        let m = cues_of(&tm);
        assert_eq!(
            m[..2],
            [cue(0, 5000, None), cue(2, 9, None)],
            "Keep leaves what is there"
        );
        assert_eq!(hot(&m), hot(&l), "and fills the empty pads");
        assert!(
            hot(&cues_of(&tq)) > 1,
            "a silent track still gets its hot cues"
        );

        let t = work(&store, &jobs, Mode::Keep, -60.0, &grid, &o, &stop, |_| {});
        assert_eq!(
            (t.set.len(), t.kept, t.failed),
            (0, 3, 1),
            "nothing missing any more"
        );

        let t = work(
            &store,
            &jobs,
            Mode::Replace,
            -60.0,
            &grid,
            &o,
            &stop,
            |_| {},
        );
        assert_eq!((t.set.len(), t.kept, t.silent, t.failed), (1, 2, 1, 1));
        let moved = cues_of(&tm);
        assert!((700..=710).contains(&moved[0].time_ms));
        assert_eq!(moved.iter().filter(|c| c.hot_cue == 2).count(), 1);
        assert!(
            !moved.contains(&cue(2, 9, None)),
            "the hot cues are the automatic ones"
        );

        stop.store(true, Ordering::Relaxed);
        let t = work(
            &store,
            &jobs,
            Mode::Replace,
            -60.0,
            &grid,
            &o,
            &stop,
            |_| {},
        );
        assert_eq!(
            (t.set.len(), t.kept, t.silent, t.failed),
            (0, 0, 0, 0),
            "a stopped batch does nothing"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
