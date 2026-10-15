//! Library plugins, host side: the C ABI and the JSON ops of `openfader_plugin::library` (docs in `docs/plugins.md`). A
//! plugin declares `"library": {"lib": …, "formats": […], …}` ([`LibraryDecl`]); [`load`] opens each library once at
//! start (a library that does not load, has no entry point or speaks another ABI is reported and skipped) and keeps it
//! loaded for the rest of the process. [`open`] asks every plugin that may read a path to `probe` it and opens the best,
//! as a [`PluginLibrary`] (a [`Library`] over the plugin's handle, closed on drop). [`write_collection`] has a plugin write
//! the Collection as a file in its format. The device ops of a `"device": true` plugin are [`super::device`]'s.
//!
//! The plugin's own settings ([`super::SettingDecl`]) go with every request, and a table of host callbacks ([`host`]).
//! Every failure is a `String`. Children: [`decl`] (the manifest object), [`host`] (what plugins ask back), [`refs`]
//! (analysis refs: `<plugin>:<opaque>`, [`load_ref`], and the migration of bare ones).

mod decl;
mod host;
mod refs;

use super::{Plugin, config};
use crate::analysis::Analysis;
use crate::library::{AnalysisReader, Library, Playlist, Track};
pub use decl::{BrowserDecl, LibFile, LibraryDecl, check};
pub(super) use host::with_job;
use libloading::Library as Dylib;
use openfader_plugin::library::{
    Analysed, LIBRARY_ABI, LIBRARY_ENTRY, LibraryApi, Opened, Probe, Request, Snapshot, owned_ref,
    split_ref,
};
pub use refs::{load_ref, migrate_refs};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One loaded library: the plugin and its entry table (valid for the rest of the process).
#[derive(Clone)]
pub struct Loaded {
    pub plugin: Plugin,
    api: &'static LibraryApi,
}

impl Loaded {
    pub fn decl(&self) -> &LibraryDecl {
        self.plugin
            .manifest
            .library
            .as_ref()
            .expect("loaded plugins declare a library")
    }

    pub fn name(&self) -> &str {
        &self.plugin.manifest.name
    }

    /// The plugin's settings as the user set them.
    pub(super) fn settings(&self) -> Value {
        config::get().values(&self.plugin.manifest)
    }

    /// Whether `formats` allows `path`.
    fn may_read(&self, path: &Path) -> bool {
        let f = &self.decl().formats;
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
        f.is_empty()
            || if path.is_dir() {
                f.iter().any(|x| x == "dir")
            } else {
                ext.is_some_and(|e| f.contains(&e))
            }
    }

    /// One request, one answer: the JSON bytes cross the ABI both ways; an `{"error"}` answer is an `Err`. The plugin gets the
    /// host's callbacks ([`host`]).
    pub(super) fn call<T: DeserializeOwned>(&self, req: &impl Serialize) -> Result<T, String> {
        let name = self.name();
        let body = serde_json::to_vec(req).map_err(|e| format!("{name}: {e}"))?;
        // SAFETY: `call` and `free` come from a checked table; the buffer is read before it is freed, once; the host table is
        // a static.
        let reply = unsafe {
            let buf = (self.api.call)(body.as_ptr(), body.len(), &host::HOST);
            let v = serde_json::from_slice::<Value>(std::slice::from_raw_parts(buf.ptr, buf.len));
            (self.api.free)(buf);
            v
        }
        .map_err(|e| format!("{name}: unreadable answer ({e})"))?;
        if let Some(e) = reply.get("error").and_then(Value::as_str) {
            return Err(format!("{name}: {e}"));
        }
        serde_json::from_value(reply).map_err(|e| format!("{name}: unexpected answer ({e})"))
    }

    fn probe(&self, path: &Path) -> Result<Probe, String> {
        self.call(&Request::Probe {
            path: path.to_owned(),
        })
    }

    fn open(&self, path: &Path) -> Result<PluginLibrary, String> {
        let o: Opened = self.call(&Request::Open {
            path: path.to_owned(),
            settings: self.settings(),
        })?;
        Ok(PluginLibrary {
            lib: self.clone(),
            handle: o.handle,
            kind: o.kind,
        })
    }

    /// The analysis behind `opaque`, a track ref this plugin made (without its name); `None` when it holds none.
    fn analysis_ref(&self, opaque: &str) -> Option<Analysis> {
        self.call::<Analysed>(&Request::AnalysisRef {
            reference: opaque.to_owned(),
            settings: self.settings(),
        })
        .map_err(|e| eprintln!("plugin: {e}"))
        .ok()?
        .analysis
    }
}

/// The loaded library plugins that are still switched on, in registry order.
pub(super) fn enabled() -> impl Iterator<Item = &'static Loaded> {
    let now = super::current();
    super::registry()
        .libraries
        .iter()
        .filter(move |l| now.get(l.name()).is_some())
}

/// Every library of `plugins`, loaded; what goes wrong is added to `errors` (the plugin is skipped).
pub fn load(plugins: &[Plugin], errors: &mut Vec<String>) -> Vec<Loaded> {
    let mut out = Vec::new();
    for p in plugins {
        let Some(d) = &p.manifest.library else {
            continue;
        };
        let file = p.dir.join(d.lib.file().unwrap_or_default());
        match open_api(&file) {
            Ok(api) => out.push(Loaded {
                plugin: p.clone(),
                api,
            }),
            Err(e) => errors.push(format!("{}: {e}", p.manifest.name)),
        }
    }
    out
}

fn open_api(path: &Path) -> Result<&'static LibraryApi, String> {
    // SAFETY: loading runs the library's initialisers. A plugin is native code the user installed, like any program.
    let lib =
        unsafe { Dylib::new(path) }.map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    let api = {
        type Entry = unsafe extern "C" fn() -> *const LibraryApi;
        let entry = unsafe { lib.get::<Entry>(LIBRARY_ENTRY) }.map_err(|e| {
            format!(
                "{} has no `openfader_library_v2` entry point ({e}); it was not built against library ABI {LIBRARY_ABI} of openfader-plugin",
                path.display()
            )
        })?;
        unsafe { (*entry)().as_ref() }.ok_or("the entry point returned null")?
    };
    if api.abi != LIBRARY_ABI {
        return Err(format!(
            "built for library ABI {}, this build speaks {LIBRARY_ABI}",
            api.abi
        ));
    }
    // The table and the functions live in the library, which stays loaded for the rest of the process.
    std::mem::forget(lib);
    Ok(api)
}

/// An opened library of a plugin; the plugin's handle is closed on drop.
pub struct PluginLibrary {
    lib: Loaded,
    handle: u64,
    kind: String,
}

// SAFETY: the handle is an id the plugin looks up under its own locks; the table is immutable.
unsafe impl Send for PluginLibrary {}

#[derive(Deserialize)]
struct Playlists {
    playlists: Vec<Playlist>,
}

#[derive(Deserialize)]
struct Tracks {
    tracks: Vec<Track>,
}

#[derive(Deserialize)]
struct Started {
    started: bool,
}

/// What the plugin's open library holds for `track`. The plugin gets the track with the opaque part of its own ref; a track
/// that carries another plugin's ref has no analysis here.
fn analyse(lib: &Loaded, handle: u64, track: &Track) -> Option<Analysis> {
    let mut track = track.clone();
    if let Some(r) = track.analysis_ref.as_deref().and_then(split_ref) {
        if r.0 != lib.name() {
            return None;
        }
        track.analysis_ref = Some(r.1.to_owned());
    }
    lib.call::<Analysed>(&Request::Analysis { handle, track })
        .map_err(|e| eprintln!("plugin: {e}"))
        .ok()?
        .analysis
}

impl Library for PluginLibrary {
    fn kind(&self) -> &str {
        &self.kind
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let r: Playlists = self.lib.call(&Request::Playlists {
            handle: self.handle,
        })?;
        Ok(r.playlists)
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let mut r: Tracks = self.lib.call(&Request::Tracks {
            handle: self.handle,
            playlist: playlist.map(str::to_owned),
        })?;
        for t in &mut r.tracks {
            t.analysis_ref = t
                .analysis_ref
                .take()
                .map(|o| owned_ref(self.lib.name(), &o));
        }
        Ok(r.tracks)
    }

    fn search(&self, playlist: &str, query: &str) -> bool {
        self.lib
            .call::<Started>(&Request::Search {
                handle: self.handle,
                playlist: playlist.to_owned(),
                query: query.to_owned(),
            })
            .is_ok_and(|r| r.started)
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        analyse(&self.lib, self.handle, track)
    }

    fn analysis_reader(&self) -> Option<AnalysisReader> {
        let (lib, handle) = (self.lib.clone(), self.handle);
        Some(std::sync::Arc::new(move |t| analyse(&lib, handle, t)))
    }
}

impl Drop for PluginLibrary {
    fn drop(&mut self) {
        let _ = self.lib.call::<Value>(&Request::Close {
            handle: self.handle,
        });
    }
}

/// The library plugin that reads `path` best (the highest `probe` score above 0), opened; `None` when no enabled plugin
/// claims it. A plugin whose `probe` fails is reported on stderr and does not take part.
pub fn open(path: &Path) -> Result<Option<PluginLibrary>, String> {
    let mut best: Option<(u8, &Loaded)> = None;
    for l in enabled() {
        if !l.may_read(path) {
            continue;
        }
        match l.probe(path) {
            Ok(p) if p.score > best.map_or(0, |b| b.0) => best = Some((p.score, l)),
            Ok(_) => {}
            Err(e) => eprintln!("plugin: {e}"),
        }
    }
    best.map(|(_, l)| l.open(path)).transpose()
}

/// A plugin's menu entry: its name, the manifest's label and the library file its settings name (where the file dialog starts).
pub struct Entry {
    pub plugin: String,
    pub label: String,
    pub path: String,
    /// The plugin reads sample packs (`samples` of its manifest).
    pub samples: bool,
    /// The file extensions (and `dir`) it reads: the file dialog's filter.
    pub formats: Vec<String>,
}

fn entries(label: impl Fn(&LibraryDecl) -> Option<&String>) -> Vec<Entry> {
    enabled()
        .filter_map(|l| {
            let path = l.decl().browser.as_ref().and_then(|b| {
                l.settings()
                    .get(&b.path_setting)?
                    .as_str()
                    .map(str::to_owned)
            });
            Some(Entry {
                plugin: l.name().to_owned(),
                label: label(l.decl())?.clone(),
                path: path.unwrap_or_default(),
                samples: l.decl().samples,
                formats: l.decl().formats.clone(),
            })
        })
        .collect()
}

/// The plugins that write a collection file, for File > Export.
pub fn writers() -> Vec<Entry> {
    entries(|d| d.collection_writer.as_ref())
}

/// The plugins that read a collection file the user names, for File > Import.
pub fn readers() -> Vec<Entry> {
    entries(|d| d.collection_reader.as_ref())
}

/// A Media Browser source: the plugin, its tree label and the library file the user set.
pub struct Source {
    pub plugin: String,
    pub label: String,
    pub path: PathBuf,
}

/// The Media Browser sources of the enabled library plugins that have a library file set (and existing).
pub fn sources() -> Vec<Source> {
    enabled()
        .filter_map(|l| {
            let b = l.decl().browser.as_ref()?;
            let settings = l.settings();
            let path = PathBuf::from(settings.get(&b.path_setting)?.as_str()?);
            path.is_file().then(|| Source {
                plugin: l.name().to_owned(),
                label: b.label.clone(),
                path,
            })
        })
        .collect()
}

/// The enabled library plugin named `plugin`.
fn named(plugin: &str) -> Result<&'static Loaded, String> {
    enabled()
        .find(|l| l.name() == plugin)
        .ok_or_else(|| format!("{plugin}: not a loaded library plugin"))
}

/// Opens `path` with the plugin named `plugin` (not probing: the user chose the plugin).
pub fn open_with(plugin: &str, path: &Path) -> Result<PluginLibrary, String> {
    named(plugin)?.open(path)
}

/// Has plugin `plugin` write `snapshot` as a new file at `path` in its format; returns the number of tracks written. The
/// file is removed again when the plugin fails.
pub fn write_collection(plugin: &str, path: &Path, snapshot: &Snapshot) -> Result<usize, String> {
    #[derive(Serialize)]
    struct Write<'a> {
        op: &'static str,
        path: &'a Path,
        snapshot: &'a Snapshot,
        settings: Value,
    }
    #[derive(Deserialize)]
    struct Written {
        count: usize,
    }
    let l = named(plugin)?;
    l.call::<Written>(&Write {
        op: "write_collection",
        path,
        snapshot,
        settings: l.settings(),
    })
    .map(|w| w.count)
    .inspect_err(|_| drop(std::fs::remove_file(path)))
}

/// The file extension the collection of plugin `plugin` gets (the first of its `formats`).
pub fn extension(plugin: &str) -> Option<String> {
    named(plugin).ok()?.decl().formats.first().cloned()
}
