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
use parking_lot::{Condvar, Mutex};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};
use std::time::Duration;
#[cfg(all(test, feature = "stems-gpu"))]
use std::time::Instant;

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

static LOW_MEMORY: AtomicU8 = AtomicU8::new(0);

/// Apply the resolved low-power preference; before the UI applies it, use the hardware memory budget.
pub fn set_low_memory(enabled: bool) {
    LOW_MEMORY.store(if enabled { 1 } else { 2 }, Relaxed);
}

pub(super) fn low_memory() -> bool {
    match LOW_MEMORY.load(Relaxed) {
        1 => true,
        2 => false,
        _ => crate::memory::constrained(),
    }
}

/// Never attempt the multi-GiB fold without a known memory budget; the original model remains usable.
fn fold_memory_ok() -> Result<(), String> {
    fold_memory_budget(crate::memory::available_bytes(), low_memory())
}

fn fold_memory_budget(available: Option<u64>, constrained: bool) -> Result<(), String> {
    if constrained {
        return Err("low-power memory budget; using the original model without folding".into());
    }
    match available.map(|bytes| bytes >> 10) {
        Some(k) if k < FOLD_MIN_KIB => Err(format!(
            "not enough free memory to fold the model ({} MiB available, {} MiB needed)",
            k >> 10,
            FOLD_MIN_KIB >> 10
        )),
        Some(_) => Ok(()),
        None => Err("available memory is unknown; using the original model without folding".into()),
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
        let cache = if low_memory() {
            GpuCache::Disabled
        } else {
            GpuCache::Default
        };
        Self::open_with_gpu_cache(threads, gpu, cache)
    }

    fn open_with_gpu_cache(threads: usize, gpu: bool, _cache: GpuCache) -> Result<Model, String> {
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
            let providers = vec![
                ep::WebGPU::default()
                    // Tight budgets return temporary buffers promptly; roomy devices reuse them until the batch ends.
                    .with_storage_buffer_cache_mode(_cache.mode())
                    .build()
                    .error_on_failure(),
                cpu,
            ];
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
/// cuDNN); the CPU is the default provider. Sessions are shared by all active separation batches and closed when the last
/// batch ends. Opening costs about 2 s, and the GPU's first run compiles its shaders (about 1 s more).
///
/// On machines above the low-memory budget the two run side by side; smaller machines run one batch on the GPU, falling
/// back to the CPU if needed, without retaining two models and their scratch space.
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

enum GpuCache {
    Disabled,
    Default,
    #[cfg(all(test, feature = "stems-gpu"))]
    Simple,
}

#[cfg(feature = "stems-gpu")]
impl GpuCache {
    fn mode(self) -> ep::webgpu::BufferCacheMode {
        match self {
            Self::Disabled => ep::webgpu::BufferCacheMode::Disabled,
            Self::Default => ep::webgpu::BufferCacheMode::Bucket,
            #[cfg(test)]
            Self::Simple => ep::webgpu::BufferCacheMode::Simple,
        }
    }
}

struct BatchState {
    active: usize,
    serial: bool,
}

impl BatchState {
    fn can_start(&self, serial: bool) -> bool {
        !self.serial && (!serial || self.active == 0)
    }
}

static ACTIVE_BATCHES: Mutex<BatchState> = Mutex::new(BatchState {
    active: 0,
    serial: false,
});
static BATCH_READY: Condvar = Condvar::new();

/// Held on the job worker, never on the UI/audio thread or for a cache hit.
pub(super) struct Batch {
    serial: bool,
}

impl Batch {
    pub(super) fn start(cancel: &AtomicBool) -> Result<Self, String> {
        let serial = low_memory();
        admit_batch(&ACTIVE_BATCHES, &BATCH_READY, serial, cancel)?;
        Ok(Self { serial })
    }

    pub(super) fn serial(&self) -> bool {
        self.serial
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        finish_batch(&ACTIVE_BATCHES, release);
        BATCH_READY.notify_all();
    }
}

fn admit_batch(
    active: &Mutex<BatchState>,
    ready: &Condvar,
    serial: bool,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut state = active.lock();
    loop {
        if cancel.load(Relaxed) {
            return Err("cancelled".into());
        }
        if state.can_start(serial) {
            state.active += 1;
            state.serial = serial;
            return Ok(());
        }
        ready.wait_for(&mut state, Duration::from_millis(100));
    }
}

fn finish_batch(active: &Mutex<BatchState>, release: impl FnOnce()) {
    let mut state = active.lock();
    state.active -= 1;
    if state.active == 0 {
        state.serial = false;
        // Keep admission locked through teardown so a new batch cannot lose its sessions.
        release();
    }
}

/// Elapsed time cannot distinguish a small GPU from a software renderer; only a failed run retires it.
fn gpu_verdict<T>(out: &Result<T, String>) -> Option<String> {
    out.as_ref().err().cloned()
}

/// PREFS > Creative tools > STEMS PROCESSING: `Auto` uses the GPU where that is safe, `Cpu` never does. Set by the app from the
/// settings; a running separation keeps its engines, the next one follows the new choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Processor {
    #[default]
    Auto,
    Cpu,
}

static PROCESSOR: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_processor(p: Processor) {
    PROCESSOR.store(p as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn processor() -> Processor {
    match PROCESSOR.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Processor::Cpu,
        _ => Processor::Auto,
    }
}

/// Whether the GPU engine may open: the setting ([`Processor`]), `OPENFADER_STEMS_GPU=0` (comparisons, machines where Vulkan
/// misbehaves), a build without the `stems-gpu` feature, and on Windows a machine whose only graphics adapters are integrated
/// ones ([`integrated_only`]): there the desktop runs on the same GPU, and a segment of inference (seconds of compute the
/// driver cannot preempt) freezes the screen, the pointer and the taskbar with it (issue 42). `OPENFADER_STEMS_GPU=1` overrides
/// that check.
fn gpu_allowed() -> Result<(), &'static str> {
    let forced = std::env::var("OPENFADER_STEMS_GPU").ok();
    if !cfg!(feature = "stems-gpu") {
        Err("not in this build")
    } else if forced.as_deref() == Some("0") {
        Err("disabled by OPENFADER_STEMS_GPU=0")
    } else if processor() == Processor::Cpu {
        Err("STEMS PROCESSING is set to CPU")
    } else if forced.as_deref() != Some("1") && integrated_only() {
        Err("only integrated graphics here: the desktop would freeze while a segment runs")
    } else {
        Ok(())
    }
}

/// Windows: every display adapter the driver class lists is an integrated one. The registry's display class
/// (`{4d36e968-e325-11ce-bfc1-08002be10318}`) holds one subkey per adapter with its `DriverDesc` and, from the driver, its
/// dedicated memory (`HardwareInformation.qwMemorySize`); an Intel adapter that is not an Arc, or any adapter the driver gives
/// less than 1 GiB of its own memory, is integrated. Unreadable = false (the GPU stays allowed, as before). Checked once.
#[cfg(windows)]
fn integrated_only() -> bool {
    static ONLY: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let adapters = display_adapters();
        let verdict =
            !adapters.is_empty() && adapters.iter().all(|(desc, mem)| integrated(desc, *mem));
        if verdict {
            eprintln!(
                "stems: integrated graphics only ({}): GPU separation off",
                adapters
                    .iter()
                    .map(|a| a.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        verdict
    });
    *ONLY
}

#[cfg(not(windows))]
fn integrated_only() -> bool {
    false
}

/// Whether an adapter with this `DriverDesc` and dedicated memory (bytes, `None` when the driver reports none) is integrated.
fn integrated(desc: &str, dedicated: Option<u64>) -> bool {
    let d = desc.to_ascii_lowercase();
    if d.contains("intel") {
        return !d.contains("arc");
    }
    dedicated.is_none_or(|m| m < 1 << 30)
}

/// `(DriverDesc, HardwareInformation.qwMemorySize)` of every adapter under the display class key.
#[cfg(windows)]
fn display_adapters() -> Vec<(String, Option<u64>)> {
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_QWORD, RRF_RT_REG_SZ, RegCloseKey,
        RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let class =
        wide(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}");
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: valid null-terminated path, out-pointer to an HKEY; the key is closed below.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, class.as_ptr(), 0, KEY_READ, &mut key) }
        != ERROR_SUCCESS
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    for index in 0.. {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: the buffer and its length are passed together; unused out-parameters are null as the API allows.
        let r = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if r == ERROR_NO_MORE_ITEMS || r != ERROR_SUCCESS {
            break;
        }
        let sub = String::from_utf16_lossy(&name[..len as usize]);
        if !sub.chars().all(|c| c.is_ascii_digit()) {
            continue; // "Properties" and the like
        }
        let sub_w = wide(&sub);
        let mut desc = [0u16; 512];
        let mut size = (desc.len() * 2) as u32;
        // SAFETY: byte size of the buffer is passed with it; RRF_RT_REG_SZ makes the API null-terminate within it.
        let r = unsafe {
            RegGetValueW(
                key,
                sub_w.as_ptr(),
                wide("DriverDesc").as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                desc.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if r != ERROR_SUCCESS {
            continue;
        }
        let chars = (size as usize / 2).min(desc.len());
        let desc = String::from_utf16_lossy(&desc[..chars])
            .trim_end_matches('\0')
            .to_owned();
        let mut mem = 0u64;
        let mut msize = 8u32;
        // SAFETY: an 8-byte out-buffer for a QWORD value.
        let r = unsafe {
            RegGetValueW(
                key,
                sub_w.as_ptr(),
                wide("HardwareInformation.qwMemorySize").as_ptr(),
                RRF_RT_REG_QWORD,
                std::ptr::null_mut(),
                (&mut mem as *mut u64).cast(),
                &mut msize,
            )
        };
        out.push((desc, (r == ERROR_SUCCESS).then_some(mem)));
    }
    // SAFETY: the key was opened above.
    unsafe { RegCloseKey(key) };
    out
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

/// Closes idle sessions on the last batch's worker; an unusable GPU stays out.
fn release() {
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
                let out = m.run(mix, consume);
                if let Some(why) = gpu_verdict(&out) {
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

/// The engines a separation uses under the admitted batch's immutable memory budget.
pub fn engines(multithread: bool, low_memory: bool) -> [Engine; 2] {
    [
        Engine::Gpu,
        Engine::Cpu(super::threads(multithread, low_memory)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrated_adapters_are_told_from_dedicated_ones() {
        assert!(integrated("Intel(R) Iris(R) Xe Graphics", Some(128 << 20)));
        assert!(integrated("Intel(R) UHD Graphics 630", None));
        assert!(!integrated(
            "Intel(R) Arc(TM) A770 Graphics",
            Some(16 << 30)
        ));
        assert!(!integrated("NVIDIA GeForce RTX 3090", Some(24 << 30)));
        assert!(
            integrated("AMD Radeon(TM) Graphics", Some(512 << 20)),
            "an APU"
        );
        assert!(!integrated("AMD Radeon RX 6700 XT", Some(12 << 30)));
    }

    #[test]
    fn the_cpu_choice_keeps_the_gpu_closed() {
        set_processor(Processor::Cpu);
        assert_eq!(gpu_allowed(), Err("STEMS PROCESSING is set to CPU"));
        set_processor(Processor::Auto);
        assert_ne!(gpu_allowed(), Err("STEMS PROCESSING is set to CPU"));
    }

    #[test]
    fn folding_requires_known_free_memory_for_its_peak() {
        assert!(fold_memory_budget(None, false).is_err());
        assert!(fold_memory_budget(Some((FOLD_MIN_KIB << 10) - 1), false).is_err());
        assert_eq!(fold_memory_budget(Some(FOLD_MIN_KIB << 10), false), Ok(()));
        assert!(fold_memory_budget(Some(16 << 30), true).is_err());
    }

    #[test]
    fn only_the_last_batch_releases_sessions_with_admission_still_locked() {
        let active = Mutex::new(BatchState {
            active: 2,
            serial: false,
        });
        let releases = Cell::new(0);
        finish_batch(&active, || panic!("another batch still needs its sessions"));
        assert_eq!(active.lock().active, 1);
        finish_batch(&active, || {
            assert!(
                active.try_lock().is_none(),
                "teardown must exclude new admission"
            );
            releases.set(releases.get() + 1);
        });
        assert_eq!(active.lock().active, 0);
        assert_eq!(releases.get(), 1);
    }

    #[test]
    fn serial_budget_waits_for_parallel_batches_and_survives_preference_changes() {
        let parallel = BatchState {
            active: 2,
            serial: false,
        };
        assert!(parallel.can_start(false));
        assert!(!parallel.can_start(true));
        let serial = BatchState {
            active: 1,
            serial: true,
        };
        assert!(
            !serial.can_start(false),
            "turning low-power off cannot overlap an exclusive job"
        );
        assert!(!serial.can_start(true));
        let idle = BatchState {
            active: 0,
            serial: false,
        };
        assert!(idle.can_start(false));
        assert!(idle.can_start(true));
    }

    #[test]
    fn cancelled_batch_waiter_releases_its_payload_without_waiting_for_active_inference() {
        let active = Mutex::new(BatchState {
            active: 1,
            serial: true,
        });
        let ready = Condvar::new();
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (started, rx) = std::sync::mpsc::channel();
            let (done, result) = std::sync::mpsc::channel();
            let (active, ready, cancel) = (&active, &ready, &cancel);
            let waiter = scope.spawn(move || {
                started.send(()).unwrap();
                done.send(admit_batch(active, ready, true, cancel)).unwrap();
            });
            rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(30));
            cancel.store(true, Relaxed);
            let stopped = result.recv_timeout(Duration::from_secs(2));
            ready.notify_all(); // let a broken waiter exit too, rather than hanging the suite
            waiter.join().unwrap();
            assert_eq!(stopped.unwrap(), Err("cancelled".into()));
        });
        assert_eq!(active.lock().active, 1);
    }

    fn probe_mix() -> Vec<f32> {
        (0..2 * SEGMENT)
            .map(|i| {
                let phase = (i % SEGMENT) as f32;
                let carrier = if i < SEGMENT { 0.031 } else { 0.047 };
                0.2 * (phase * carrier).sin() + 0.1 * (phase * 0.013).cos()
            })
            .collect()
    }

    fn report_memory(phase: &str) {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let memory: Vec<_> = status
            .lines()
            .filter(|l| l.starts_with("VmRSS:") || l.starts_with("VmHWM:"))
            .collect();
        eprintln!(
            "model-memory pid={} phase={phase} {}",
            std::process::id(),
            memory.join(" ")
        );
    }

    #[cfg(feature = "stems-gpu")]
    fn gpu_memory_probe(cache: GpuCache) {
        gpu_allowed().expect("GPU probe needs the stems-gpu feature and a GPU");
        report_memory("before-open");
        let mut model = Model::open_with_gpu_cache(1, true, cache).expect("open real GPU model");
        report_memory("opened");
        for segment in 0..3 {
            let start = Instant::now();
            model
                .run(probe_mix(), &mut |data| {
                    assert!(data.iter().all(|v| v.is_finite()));
                    let energy: f64 = data.iter().map(|&v| (v as f64).powi(2)).sum();
                    assert!(energy > 0.0, "nonzero input must produce nonzero stems");
                    let hash = data.iter().fold(0xcbf29ce484222325u64, |hash, v| {
                        (hash ^ u64::from(v.to_bits())).wrapping_mul(0x100000001b3)
                    });
                    eprintln!(
                        "model-memory segment={segment} energy={energy:.12} hash={hash:016x}"
                    );
                    Ok(())
                })
                .expect("real GPU inference");
            eprintln!(
                "model-memory segment={segment} seconds={:.6}",
                start.elapsed().as_secs_f64()
            );
            report_memory("segment-finished");
        }
        drop(model);
        trim_heap();
        report_memory("released");
    }

    #[cfg(feature = "stems-gpu")]
    #[test]
    #[ignore = "real model/GPU memory probe; run alone with --nocapture"]
    fn gpu_default_cache_memory_probe() {
        gpu_memory_probe(GpuCache::Default);
    }

    #[cfg(feature = "stems-gpu")]
    #[test]
    #[ignore = "real model/GPU memory probe; run alone with --nocapture"]
    fn gpu_simple_cache_memory_probe() {
        gpu_memory_probe(GpuCache::Simple);
    }

    #[cfg(feature = "stems-gpu")]
    #[test]
    #[ignore = "real model/GPU memory probe; run alone with --nocapture"]
    fn gpu_disabled_cache_memory_probe() {
        gpu_memory_probe(GpuCache::Disabled);
    }

    #[test]
    #[ignore = "real model lifecycle/memory probe; run alone with --nocapture"]
    fn cpu_batch_releases_the_model_after_multiple_segments() {
        let cancel = AtomicBool::new(false);
        report_memory("before-batch");
        let batch = Batch::start(&cancel).unwrap();
        for _ in 0..3 {
            Engine::Cpu(2)
                .run(probe_mix(), &cancel, &mut |data| {
                    assert!(data.iter().all(|v| v.is_finite()));
                    assert!(data.iter().any(|&v| v != 0.0));
                    Ok(())
                })
                .expect("real CPU inference");
            assert!(CPU.lock().is_some());
            report_memory("segment-finished");
        }
        drop(batch);
        assert!(
            CPU.lock().is_none(),
            "last batch must release model weights"
        );
        assert!(!matches!(*GPU.lock(), Slot::Ready(_)));
        report_memory("released");
    }

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
    fn only_an_inference_error_retires_the_gpu() {
        assert_eq!(gpu_verdict(&Ok(())), None);
        assert_eq!(
            gpu_verdict::<()>(&Err("device lost".into())).as_deref(),
            Some("device lost")
        );
    }
}
