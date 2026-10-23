//! The separation model: Demucs v4 `htdemucs` (Meta, MIT licence), in the ONNX export published by StemSplit
//! (`StemSplitio/htdemucs-onnx`, MIT) with the STFT/iSTFT inside the graph. Run by ONNX Runtime (`ort`) on the CPU and, when
//! there is a Vulkan GPU, on the GPU too ([`Engine`]).
//!
//! The weights are never in this repository: [`path`] is `$OPENFADER_STEMS_MODEL`, else
//! `$XDG_CACHE_HOME/openfader/models/htdemucs.onnx`, and [`fetch`] downloads it there (316 MB). By hand:
//! `curl -L --create-dirs -o ~/.cache/openfader/models/htdemucs.onnx <URL>` (sha256 in [`SHA256`]).
//!
//! Interface (observed from the model card and by running it): input `mix` `[1, 2, 343980]` f32 (stereo, 44.1 kHz,
//! 7.8 s), output `stems` `[1, 4, 2, 343980]` in the order [`ORDER`]. Fixed segment length: longer audio is chunked
//! by [`super::separate`].

use crate::analysis::cache;
use ort::{environment::Environment, ep, logging::LogLevel, session::Session, value::Tensor};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

pub const URL: &str = "https://huggingface.co/StemSplitio/htdemucs-onnx/resolve/main/htdemucs.onnx";
pub const SIZE: u64 = 316_446_953;
pub const SHA256: &str = "68d0bf16428ef66e692cdff8a9ccf28f1ef3f69440d57e58605a4cc55fcc5e74";
pub const RATE: u32 = 44_100;
pub const SEGMENT: usize = 343_980;
const MODEL_ENV: &str = "OPENFADER_STEMS_MODEL";
/// Our stem (`audio::VOCAL`…) of each model output, in the model's order: drums, bass, other, vocals.
pub const ORDER: [usize; 4] = [
    crate::audio::DRUMS,
    crate::audio::BASS,
    crate::audio::INST,
    crate::audio::VOCAL,
];

/// Where the model file is (or goes).
pub fn path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(MODEL_ENV).filter(|v| !v.is_empty()) {
        return Ok(p.into());
    }
    Ok(cache::root()
        .ok_or("no cache directory (set XDG_CACHE_HOME or HOME)")?
        .join("models")
        .join("htdemucs.onnx"))
}

/// The model file exists with the published size (a cut-off download does not count).
pub fn present() -> bool {
    path()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| m.len() == SIZE)
}

/// A separation can start without a download: the model is in place, or `$OPENFADER_STEMS_MODEL` names one (whose size is not
/// checked).
pub fn usable() -> bool {
    present() || std::env::var_os(MODEL_ENV).is_some()
}

/// The message the UI shows while the model is missing.
pub fn missing_hint() -> String {
    format!(
        "STEMS model missing: turn STEMS on to download it ({} MB), or `curl -L --create-dirs -o {} {URL}`",
        SIZE / 1_000_000,
        path().map_or_else(|e| e, |p| p.display().to_string())
    )
}

/// Bytes of the download in progress; `u64::MAX` while none runs.
static GOT: AtomicU64 = AtomicU64::new(u64::MAX);

/// The bytes downloaded so far while [`fetch`] runs (of [`SIZE`]), from any thread.
pub fn downloading() -> Option<u64> {
    Some(GOT.load(Relaxed)).filter(|&g| g != u64::MAX)
}

/// Downloads the model to [`path`] (via a `.part` file, so a failure never leaves a truncated model), reporting 0..=1.
/// Offline or a short download is an error naming what to do by hand. One download at a time: a second caller waits
/// for the first and finds the model in place.
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
    let dest = path()?;
    let hand = |e: String| {
        format!(
            "STEMS model download failed ({e}); fetch it by hand: curl -L --create-dirs -o {} {URL}",
            dest.display()
        )
    };
    crate::models::download(URL, &dest, SIZE, None, &|got| {
        GOT.store(got, Relaxed);
        progress((got as f32 / SIZE as f32).min(1.0));
    })
    .map_err(hand)
}

/// ONNX Runtime allows one environment per process. Its log goes through [`ort_log`].
static ENV: LazyLock<Result<Environment, String>> = LazyLock::new(|| {
    ort::init()
        .with_name("openfader-stems")
        .with_logger(std::sync::Arc::new(ort_log))
        .build()
        .map_err(|e| e.to_string())
});

/// Prints ONNX Runtime's first error only. A GPU out of video memory logs one error per buffer it fails to allocate (hundreds
/// per segment); the first names the cause, and the failed run reaches [`Engine::run`], which logs the fallback to the CPU.
fn ort_log(level: LogLevel, _category: &str, _id: &str, at: &str, msg: &str) {
    static SHOWN: AtomicBool = AtomicBool::new(false);
    if level >= LogLevel::Error && !SHOWN.swap(true, Relaxed) {
        eprintln!(
            "stems: ONNX Runtime: {} ({at}); its further errors are not shown",
            msg.trim()
        );
    }
}

pub(crate) fn environment() -> Result<&'static Environment, String> {
    ENV.as_ref().map_err(Clone::clone)
}

/// An open model: one ONNX Runtime session.
pub struct Model(Session);

/// Free memory the one-off fold needs (it peaks at about 6.5 GB of RSS).
const FOLD_MIN_KIB: u64 = 8 << 20;

/// `MemAvailable` in KiB from the text of `/proc/meminfo`.
fn mem_available_kib(meminfo: &str) -> Option<u64> {
    let l = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?;
    l.split_whitespace().next()?.parse().ok()
}

/// Refuses the fold when the machine lacks the memory for its peak (Linux; elsewhere, or if unreadable, no guard).
fn fold_memory_ok() -> Result<(), String> {
    let free = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| mem_available_kib(&t));
    match free {
        Some(k) if k < FOLD_MIN_KIB => Err(format!(
            "not enough free memory to fold the model ({} MiB available, {} MiB needed)",
            k >> 10,
            FOLD_MIN_KIB >> 10
        )),
        _ => Ok(()),
    }
}

/// The model with its constant subgraphs folded (ONNX Runtime's basic optimisations, run once on the CPU and saved as
/// `<model>.folded.onnx` in the cache, 345 MB), rebuilt when the model is newer. Folding inside every session open costs
/// 6.5 GB of RSS at the peak and keeps 1.7 GB; the saved result opens in 0.6 GB. Building it peaks at about 6 GB once.
fn folded() -> Result<PathBuf, String> {
    static FOLDING: Mutex<()> = Mutex::new(());
    let src = path()?;
    let dst = cache::root()
        .ok_or("no cache directory")?
        .join("models")
        .join(
            src.with_extension("folded.onnx")
                .file_name()
                .ok_or("model path has no file name")?,
        );
    let _one = FOLDING.lock();
    let modified = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if modified(&dst).is_some_and(|d| modified(&src).is_none_or(|s| d >= s)) {
        return Ok(dst);
    }
    fold_memory_ok()?;
    std::fs::create_dir_all(dst.parent().ok_or("no models folder")?).map_err(|e| e.to_string())?;
    let part = dst.with_extension("part");
    let session = Session::builder(environment()?)
        .map_err(|e| e.to_string())?
        .with_log_level(LogLevel::Error)
        .map_err(|e| e.to_string())?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level1)
        .map_err(|e| e.to_string())?
        .with_execution_providers([ep::CPU::default().with_arena_allocator(false).build()])
        .map_err(|e| e.to_string())?
        .with_optimized_model_path(&part)
        .map_err(|e| e.to_string())?
        .commit_from_file(&src);
    let session = crate::paths::at(session, &src);
    if session.is_err() {
        let _ = std::fs::remove_file(&part); // a failed fold may leave a truncated file
    }
    drop(session?);
    trim_heap(); // the folding's transient gigabytes go back to the system
    std::fs::rename(&part, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// Hands freed heap pages back to the system (glibc keeps them otherwise).
pub(crate) fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: glibc's malloc_trim only releases free memory; any argument is valid.
        unsafe { malloc_trim(0) };
    }
}

impl Model {
    /// Memory (measured 2026-10-15, `htdemucs`, after three segments): ORT's defaults take 5.5 GB RSS on the CPU (constant
    /// folding expands the in-graph STFT into 2.2 GB of constants, the CPU arena keeps every peak) and 1.7 GB on the GPU
    /// (6.5 GB at open). Now: no constant folding at open and no CPU arena; the GPU reads the model folded once
    /// ([`folded`]): 0.6 GB, 0.50 s per segment (unfolded it slows to 0.8 s while the CPU engine runs). The CPU reads the
    /// original: 0.8 GB, 1.2 to 1.3 s per segment (folded it peaks at 2.7 GB for the same speed).
    fn open(threads: usize, gpu: bool) -> Result<Model, String> {
        let p = match gpu.then(folded) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                eprintln!("stems: folded model unavailable ({e}); the GPU reads the original");
                path()?
            }
            None => path()?,
        };
        let cpu = ep::CPU::default().with_arena_allocator(false).build();
        let providers = if gpu {
            #[cfg(feature = "stems-gpu")]
            let providers = vec![ep::WebGPU::default().build().error_on_failure(), cpu];
            #[cfg(not(feature = "stems-gpu"))]
            let providers = vec![cpu];
            providers
        } else {
            vec![cpu]
        };
        let mut b = Session::builder(environment()?)
            .map_err(|e| e.to_string())?
            .with_log_level(LogLevel::Error)
            .map_err(|e| e.to_string())?
            .with_intra_threads(threads)
            .map_err(|e| e.to_string())?
            .with_inter_threads(1)
            .map_err(|e| e.to_string())?
            // Idle intra-op threads spin by default; two engines share the machine, so let them sleep.
            .with_intra_op_spinning(false)
            .map_err(|e| e.to_string())?
            .with_config_entry(
                "optimization.disable_specified_optimizers",
                "ConstantFolding",
            )
            .map_err(|e| e.to_string())?
            .with_execution_providers(providers)
            .map_err(|e| e.to_string())?;
        let model = crate::paths::at(b.commit_from_file(&p).map(Model), &p);
        trim_heap(); // what the graph transformations allocated and freed
        model
    }

    /// Consumes `[model stem][channel][SEGMENT]` while the ORT output is alive.
    fn run(
        &mut self,
        mix: Vec<f32>,
        consume: &mut dyn FnMut(&[f32]) -> Result<(), String>,
    ) -> Result<(), String> {
        let input = Tensor::from_array(([1usize, 2, SEGMENT], mix)).map_err(|e| e.to_string())?;
        let out = self
            .0
            .run(ort::inputs!["mix" => input])
            .map_err(|e| e.to_string())?;
        let (shape, data) = out["stems"]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        if shape.iter().copied().ne([1, 4, 2, SEGMENT as i64]) {
            return Err(format!("unexpected model output shape {shape:?}"));
        }
        consume(data)
    }
}

/// Where a segment runs. The GPU is ONNX Runtime's WebGPU provider (Dawn over Vulkan: any GPU with a Vulkan driver, no CUDA or
/// cuDNN); the CPU is the default provider. Both sessions live for the whole process and are shared by every deck's separation:
/// opening one costs about 2 s, and a GPU's first run compiles its shaders (about 1 s more).
///
/// Measured 2026-10-13 on a Ryzen/RTX 3090 box under load (`examples/stems_bench.rs`, one 7.8 s segment): CPU 1.15 to 1.45 s
/// whatever the thread count (8, 12 or 16; the graph does not scale), WebGPU 0.53 to 0.66 s. The two run side by side, so a
/// separation uses both and takes each next segment as soon as an engine is free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Gpu,
    /// ONNX Runtime intra-op threads.
    Cpu(usize),
}

/// A lazily opened session of an engine that may turn out unusable: the reason is remembered and logged once, and the engine
/// is never tried again.
enum Slot<T> {
    Untried,
    Ready(T),
    Unusable(String),
}

impl<T> Slot<T> {
    /// The session, opened by `open` on the first call (not at all when `allowed` is `Err(why)`); `log` hears the reason once
    /// if the engine is out.
    fn ensure(
        &mut self,
        allowed: Result<(), &str>,
        open: impl FnOnce() -> Result<T, String>,
        log: impl FnOnce(&str),
    ) -> Result<&mut T, String> {
        if let Slot::Untried = self {
            *self = match allowed {
                Err(why) => Slot::Unusable(why.into()),
                Ok(()) => open().map_or_else(Slot::Unusable, Slot::Ready),
            };
            if let Slot::Unusable(why) = self {
                log(why);
            }
        }
        match self {
            Slot::Ready(t) => Ok(t),
            Slot::Unusable(why) => Err(why.clone()),
            Slot::Untried => unreachable!("opened above"),
        }
    }
}

static GPU: Mutex<Slot<Model>> = Mutex::new(Slot::Untried);
static CPU: Mutex<Option<(usize, Model)>> = Mutex::new(None);

/// A GPU segment slower than this is a software rasteriser (llvmpipe), which only steals a core from the CPU engine. The
/// first run compiles shaders (about 1 s on a fast GPU).
const GPU_TOO_SLOW: Duration = Duration::from_secs(10);

/// Why a GPU that just ran a segment is to be given up, if it is: the run failed, or took far too long.
fn gpu_verdict<T>(out: &Result<T, String>, took: Duration) -> Option<String> {
    match out {
        Err(e) => Some(e.clone()),
        Ok(_) if took > GPU_TOO_SLOW => Some(format!(
            "{:.1} s per segment: a software renderer",
            took.as_secs_f32()
        )),
        Ok(_) => None,
    }
}

/// `OPENFADER_STEMS_GPU=0` keeps the GPU out (comparisons, machines where Vulkan misbehaves); a build without the `stems-gpu`
/// feature has none.
fn gpu_allowed() -> Result<(), &'static str> {
    if !cfg!(feature = "stems-gpu") {
        Err("not in this build")
    } else if std::env::var("OPENFADER_STEMS_GPU").is_ok_and(|v| v == "0") {
        Err("disabled by OPENFADER_STEMS_GPU=0")
    } else {
        Ok(())
    }
}

fn gpu_session(slot: &mut Slot<Model>) -> Result<&mut Model, String> {
    slot.ensure(
        gpu_allowed(),
        || Model::open(1, true),
        |why| eprintln!("stems: GPU unavailable ({why}); separating on the CPU"),
    )
}

fn cpu_session(slot: &mut Option<(usize, Model)>, threads: usize) -> Result<&mut Model, String> {
    if slot.as_ref().is_none_or(|(t, _)| *t != threads) {
        *slot = None; // release the old weights before allocating a replacement
        *slot = Some((threads, Model::open(threads, false)?));
    }
    Ok(&mut slot.as_mut().unwrap().1)
}

/// Closes both sessions before the process exits: Dawn's teardown inside `exit()` can crash with a session still open.
/// Waits for a segment in flight; engines opened afterwards are out.
pub fn shutdown() {
    *GPU.lock() = Slot::Unusable("shut down".into());
    *CPU.lock() = None;
}

/// Closes the open sessions to give their memory back (about 1 GB for both); the next run opens them again (2 to 3 s). An
/// engine found unusable stays out. Waits for a segment in flight.
pub fn release() {
    let mut g = GPU.lock();
    if matches!(*g, Slot::Ready(_)) {
        *g = Slot::Untried;
    }
    drop(g);
    *CPU.lock() = None;
    trim_heap();
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Engine::Gpu => "GPU",
            Engine::Cpu(_) => "CPU",
        }
    }

    /// Opens the engine's session now if it is not open (the GPU: the first time per process). `Err` = this engine is out,
    /// for the GPU with the reason logged once.
    pub fn open(self) -> Result<(), String> {
        self.open_cancellable(&std::sync::atomic::AtomicBool::new(false))
    }

    /// Preflights an engine before it reserves any segments, checking cancellation under its session lock.
    pub fn open_cancellable(self, cancel: &std::sync::atomic::AtomicBool) -> Result<(), String> {
        if !usable() {
            return Err(missing_hint()); // not the engine's fault: nothing is remembered
        }
        match self {
            Engine::Gpu => with_active(&GPU, cancel, |g| gpu_session(g).map(|_| ())),
            Engine::Cpu(threads) => {
                with_active(&CPU, cancel, |c| cpu_session(c, threads).map(|_| ()))
            }
        }
    }

    /// Consumes one segment under the session lock; a cancelled queued job runs no inference.
    pub fn run(
        self,
        mix: Vec<f32>,
        cancel: &std::sync::atomic::AtomicBool,
        consume: &mut dyn FnMut(&[f32]) -> Result<(), String>,
    ) -> Result<(), String> {
        if !usable() {
            return Err(missing_hint());
        }
        match self {
            Engine::Gpu => with_active(&GPU, cancel, |g| {
                let m = gpu_session(g)?;
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                let started = Instant::now();
                let mut took = Duration::ZERO;
                let out = m.run(mix, &mut |data| {
                    took = started.elapsed();
                    consume(data)
                });
                if let Some(why) = gpu_verdict(&out, took) {
                    eprintln!("stems: GPU unusable ({why}); separating on the CPU");
                    *g = Slot::Unusable(why);
                }
                out
            }),
            Engine::Cpu(threads) => with_active(&CPU, cancel, |c| {
                let m = cpu_session(c, threads)?;
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                m.run(mix, consume)
            }),
        }
    }
}

fn with_active<T>(
    slot: &Mutex<T>,
    cancel: &std::sync::atomic::AtomicBool,
    run: impl FnOnce(&mut T) -> Result<(), String>,
) -> Result<(), String> {
    let mut slot = slot.lock();
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    run(&mut slot)
}

/// Opens the sessions in the background, so the first track loaded after STEMS is switched on does not wait for them.
pub fn warm(engines: &[Engine]) {
    for &e in engines {
        std::thread::spawn(move || {
            if e.open().is_ok() {
                let _ = e.run(
                    vec![0.0; 2 * SEGMENT],
                    &std::sync::atomic::AtomicBool::new(false),
                    &mut |_| Ok(()),
                ); // shaders, thread pool, allocator
            }
        });
    }
}

/// The engines a separation uses: the GPU, and the CPU with [`super::threads`] threads.
pub fn engines(multithread: bool) -> [Engine; 2] {
    [Engine::Gpu, Engine::Cpu(super::threads(multithread))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_while_waiting_for_session_skips_the_run() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        let (slot, cancel) = (Mutex::new(0usize), AtomicBool::new(false));
        let guard = slot.lock();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (slot, cancel) = (&slot, &cancel);
            let queued = scope.spawn(move || {
                tx.send(()).unwrap();
                with_active(slot, cancel, |runs| {
                    *runs += 1;
                    Ok(())
                })
            });
            rx.recv().unwrap();
            cancel.store(true, Relaxed);
            drop(guard);
            assert_eq!(queued.join().unwrap(), Err("cancelled".into()));
        });
        assert_eq!(*slot.lock(), 0);
        with_active(&slot, &AtomicBool::new(false), |runs| {
            *runs += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(*slot.lock(), 1);
    }

    #[test]
    fn cancelled_session_waiter_does_not_initialize_the_engine() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        let (slot, cancel) = (Mutex::new(Slot::<usize>::Untried), AtomicBool::new(false));
        let guard = slot.lock();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (slot, cancel) = (&slot, &cancel);
            let queued = scope.spawn(move || {
                tx.send(()).unwrap();
                with_active(slot, cancel, |slot| {
                    slot.ensure(
                        Ok(()),
                        || panic!("cancelled preflight opened an engine"),
                        |_| {},
                    )
                    .map(|_| ())
                })
            });
            rx.recv().unwrap();
            cancel.store(true, Relaxed);
            drop(guard);
            assert_eq!(queued.join().unwrap(), Err("cancelled".into()));
        });
        assert!(matches!(*slot.lock(), Slot::Untried));
        with_active(&slot, &AtomicBool::new(false), |slot| {
            slot.ensure(Ok(()), || Ok(7), |_| {}).map(|_| ())
        })
        .unwrap();
        assert!(matches!(*slot.lock(), Slot::Ready(7)));
    }

    #[test]
    fn meminfo_parse() {
        let t = "MemTotal:       32000000 kB\nMemAvailable:    7340032 kB\nBuffers: 1 kB\n";
        assert_eq!(mem_available_kib(t), Some(7_340_032));
        assert_eq!(mem_available_kib("MemTotal: 1 kB\n"), None);
    }
    use std::cell::Cell;

    #[test]
    fn a_gpu_that_will_not_open_falls_back_once_and_is_never_asked_again() {
        let (mut gpu, opens, logs) = (Slot::<u32>::Untried, Cell::new(0), Cell::new(0));
        let mut ask = |allowed| {
            gpu.ensure(
                allowed,
                || {
                    opens.set(opens.get() + 1);
                    Err("no Vulkan device".into())
                },
                |why| {
                    assert_eq!(why, "no Vulkan device");
                    logs.set(logs.get() + 1);
                },
            )
            .map(|n| *n)
        };
        assert_eq!(ask(Ok(())), Err("no Vulkan device".into()));
        assert_eq!(ask(Ok(())), Err("no Vulkan device".into()));
        assert_eq!((opens.get(), logs.get()), (1, 1), "tried once, logged once");
    }

    #[test]
    fn a_gpu_that_opens_is_kept_and_a_disabled_one_is_never_opened() {
        let (mut gpu, opens) = (Slot::Untried, Cell::new(0));
        let open = || {
            opens.set(opens.get() + 1);
            Ok(7u32)
        };
        assert_eq!(
            gpu.ensure(Ok(()), open, |_| panic!("works")).map(|n| *n),
            Ok(7)
        );
        assert_eq!(
            gpu.ensure(Ok(()), || panic!("opened twice"), |_| panic!("works"))
                .map(|n| *n),
            Ok(7)
        );
        assert_eq!(opens.get(), 1);

        let mut off = Slot::<u32>::Untried;
        let logged = Cell::new(false);
        let why = off
            .ensure(
                Err("disabled by OPENFADER_STEMS_GPU=0"),
                || panic!("never opened"),
                |_| logged.set(true),
            )
            .map(|n| *n);
        assert_eq!(why, Err("disabled by OPENFADER_STEMS_GPU=0".into()));
        assert!(logged.get(), "the reason is logged, not silent");
    }

    #[test]
    fn a_failed_or_software_rendered_run_gives_the_gpu_up_a_fast_one_does_not() {
        assert_eq!(gpu_verdict(&Ok(()), Duration::from_millis(600)), None);
        assert_eq!(
            gpu_verdict(&Ok(()), Duration::from_secs(1)),
            None,
            "shader compile on the first run"
        );
        assert!(
            gpu_verdict(&Ok(()), Duration::from_secs(30))
                .unwrap()
                .contains("software")
        );
        assert_eq!(
            gpu_verdict::<()>(&Err("device lost".into()), Duration::ZERO).as_deref(),
            Some("device lost")
        );
    }
}
