//! Track menu > Make Auto Cue point: the selected tracks get a memory cue at the first sound at or above the Auto Cue Level
//! (Preferences > Controller > Deck). A track's "cue point" is its earliest memory cue that is not a loop; a track that has
//! one is asked about first (Replace moves it, Keep existing leaves it) unless Preferences > Analysis > Prohibit overwriting
//! CUEs is on (then Keep, without asking). Hot cues, loops and the other memory cues stay.
//!
//! One track at a time on a thread (decode, find the level, drop the PCM), progress in the status line; choosing the item
//! again while it runs stops it. The cues are stored as the user's own edit, in one undo step.

use super::{prefs, Confirm, Ctx, CTX};
use crate::app::{deck::first_sound, device, BrowserState};
use openfader::{analysis::Cue, audio::decode_cancellable, library::Track, store::Store};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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
    /// Tracks that got a cue point.
    set: Vec<Track>,
    kept: usize,
    silent: usize,
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

/// `cues` with the cue point at `at_ms`; `None` when the track keeps the one it has ([`Mode::Keep`]).
fn with_auto(cues: &[Cue], at_ms: u32, mode: Mode) -> Option<Vec<Cue>> {
    let mut out = cues.to_vec();
    match (cue_point(cues), mode) {
        (Some(_), Mode::Keep) => return None,
        (Some(i), Mode::Replace) => out[i].time_ms = at_ms,
        (None, _) => out.push(Cue {
            hot_cue: 0,
            time_ms: at_ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }),
    }
    Some(out)
}

/// What the track shows: the user's stored list, else the library's.
fn current(store: &Store, key: &str, library: &[Cue]) -> Vec<Cue> {
    store
        .edits(key)
        .ok()
        .and_then(|e| e.cues)
        .unwrap_or_else(|| library.to_vec())
}

/// The batch: `progress(n)` before the n-th track; `stop` ends it between tracks (and inside a decode).
fn work(
    store: &Store,
    jobs: &[Job],
    mode: Mode,
    db: f32,
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
            if mode == Mode::Keep && cue_point(&cues).is_some() {
                t.kept += 1;
                continue;
            }
            let Some(file) = &track.file else {
                t.failed += 1;
                continue;
            };
            let found = decode_cancellable(file, &|| stop.load(Ordering::Relaxed))
                .map(|pcm| first_sound(&pcm.samples, pcm.rate, db));
            match found {
                Ok(Some(at)) => {
                    let new = with_auto(&cues, at, mode).unwrap_or(cues);
                    match store.set_cues(&key, Some(&new)) {
                        Ok(()) => t.set.push(track.clone()),
                        Err(_) => t.failed += 1,
                    }
                }
                Ok(None) => t.silent += 1,
                Err(_) if stop.load(Ordering::Relaxed) => {}
                Err(_) => t.failed += 1,
            }
        }
    });
    t
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
        .filter(|(t, l)| cue_point(&current(&c.store, &c.store.key_of(t), l)).is_some())
        .collect();
    if have.is_empty() {
        return run(c, jobs, Mode::Replace, false);
    }
    if prefs::with(|p| p.analysis.cue_keep) {
        return run(c, jobs, Mode::Keep, true);
    }
    let text = match have.as_slice() {
        [(t, _)] if jobs.len() == 1 => tr!(
            "“{}” already has a cue point. Replace it with the auto cue point?",
            t.title
        ),
        _ => tr!(
            "{} of {} tracks already have a cue point. Replace it with the auto cue point?",
            have.len(),
            jobs.len()
        ),
    };
    c.confirm(
        Confirm::AutoCue(jobs),
        tr!("MAKE AUTO CUE POINT"),
        &text,
        tr!("Replace"),
        tr!("Keep existing"),
    );
}

/// Runs the batch. `forced`: Keep because Prohibit overwriting CUEs is on (the summary says so).
pub fn run(c: &Rc<Ctx>, jobs: Vec<Job>, mode: Mode, forced: bool) {
    let db = prefs::with(|p| p.controller.auto_cue_db());
    let (store, total) = (c.store.clone(), jobs.len());
    let stop = Arc::new(AtomicBool::new(false));
    RUN.with(|r| *r.borrow_mut() = Some(stop.clone()));
    let flag = stop.clone();
    c.spawn(
        move || {
            work(&store, &jobs, mode, db, &stop, |n| {
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
            "Auto cue stopped: cue point set on {} of {} tracks",
            t.set.len(),
            total
        )
    } else {
        tr!(
            "Auto cue point set on {} tracks ({} kept, {} without sound above {} dB)",
            t.set.len(),
            t.kept,
            t.silent,
            db
        )
    };
    if t.failed > 0 {
        msg = tr!("{}; {} could not be read", msg, t.failed);
    }
    if forced && t.kept > 0 {
        msg = tr!(
            "{}; existing cue points kept: Prohibit overwriting CUEs is on",
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

    #[test]
    fn no_cue_point_gets_a_new_memory_cue() {
        let hot = [cue(1, 500, None), cue(0, 9000, Some(10_000))];
        for mode in [Mode::Replace, Mode::Keep] {
            let out = with_auto(&hot, 1500, mode).unwrap();
            assert_eq!(out[..2], hot, "hot cues and loops stay");
            assert_eq!(out[2], cue(0, 1500, None));
        }
        assert_eq!(with_auto(&[], 40, Mode::Keep), Some(vec![cue(0, 40, None)]));
    }

    #[test]
    fn replace_moves_the_earliest_memory_cue_and_nothing_else() {
        let cues = [
            cue(1, 100, None),
            cue(0, 8000, None),
            cue(0, 2000, None),
            cue(0, 500, Some(900)),
        ];
        let mut want = cues.to_vec();
        want[2].time_ms = 1500;
        assert_eq!(with_auto(&cues, 1500, Mode::Replace), Some(want));
        assert_eq!(cue_point(&cues), Some(2), "a loop is not a cue point");
    }

    #[test]
    fn replace_keeps_the_color_and_comment_of_the_cue_point() {
        let mut c = cue(0, 2000, None);
        c.comment = Some("drop".into());
        let out = with_auto(&[c.clone()], 300, Mode::Replace).unwrap();
        assert_eq!(out, [Cue { time_ms: 300, ..c }]);
    }

    #[test]
    fn keep_leaves_a_track_with_a_cue_point_alone() {
        assert_eq!(with_auto(&[cue(0, 2000, None)], 1500, Mode::Keep), None);
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

    #[test]
    fn the_batch_puts_the_cue_point_on_the_first_sound_and_counts_the_rest() {
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
        let stop = AtomicBool::new(false);
        let seen = std::cell::Cell::new(0);

        let t = work(&store, &jobs, Mode::Keep, -60.0, &stop, |n| seen.set(n));
        assert_eq!((t.kept, t.silent, t.failed, seen.get()), (1, 1, 1, 4));
        assert_eq!(t.set.len(), 1);
        let cues = store.edits(&store.key_of(&tl)).unwrap().cues.unwrap();
        assert_eq!(cues.len(), 1);
        assert!(
            (1500..=1510).contains(&cues[0].time_ms),
            "within one window of the tone: {}",
            cues[0].time_ms
        );
        let kept = store.edits(&store.key_of(&tm)).unwrap().cues.unwrap();
        assert_eq!(kept, [cue(0, 5000, None), cue(2, 9, None)]);

        let t = work(&store, &jobs, Mode::Replace, -60.0, &stop, |_| {});
        assert_eq!((t.set.len(), t.kept, t.silent, t.failed), (2, 0, 1, 1));
        let moved = store.edits(&store.key_of(&tm)).unwrap().cues.unwrap();
        assert!((700..=710).contains(&moved[0].time_ms));
        assert_eq!(moved[1], cue(2, 9, None), "the hot cue stays");

        stop.store(true, Ordering::Relaxed);
        let t = work(&store, &jobs, Mode::Replace, -60.0, &stop, |_| {});
        assert_eq!(
            (t.set.len(), t.kept, t.silent, t.failed),
            (0, 0, 0, 0),
            "a stopped batch does nothing"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
