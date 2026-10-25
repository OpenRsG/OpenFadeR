//! Library plugins: a native shared library that reads a track library (a file or folder in some format) for the host,
//! may write the host's collection in that format, and may work with removable devices. ABI version [`LIBRARY_ABI`],
//! behind the `library` feature.
//!
//! The library exports one C function, [`LIBRARY_ENTRY`], returning a static [`LibraryApi`]: `call` takes one JSON
//! request (and the host's callback table, see [`host`]) and returns one JSON answer in a buffer the library owns until the
//! host hands it to `free`. Every request is an object with an `"op"`; an answer is an object, or `{"error": "<text>"}`. An
//! op the library does not know is an error. Requests and answers are the types of this module ([`Request`], [`Probe`], …),
//! so host and plugin share one definition.
//!
//! | `op` | other fields | answer |
//! |---|---|---|
//! | `probe` | `path` | [`Probe`] `{score 0..=100, kind}`: does this plugin read that file or folder? 0: no |
//! | `open` | `path`, `settings` | [`Opened`] `{handle, kind}` |
//! | `playlists` | `handle` | `{"playlists": [`[`Playlist`]`]}` |
//! | `tracks` | `handle`, `playlist` (id or null: all tracks) | `{"tracks": [`[`Track`]`]}` |
//! | `search` | `handle`, `playlist`, `query` | `{"started": bool}` |
//! | `analysis` | `handle`, `track` | `{"analysis": `[`Analysis`]` or null}` |
//! | `analysis_ref` | `reference`, `settings` | `{"analysis": `[`Analysis`]` or null}`: the analysis behind a [`Track::analysis_ref`] |
//! | `close` | `handle` | `{}` |
//! | `write_collection` | `path`, `snapshot`, `settings` | `{"count": tracks written}` |
//! | `capabilities` | `settings` | [`Capabilities`]: what the format holds (a plugin older than this op answers an error: the host assumes the default) |
//! | device ops | see [`device`] | |
//!
//! `settings` is the plugin's own settings from `plugin.json` (id → value). The host keeps a track's `analysis_ref` as
//! `<plugin>:<opaque>` ([`owned_ref`]): the plugin only ever sees its own opaque part. In Rust, implement [`LibraryPlugin`]
//! and [`Source`] and call [`export_library!`]: it generates the C ABI and keeps panics from crossing it.

pub mod commit;
mod device;
pub mod host;
mod model;
mod tree;
pub use device::*;
pub use model::*;
pub use tree::*;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};

/// ABI version of the library plugin interface.
pub const LIBRARY_ABI: u32 = 2;

/// Name of the entry point a library plugin exports (NUL terminated, for `dlsym`).
pub const LIBRARY_ENTRY: &[u8] = b"openfader_library_v2\0";

/// A buffer the library allocated; give it back to [`LibraryApi::free`] exactly once.
#[repr(C)]
pub struct Buf {
    pub ptr: *mut u8,
    pub len: usize,
}

/// What the entry point returns: a static table.
#[repr(C)]
pub struct LibraryApi {
    /// [`LIBRARY_ABI`]; the host reads nothing else before checking it.
    pub abi: u32,
    /// One JSON request in, one JSON answer out; `host` is valid until the call returns. Never unwinds; an internal failure
    /// is an `{"error"}` answer.
    pub call: unsafe extern "C" fn(req: *const u8, len: usize, host: *const host::Host) -> Buf,
    /// Frees an answer of `call`.
    pub free: unsafe extern "C" fn(buf: Buf),
}

// SAFETY: an immutable table of function pointers.
unsafe impl Sync for LibraryApi {}

/// A request, `{"op": "<snake_case variant>", …}` in JSON.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Probe {
        path: PathBuf,
    },
    Open {
        path: PathBuf,
        #[serde(default)]
        settings: Value,
    },
    Playlists {
        handle: u64,
    },
    Tracks {
        handle: u64,
        playlist: Option<String>,
    },
    Search {
        handle: u64,
        playlist: String,
        query: String,
    },
    Analysis {
        handle: u64,
        track: Track,
    },
    Close {
        handle: u64,
    },
    WriteCollection {
        path: PathBuf,
        snapshot: Snapshot,
        #[serde(default)]
        settings: Value,
    },
    AnalysisRef {
        reference: String,
        #[serde(default)]
        settings: Value,
    },
    DeviceProbe {
        root: PathBuf,
        #[serde(default)]
        settings: Value,
    },
    Export {
        root: PathBuf,
        snapshot: Snapshot,
        options: ExportOptions,
        #[serde(default)]
        settings: Value,
    },
    Update {
        root: PathBuf,
        snapshot: Snapshot,
        #[serde(default)]
        baselines: BTreeMap<String, i64>,
        #[serde(default)]
        settings: Value,
    },
    Remove {
        root: PathBuf,
        target: RemoveTarget,
        #[serde(default)]
        delete_tracks: bool,
        #[serde(default)]
        config: BTreeMap<String, String>,
        #[serde(default)]
        settings: Value,
    },
    DeviceAction {
        root: PathBuf,
        action: String,
        #[serde(default)]
        confirmed: bool,
        #[serde(default)]
        config: BTreeMap<String, String>,
        #[serde(default)]
        settings: Value,
    },
    SettingsSchema {
        #[serde(default)]
        settings: Value,
    },
    SettingsRead {
        root: PathBuf,
        #[serde(default)]
        settings: Value,
    },
    SettingsWrite {
        root: PathBuf,
        values: BTreeMap<String, String>,
        #[serde(default)]
        settings: Value,
    },
    BackupTracks {
        dir: PathBuf,
        #[serde(default)]
        settings: Value,
    },
    Capabilities {
        #[serde(default)]
        settings: Value,
    },
    DeviceCheck {
        root: PathBuf,
        #[serde(default)]
        config: BTreeMap<String, String>,
        #[serde(default)]
        settings: Value,
    },
    DeviceFix {
        root: PathBuf,
        fix: String,
        #[serde(default)]
        config: BTreeMap<String, String>,
        #[serde(default)]
        settings: Value,
    },
}

/// Answer of `probe`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Probe {
    /// 0 (not mine) to 100 (certainly mine); the host opens with the highest score.
    pub score: u8,
    /// Short name of the source, for the window title.
    pub kind: String,
}

/// Answer of `open`.
#[derive(Serialize, Deserialize, Debug)]
pub struct Opened {
    pub handle: u64,
    pub kind: String,
}

/// An opened library.
pub trait Source: Send {
    /// Short name of the source, for the window title.
    fn kind(&self) -> String;
    /// Folders and plain playlists in depth-first tree order.
    fn playlists(&self) -> Result<Vec<Playlist>, String>;
    /// `None`: all tracks. `Some(id)`: that playlist's tracks in playlist order.
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String>;
    /// Runs `query` in the search entry `playlist`; the results then appear in `tracks(Some(playlist))`. `false`:
    /// `playlist` is no search entry.
    fn search(&self, _playlist: &str, _query: &str) -> bool {
        false
    }
    /// Beat grid, cues and waveforms the source holds for `track`; `None` when it has none.
    fn analysis(&self, _track: &Track) -> Option<Analysis> {
        None
    }
}

/// A library plugin. [`export_library!`] turns a type implementing it into the C ABI.
pub trait LibraryPlugin {
    /// Whether this plugin reads `path` (a file or folder): the score and the source's name, `None` for no. Cheap: look at
    /// the name and the first bytes, never the whole file.
    fn probe(path: &Path) -> Option<Probe>;
    /// Opens `path` with the plugin's `settings`.
    fn open(path: &Path, settings: &Value) -> Result<Box<dyn Source>, String>;
    /// Writes the collection of `snapshot` as a file at `path` (new, chosen by the host); returns the number of tracks.
    fn write_collection(
        _path: &Path,
        _snapshot: &Snapshot,
        _settings: &Value,
    ) -> Result<usize, String> {
        Err("this plugin writes no collection".into())
    }
    /// The analysis behind a track's `analysis_ref` (the opaque part this plugin made), without an opened library; `None`
    /// when the plugin holds none.
    fn analysis_ref(_reference: &str, _settings: &Value) -> Option<Analysis> {
        None
    }
    /// What the format this plugin writes can hold; the host fits a collection to it before `write_collection` / `export`.
    fn capabilities(_settings: &Value) -> Capabilities {
        Capabilities::default()
    }

    // Devices (see [`device`]): a plugin that declares `"device": true` implements what its `DeviceInfo::features` lists.

    /// What the plugin finds on the device mounted at `root`.
    fn device_probe(_root: &Path, _settings: &Value) -> Result<DeviceInfo, String> {
        Err("this plugin handles no devices".into())
    }
    /// Writes `snapshot` to the device: a new library when it has none, else the existing one is extended.
    fn export(
        _root: &Path,
        _snapshot: &Snapshot,
        _options: &ExportOptions,
        _settings: &Value,
    ) -> Result<ExportReport, String> {
        Err("this plugin exports nothing".into())
    }
    /// "Update Collection": what the players changed since `baselines`.
    fn update(
        _root: &Path,
        _snapshot: &Snapshot,
        _baselines: &BTreeMap<String, i64>,
        _settings: &Value,
    ) -> Result<UpdateReport, String> {
        Err("this plugin updates nothing".into())
    }
    fn remove(
        _root: &Path,
        _target: &RemoveTarget,
        _delete_tracks: bool,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<RemoveReport, String> {
        Err("this plugin removes nothing".into())
    }
    fn device_action(
        _root: &Path,
        _action: &str,
        _confirmed: bool,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<ActionAnswer, String> {
        Err("this plugin has no device actions".into())
    }
    fn settings_schema(_settings: &Value) -> Result<SettingsSchema, String> {
        Err("this plugin has no device settings".into())
    }
    fn settings_read(_root: &Path, _settings: &Value) -> Result<SettingsRead, String> {
        Err("this plugin has no device settings".into())
    }
    fn settings_write(
        _root: &Path,
        _values: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<SettingsWritten, String> {
        Err("this plugin has no device settings".into())
    }
    /// The tracks the library inside a backup folder `dir` lists.
    fn backup_tracks(_dir: &Path, _settings: &Value) -> Result<Vec<BackupTrack>, String> {
        Err("this plugin has no backups".into())
    }
    /// The state of this plugin's libraries on the device (`Feature::Check`): read-only, quick enough to run on every plug-in.
    fn device_check(
        _root: &Path,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<CheckReport, String> {
        Err("this plugin checks no devices".into())
    }
    /// Runs the fix `fix` a [`Problem::fix`] of [`LibraryPlugin::device_check`] named (`Feature::Check`).
    fn device_fix(
        _root: &Path,
        _fix: &str,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<ActionAnswer, String> {
        Err("this plugin fixes no devices".into())
    }
}

type Handle = Arc<Mutex<Box<dyn Source>>>;
/// The opened libraries of this plugin (the SDK is linked into each plugin, so each has its own table).
static OPEN: Mutex<BTreeMap<u64, Handle>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

fn handle(h: u64) -> Result<Handle, String> {
    OPEN.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&h)
        .cloned()
        .ok_or_else(|| format!("no open library {h}"))
}

fn with<R>(h: u64, f: impl FnOnce(&dyn Source) -> R) -> Result<R, String> {
    let s = handle(h)?;
    let s = s.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(f(&**s))
}

fn dispatch<P: LibraryPlugin>(req: &[u8]) -> Result<Value, String> {
    let req: Request = serde_json::from_slice(req).map_err(|e| format!("bad request: {e}"))?;
    Ok(match req {
        Request::Probe { path } => json!(P::probe(&path).unwrap_or(Probe {
            score: 0,
            kind: String::new(),
        })),
        Request::Open { path, settings } => {
            let s = P::open(&path, &settings)?;
            let kind = s.kind();
            let handle = NEXT.fetch_add(1, Relaxed);
            OPEN.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(handle, Arc::new(Mutex::new(s)));
            json!(Opened { handle, kind })
        }
        Request::Playlists { handle } => json!({ "playlists": with(handle, |s| s.playlists())?? }),
        Request::Tracks { handle, playlist } => {
            json!({ "tracks": with(handle, |s| s.tracks(playlist.as_deref()))?? })
        }
        Request::Search {
            handle,
            playlist,
            query,
        } => json!({ "started": with(handle, |s| s.search(&playlist, &query))? }),
        Request::Analysis { handle, track } => {
            json!({ "analysis": with(handle, |s| s.analysis(&track))? })
        }
        Request::Close { handle } => {
            OPEN.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&handle);
            json!({})
        }
        Request::WriteCollection {
            path,
            snapshot,
            settings,
        } => json!({ "count": P::write_collection(&path, &snapshot, &settings)? }),
        Request::AnalysisRef {
            reference,
            settings,
        } => json!(Analysed {
            analysis: P::analysis_ref(&reference, &settings)
        }),
        Request::DeviceProbe { root, settings } => json!(P::device_probe(&root, &settings)?),
        Request::Export {
            root,
            snapshot,
            options,
            settings,
        } => json!(P::export(&root, &snapshot, &options, &settings)?),
        Request::Update {
            root,
            snapshot,
            baselines,
            settings,
        } => json!(P::update(&root, &snapshot, &baselines, &settings)?),
        Request::Remove {
            root,
            target,
            delete_tracks,
            config,
            settings,
        } => json!(P::remove(
            &root,
            &target,
            delete_tracks,
            &config,
            &settings
        )?),
        Request::DeviceAction {
            root,
            action,
            confirmed,
            config,
            settings,
        } => json!(P::device_action(
            &root, &action, confirmed, &config, &settings
        )?),
        Request::SettingsSchema { settings } => json!(P::settings_schema(&settings)?),
        Request::SettingsRead { root, settings } => json!(P::settings_read(&root, &settings)?),
        Request::SettingsWrite {
            root,
            values,
            settings,
        } => json!(P::settings_write(&root, &values, &settings)?),
        Request::BackupTracks { dir, settings } => {
            json!({ "tracks": P::backup_tracks(&dir, &settings)? })
        }
        Request::Capabilities { settings } => json!(P::capabilities(&settings)),
        Request::DeviceCheck {
            root,
            config,
            settings,
        } => json!(P::device_check(&root, &config, &settings)?),
        Request::DeviceFix {
            root,
            fix,
            config,
            settings,
        } => json!(P::device_fix(&root, &fix, &config, &settings)?),
    })
}

/// The answer to `req`, as JSON bytes; panics become an `{"error"}` answer.
#[doc(hidden)]
pub fn answer<P: LibraryPlugin>(req: &[u8], host: *const host::Host) -> Vec<u8> {
    let r = catch_unwind(AssertUnwindSafe(|| {
        host::serving(host, || dispatch::<P>(req))
    }))
    .unwrap_or_else(|_| Err("the plugin panicked".into()));
    let v = r.unwrap_or_else(|e| json!({ "error": e }));
    serde_json::to_vec(&v).unwrap_or_else(|_| br#"{"error":"unserialisable answer"}"#.to_vec())
}

/// `LibraryApi::call` of `P`, for [`export_library!`].
///
/// # Safety
/// `req` points to `len` readable bytes; `host` is null or valid until this returns.
#[doc(hidden)]
pub unsafe extern "C" fn call_c<P: LibraryPlugin>(
    req: *const u8,
    len: usize,
    host: *const host::Host,
) -> Buf {
    let req: &[u8] = if req.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(req, len) }
    };
    let out = answer::<P>(req, host).into_boxed_slice();
    let len = out.len();
    Buf {
        ptr: Box::into_raw(out).cast::<u8>(),
        len,
    }
}

/// `LibraryApi::free`, for [`export_library!`].
///
/// # Safety
/// `buf` came from [`call_c`] and is not used again.
#[doc(hidden)]
pub unsafe extern "C" fn free_c(buf: Buf) {
    if !buf.ptr.is_null() {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.ptr, buf.len)) });
    }
}

/// The table of `P`: what [`export_library!`] returns from the entry point, and what a plugin compiled into the host is
/// called through (the same JSON `call`, no second path).
pub const fn api<P: LibraryPlugin>() -> LibraryApi {
    LibraryApi {
        abi: LIBRARY_ABI,
        call: call_c::<P>,
        free: free_c,
    }
}

/// Exports a [`LibraryPlugin`] from a `cdylib`: defines the `openfader_library_v2` entry point. Use it once per library.
///
/// ```ignore
/// openfader_plugin::export_library!(MyFormat);
/// ```
#[macro_export]
macro_rules! export_library {
    ($p:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn openfader_library_v2() -> *const $crate::library::LibraryApi {
            static API: $crate::library::LibraryApi = $crate::library::api::<$p>();
            &API
        }
    };
}

#[cfg(test)]
mod tests;
