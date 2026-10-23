//! The NEURAL key detector: a small convolutional network over the constant-Q spectrogram ([`super::cqt`]), run by ONNX
//! Runtime on the CPU (one thread per session, no GPU).
//!
//! Model: the key CNN of Korzeniowski & Widmer, "Genre-Agnostic Key Classification With Convolutional Neural Networks"
//! (ISMIR 2018), 24 classes, 0.5 M parameters, trained on the GiantSteps-MTG key dataset by github.com/a1ex90/MusicalKeyCNN
//! whose weights (`checkpoints/keynet.pt`, commit 61d8fc1) are published under the MIT licence; `examples/keynet_export.py`
//! turns them into the ONNX file (1.8 MB, [`SHA256`]) that [`fetch`] downloads into the models cache on first use. Training
//! data: Beatport previews with community-corrected keys (so electronic dance music: its accuracy on other genres is
//! lower, see docs/key-detector.md).
//!
//! Interface (observed by running it): input `cqt` `[1, 1, 105, T]` f32 (T frames, T >= 8), output `logits` `[1, 24]`
//! (averaged over the whole input). Class `i` is Camelot number `i % 12 + 1`, minor ("A") below 12 and major ("B") from 12.
//! A track of up to [`CHUNK`] frames goes in at once; a longer one in even chunks whose logits are averaged by frame count.

use super::cqt::{self, Spec, BINS};
use crate::{audio::Frames, models, stems::model::environment};
use ort::{ep, logging::LogLevel, session::Session, value::Tensor};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

pub const URL: &str = "https://github.com/OpenRsG/OpenFadeR/releases/download/models/keynet.onnx";
pub const SIZE: u64 = 1_852_740;
pub const SHA256: &str = "da9b9e947c9434069494302886f2e88e6b5cdd5e8c6acbe5eb97f693bf21fe17";
const FILE: &str = "keynet.onnx";
/// `$OPENFADER_KEY_MODEL` names a model file to use as it is (not size- or hash-checked); `$OPENFADER_KEY_MODEL_URL` another place to download it from
/// (a mirror, a test server): the size and hash still have to match.
const MODEL_ENV: &str = "OPENFADER_KEY_MODEL";
const URL_ENV: &str = "OPENFADER_KEY_MODEL_URL";
/// Frames per network run (5 minutes): bounds the activations (about 20 MB) of an hour-long track.
const CHUNK: usize = 1500;
/// The network pools three times: fewer frames than this cannot go through it.
const MIN_FRAMES: usize = 10;
/// A track whose strongest constant-Q magnitude is below this (about -60 dBFS) is silence.
const SILENCE: f32 = 0.01;

/// Where the model file is (or goes).
pub fn path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(MODEL_ENV).filter(|v| !v.is_empty()) {
        return Ok(p.into());
    }
    Ok(models::dir()
        .ok_or("no cache directory (set XDG_CACHE_HOME or HOME)")?
        .join(FILE))
}

/// The model file is in place: the one `$OPENFADER_KEY_MODEL` names, or the downloaded one with the published size (a
/// cut-off download does not count).
pub fn present() -> bool {
    let custom = std::env::var_os(MODEL_ENV).is_some_and(|v| !v.is_empty());
    path()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| if custom { m.is_file() } else { m.len() == SIZE })
}

/// A run that could not open or run the model: [`ready`] is false until the next start.
static BROKEN: AtomicBool = AtomicBool::new(false);

/// Whether a detection can run now. A model once found stays found (the stat is not repeated per track).
pub fn ready() -> bool {
    static FOUND: AtomicBool = AtomicBool::new(false);
    !BROKEN.load(Relaxed)
        && (FOUND.load(Relaxed)
            || present() && {
                FOUND.store(true, Relaxed);
                true
            })
}

/// The message while the model is missing: what to do by hand.
pub fn missing_hint() -> String {
    format!(
        "NEURAL key model missing: `curl -L --create-dirs -o {} {URL}`",
        path().map_or_else(|e| e, |p| p.display().to_string())
    )
}

/// Bytes of the download in progress; `u64::MAX` while none runs.
static GOT: AtomicU64 = AtomicU64::new(u64::MAX);

/// The bytes downloaded so far while [`fetch`] runs (of [`SIZE`]), from any thread.
pub fn downloading() -> Option<u64> {
    Some(GOT.load(Relaxed)).filter(|&g| g != u64::MAX)
}

/// Downloads the model to [`path`] unless it is there, verifying its size and sha256, reporting 0..=1. Offline or a wrong file
/// is an error naming what to do by hand. One download at a time: a second caller waits for the first and finds the model.
pub fn fetch(progress: &dyn Fn(f32)) -> Result<(), String> {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock();
    if present() {
        return Ok(());
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            GOT.store(u64::MAX, Relaxed);
        }
    }
    GOT.store(0, Relaxed);
    let _done = Done;
    let (dest, url) = (
        path()?,
        std::env::var(URL_ENV).unwrap_or_else(|_| URL.into()),
    );
    models::download(&url, &dest, SIZE, Some(SHA256), &|got| {
        GOT.store(got, Relaxed);
        progress((got as f32 / SIZE as f32).min(1.0));
    })
    .map_err(|e| {
        format!(
            "NEURAL key model download failed ({e}); fetch it by hand: curl -L --create-dirs -o {} {url}",
            dest.display()
        )
    })
}

/// Idle sessions: a detection takes one (opening it when none is idle) and gives it back, so concurrent analyses never
/// wait for each other and the sessions never outnumber the threads that ran at once.
static IDLE: Mutex<Vec<Session>> = Mutex::new(Vec::new());

fn open() -> Result<Session, String> {
    let p = path()?;
    let cpu = ep::CPU::default().with_arena_allocator(false).build();
    let mut b = Session::builder(environment()?)
        .map_err(|e| e.to_string())?
        .with_log_level(LogLevel::Error)
        .map_err(|e| e.to_string())?
        .with_intra_threads(1)
        .map_err(|e| e.to_string())?
        .with_inter_threads(1)
        .map_err(|e| e.to_string())?
        .with_execution_providers(vec![cpu])
        .map_err(|e| e.to_string())?;
    crate::paths::at(b.commit_from_file(&p), &p)
}

/// The summed logits of frames `from..to` of `spec`.
fn run(session: &mut Session, spec: &Spec, from: usize, to: usize) -> Result<[f32; 24], String> {
    let mut input = Vec::with_capacity(BINS * (to - from));
    for bin in 0..BINS {
        input.extend_from_slice(&spec.data[bin * spec.frames + from..bin * spec.frames + to]);
    }
    let input =
        Tensor::from_array(([1usize, 1, BINS, to - from], input)).map_err(|e| e.to_string())?;
    let out = session
        .run(ort::inputs!["cqt" => input])
        .map_err(|e| e.to_string())?;
    let (shape, data) = out["logits"]
        .try_extract_tensor::<f32>()
        .map_err(|e| e.to_string())?;
    let logits: [f32; 24] = data
        .try_into()
        .map_err(|_| format!("unexpected model output shape {shape:?}"))?;
    Ok(logits)
}

/// Camelot name of class `i`.
fn camelot(i: usize) -> String {
    format!("{}{}", i % 12 + 1, if i < 12 { 'A' } else { 'B' })
}

/// The key of a spectrogram: `Ok(None)` for silence or fewer than [`MIN_FRAMES`] frames (2 s). `None` when `cancelled`.
fn classify(spec: &Spec, cancelled: &dyn Fn() -> bool) -> Option<Result<Option<String>, String>> {
    if spec.frames < MIN_FRAMES || spec.peak < SILENCE {
        return Some(Ok(None));
    }
    let taken = IDLE.lock().pop();
    let mut session = match taken.map_or_else(open, Ok) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let chunks = spec.frames.div_ceil(CHUNK);
    let mut sum = [0.0f32; 24];
    for i in 0..chunks {
        if cancelled() {
            IDLE.lock().push(session);
            return None;
        }
        let (from, to) = (i * spec.frames / chunks, (i + 1) * spec.frames / chunks);
        match run(&mut session, spec, from, to) {
            Ok(l) => sum
                .iter_mut()
                .zip(l)
                .for_each(|(s, l)| *s += l * (to - from) as f32),
            Err(e) => return Some(Err(e)),
        }
    }
    IDLE.lock().push(session);
    let best = (0..24).max_by(|&a, &b| sum[a].total_cmp(&sum[b]))?;
    Some(Ok(Some(camelot(best))))
}

/// The key of stereo audio by the network; `None` when `cancelled`, `Err` when the model cannot be opened or run (the
/// caller falls back to the classic detector, and [`ready`] is false from then on).
pub(crate) fn detect_cancellable<S: Frames + ?Sized>(
    samples: &S,
    rate: u32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<Option<String>, String>> {
    let spec = cqt::log_cqt(samples, rate, cancelled)?;
    let r = classify(&spec, cancelled)?;
    if r.is_err() {
        BROKEN.store(true, Relaxed);
    }
    Some(r)
}
