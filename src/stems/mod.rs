//! STEMS: separating a loaded track into VOCAL / INST / BASS / DRUMS (a 4-stem set) in the background, while it is
//! played.
//!
//! Base: [`Job`], the background job a deck starts per loaded track: its [`Stems`] are allocated on the worker and fill
//! segment by segment (frames not separated yet play as the original track), from the cache if the track was separated
//! before, else by the GPU and CPU side by side, then cached. [`Progress`] is what it reports and
//! [`Quality`] (STEMS Analysis Process: sound quality or speed) what it separates at. Children: [`model`] (the open
//! Demucs v4 `htdemucs` ONNX model, its download, the GPU and CPU [`Engine`](model::Engine)s), [`schedule`] (the order of the
//! segments), [`separate`] (one engine's loop over segments), [`cache`] (on-disk results keyed like the analysis cache, one
//! entry per quality). The result type, [`crate::audio::Stems`], lives with the audio code that plays it.
//!
//! The model is open; the separation is done entirely by the model named above. A stem file (`.stem.mp4`, see
//! [`crate::audio::stemfile`]) needs none of it: [`Job::from_stem_file`] delivers its own four parts as the stems.

pub mod cache;
pub mod model;
pub mod schedule;
pub mod separate;

use crate::audio::{Grid, Stems, Track16, stemfile};
use parking_lot::Mutex;
use schedule::Schedule;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// STEMS Analysis Process (PREFS: "Prioritize sound quality" / "Prioritize speed"):
/// - QUALITY separates in order from the start of the track, with segments overlapping by a quarter (1.95 s cross-fade, as the
///   inference does). A jump ahead of what is done plays the original until the separation gets there.
/// - SPEED separates the play position and the HOT CUEs first (see [`schedule`]), overlapping by a 32nd (0.24 s): fewer
///   segments, and a jump finds stems right away.
///
/// Cache entries are per quality: a QUALITY entry also answers a SPEED request, never the other way round.
///
/// Measured 2026-10-13 (four 60 s mid-track excerpts, 16 cores, 8 ONNX Runtime threads, CPU only, other work running on the
/// machine): SPEED runs 8 of QUALITY's 10 segments, 10.2 to 10.5 s per audio minute against 13.0 to 15.6 (about -24 %). Σ stems
/// against the track: −24.7…−26.8 dB at both, within 0.2 dB of each other: the overlap does not move this error. SPEED stems
/// against QUALITY stems (energy relative to QUALITY's): VOCAL −20…−22 dB, INST −15…−19, BASS −26…−32, DRUMS −27…−32, which
/// is the model's own dependence on where its segments start (QUALITY on the track delayed by 0.3 s differs from QUALITY by
/// the same amount). With no overlap the step between two samples at a join is 21 to 27 dB above the track's average (a click);
/// at 1/32 it is +0.2…+6.4 dB, as at QUALITY's joins. Demucs' "shifts" trick doubles the time and moves the error by at most
/// 0.2 dB: not adopted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// Prioritize sound quality.
    Quality,
    /// Prioritize speed.
    Speed,
}

impl Quality {
    /// Overlap between consecutive segments, in frames at [`model::RATE`].
    pub fn overlap(self) -> usize {
        match self {
            Quality::Quality => model::SEGMENT / 4,
            Quality::Speed => model::SEGMENT / 32,
        }
    }

    /// Separated in order from the start of the track (else from the play position, see [`schedule`]).
    pub fn sequential(self) -> bool {
        self == Quality::Quality
    }
}

/// What a [`Job`] is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Loading the cache or the model.
    Opening,
    /// Downloading the model (first use only).
    Download,
    Separate,
}

/// Shared between the worker and the UI.
pub struct Progress {
    phase: AtomicU8,
    permille: AtomicU32,
}

impl Progress {
    fn set(&self, phase: Phase, frac: f32) {
        self.phase.store(phase as u8, Relaxed);
        self.permille
            .store((frac.clamp(0.0, 1.0) * 1000.0) as u32, Relaxed);
    }

    pub fn get(&self) -> (Phase, f32) {
        let p = [Phase::Opening, Phase::Download, Phase::Separate]
            [self.phase.load(Relaxed) as usize % 3];
        (p, self.permille.load(Relaxed) as f32 / 1000.0)
    }
}

/// A separation in the background. Dropping it cancels (between segments); the stems it filled stay with whoever holds them.
pub struct Job {
    pub progress: Arc<Progress>,
    stems: Arc<OnceLock<Arc<Stems>>>,
    ctl: Arc<Control>,
    rx: Receiver<Result<(), String>>,
}

/// What the UI side steers the worker with.
struct Control {
    sched: Mutex<Schedule>,
    cancel: AtomicBool,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.ctl.cancel.store(true, Relaxed);
    }
}

/// ONNX Runtime threads of the CPU engine: half the cores (the UI, the audio thread and analysis keep the rest), at least 2;
/// one when `multithread` is off (PREFS > Extensions > STEMS > Multi-thread). The graph does not scale past a few threads
/// (measured: 8, 12 and 16 threads run a segment in the same time), so more would only take cores from the rest of the app.
pub fn threads(multithread: bool) -> usize {
    if !multithread {
        return 1;
    }
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(2))
}

/// Opens the engines in the background (about 2 s each, the GPU's shaders another second) so the first track after STEMS is
/// switched on finds them ready. Does nothing without the model; a missing model is reported by the first [`Job`].
pub fn warm(multithread: bool) {
    if model::usable() {
        model::warm(&model::engines(multithread));
    }
}

impl Job {
    /// Stems of `audio` (decoded as `pcm`) at `quality`: from the cache, else separated (downloading the model first if it
    /// is missing) and cached. A failed cache write is dropped: the stems are still delivered. `multithread` off = one
    /// ONNX Runtime thread ([`threads`]).
    pub fn start(audio: PathBuf, pcm: Arc<Track16>, quality: Quality, multithread: bool) -> Job {
        let progress = Arc::new(Progress {
            phase: AtomicU8::new(Phase::Opening as u8),
            permille: AtomicU32::new(0),
        });
        let (frames, grid) = (pcm.frames(), separate::grid(pcm.rate, quality));
        let stems = Arc::new(OnceLock::new());
        let ctl = Arc::new(Control {
            sched: Mutex::new(Schedule::new(
                grid,
                grid.count(frames),
                quality.sequential(),
            )),
            cancel: AtomicBool::new(false),
        });
        let (tx, rx) = channel();
        let (p, s, c) = (progress.clone(), stems.clone(), ctl.clone());
        std::thread::spawn(move || {
            if c.cancel.load(Relaxed) {
                return;
            }
            let allocated = Arc::new(Stems::new(frames, grid));
            if c.cancel.load(Relaxed) {
                return;
            }
            let _ = s.set(allocated);
            let s = s.get().unwrap();
            let result = work(&audio, &pcm, quality, multithread, s, &c, &p).map(|fresh| {
                // Stored before the outcome is announced: whoever starts the same track next finds it.
                if fresh && let Err(e) = cache::store(&audio, pcm.rate, s, quality) {
                    eprintln!("stems cache: {e}");
                }
            });
            let _ = tx.send(result);
            model::trim_heap(); // the segments' buffers, spread over the engine threads' heaps
        });
        Job {
            progress,
            stems,
            ctl,
            rx,
        }
    }

    /// The stems of a stem file: its four parts decoded ([`stemfile::decode_parts`]) as the stems of `pcm`, its mix. No model,
    /// no cache entry; progress is the share of the file read. The stems are delivered whole, so the job is done once
    /// they are there.
    pub fn from_stem_file(audio: PathBuf, pcm: Arc<Track16>, info: stemfile::Info) -> Job {
        let progress = Arc::new(Progress {
            phase: AtomicU8::new(Phase::Separate as u8),
            permille: AtomicU32::new(0),
        });
        let frames = pcm.frames();
        let stems = Arc::new(OnceLock::new());
        let ctl = Arc::new(Control {
            sched: Mutex::new(Schedule::new(
                Grid {
                    len: frames.max(1),
                    overlap: 0,
                },
                1,
                true,
            )),
            cancel: AtomicBool::new(false),
        });
        let (tx, rx) = channel();
        let (p, s, c) = (progress.clone(), stems.clone(), ctl.clone());
        std::thread::spawn(move || {
            let cancelled = || c.cancel.load(Relaxed);
            let result = stemfile::decode_parts(&audio, &info, frames, &cancelled, &|f| {
                p.set(Phase::Separate, f)
            })
            .map(|parts| {
                let _ = s.set(Arc::new(Stems::complete(parts)));
            });
            let _ = tx.send(result);
        });
        Job {
            progress,
            stems,
            ctl,
            rx,
        }
    }

    /// The stems once the worker has allocated them; attach them while separation continues.
    pub fn stems(&self) -> Option<&Arc<Stems>> {
        self.stems.get()
    }

    /// Where the listener is (`play`) and the HOT CUEs, in frames: SPEED separates those first ([`Schedule::focus`]).
    pub fn focus(&self, play: usize, cues: &[usize]) {
        self.ctl.sched.lock().focus(play, cues);
    }

    /// The outcome once the stems are complete (or the separation failed), once; `None` while running.
    pub fn poll(&self) -> Option<Result<(), String>> {
        self.rx.try_recv().ok()
    }
}

/// The job's thread: the cache, else the model download, then one thread per [`Engine`] working through the schedule.
/// `Ok(true)` = separated now (to be cached), `Ok(false)` = came from the cache.
fn work(
    audio: &std::path::Path,
    pcm: &Track16,
    quality: Quality,
    multithread: bool,
    stems: &Stems,
    ctl: &Control,
    p: &Progress,
) -> Result<bool, String> {
    if ctl.cancel.load(Relaxed) {
        return Err("cancelled".into());
    }
    match cache::load(audio, pcm.rate, stems, quality) {
        Ok(true) => return Ok(false),
        Ok(false) => {}
        Err(e) => eprintln!("stems cache: {e} (recomputing)"),
    }
    if ctl.cancel.load(Relaxed) {
        return Err("cancelled".into());
    }
    if !model::usable() {
        p.set(Phase::Download, 0.0);
        model::fetch(&|f| p.set(Phase::Download, f))?;
    }
    p.set(Phase::Separate, 0.0);
    let (t0, n) = (Instant::now(), stems.segments());
    let (finished, per_engine) = (
        AtomicUsize::new(0),
        [AtomicUsize::new(0), AtomicUsize::new(0)],
    );
    let errors = Mutex::new(Vec::<String>::new());
    let engines = model::engines(multithread);
    std::thread::scope(|sc| {
        for (i, &engine) in engines.iter().enumerate() {
            let (finished, per_engine, errors) = (&finished, &per_engine, &errors);
            sc.spawn(move || {
                let done = |_: usize| {
                    per_engine[i].fetch_add(1, Relaxed);
                    p.set(
                        Phase::Separate,
                        (finished.fetch_add(1, Relaxed) + 1) as f32 / n as f32,
                    );
                };
                let result = engine.open_cancellable(&ctl.cancel).and_then(|()| {
                    separate::work(
                        pcm,
                        stems,
                        &ctl.sched,
                        &mut |mix, consume| engine.run(mix, &ctl.cancel, consume),
                        &ctl.cancel,
                        &done,
                    )
                });
                if let Err(e) = result {
                    errors.lock().push(e);
                }
            });
        }
    });
    if ctl.cancel.load(Relaxed) {
        return Err("cancelled".into());
    }
    if !stems.is_complete() {
        // Every engine failed: the CPU's reason (a missing model, say) is the one worth showing.
        return Err(errors
            .into_inner()
            .pop()
            .unwrap_or_else(|| "separation incomplete".into()));
    }
    eprintln!(
        "stems: {n} segments in {:.1} s ({} {}, {} {})",
        t0.elapsed().as_secs_f32(),
        engines[0].label(),
        per_engine[0].load(Relaxed),
        engines[1].label(),
        per_engine[1].load(Relaxed)
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multithread_off_is_one_thread_on_is_at_least_two() {
        assert_eq!(threads(false), 1);
        assert!(threads(true) >= 2);
    }

    #[test]
    fn a_stem_file_job_delivers_the_parts_complete_without_a_model_or_cache() {
        let path = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/four-parts.stem.mp4"
        ));
        let info = stemfile::read_info(&path).unwrap();
        let (mix, _) = crate::audio::decode_track16_cancellable(&path, &|| false).unwrap();
        let frames = mix.frames();
        let job = Job::from_stem_file(path.clone(), Arc::new(mix), info);
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        let outcome = loop {
            if let Some(r) = job.poll() {
                break r;
            }
            assert!(Instant::now() < deadline, "the job never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(outcome, Ok(()));
        let stems = job.stems().expect("stems delivered");
        assert!(stems.is_complete() && stems.frames() == frames);
        assert!(!cache::has(&path, Quality::Quality), "nothing was cached");
    }
}
