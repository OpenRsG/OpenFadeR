//! Plugin track sources: [`Sources`] is a [`Library`] decorator that adds one top-level folder per source a plugin
//! declares (before the "Histories" folder). Children: [`reply`] (the requests' answers as library types),
//! [`fetch`] (downloading the audio of a track that has no local file).
//!
//! Ids handed out by the decorator always start with [`PREFIX`], so they never collide with the inner library's:
//! - `plugin:<plugin>:<source>`: the source's folder;
//! - `plugin:<plugin>:<source>:search`: its search entry (`search: true` sources);
//! - `plugin:<plugin>:<source>:list:<id>`: a playlist or folder the plugin listed, `<id>` as the plugin names it.
//!
//! The decorator never waits for a plugin. A listing it does not have yet is answered empty, the request runs on a
//! worker thread, and the [`Notify`] callback fires when the answer is cached (the app then re-reads the library).
//! A failed request is reported through the same callback. Requests of the protocol are in `docs/plugins.md`.

mod fetch;
mod reply;

pub use fetch::{FetchJob, fetch, fetch_async, fetch_in};
pub use reply::track_json;

use super::{Plugin, SourceDecl, registry};
use crate::library::{Library, Playlist, Track, is_history};
use parking_lot::Mutex;
use reply::{Data, nest, read};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};

pub const PREFIX: &str = "plugin:";
/// Time a playlist, track list or search request may take.
const LIST_TIMEOUT: Duration = Duration::from_secs(120);

/// Called from a worker thread: `Ok` when cached data changed (read the library again), `Err` with a message to show.
pub type Notify = Arc<dyn Fn(Result<(), String>) + Send + Sync>;

enum Kind<'a> {
    Folder,
    Search,
    List(&'a str),
}

/// `(plugin, source, what)` of an id handed out by [`Sources`]; `None` for any other id.
fn parse(id: &str) -> Option<(&str, &str, Kind<'_>)> {
    let mut it = id.strip_prefix(PREFIX)?.splitn(4, ':');
    let (plugin, source) = (it.next()?, it.next()?);
    let kind = match (it.next(), it.next()) {
        (None, _) => Kind::Folder,
        (Some("search"), None) => Kind::Search,
        (Some("list"), Some(list)) => Kind::List(list),
        _ => return None,
    };
    Some((plugin, source, kind))
}

/// Whether `id` is a playlist id of a plugin source (any kind).
pub fn is_plugin(id: &str) -> bool {
    parse(id).is_some()
}

/// Whether `id` is the search entry of a plugin source.
pub fn is_search(id: &str) -> bool {
    matches!(parse(id), Some((_, _, Kind::Search)))
}

#[derive(Default)]
struct State {
    /// Generation counter: an answer is cached only if its request is still the latest one for its key.
    next: u64,
    /// By playlist id (the source's folder id for its playlist list): the request's generation and its answer
    /// (`None` while it runs).
    cache: HashMap<String, (u64, Option<Data>)>,
}

pub struct Sources {
    inner: Box<dyn Library>,
    plugins: Vec<Plugin>,
    state: Arc<Mutex<State>>,
    notify: Notify,
}

/// `inner` with the sources of every plugin in [`registry`]; `inner` itself when no plugin has a source.
pub fn wrap(inner: Box<dyn Library>, notify: Notify) -> Box<dyn Library> {
    let r = registry();
    if r.plugins.iter().all(|p| p.manifest.sources.is_empty()) {
        return inner;
    }
    Box::new(Sources::new(inner, r.plugins.clone(), notify))
}

impl Sources {
    pub fn new(inner: Box<dyn Library>, plugins: Vec<Plugin>, notify: Notify) -> Self {
        Sources {
            inner,
            plugins,
            state: Arc::default(),
            notify,
        }
    }

    /// The plugin `plugin` if it declares the source `source`.
    fn source(&self, plugin: &str, source: &str) -> Option<&Plugin> {
        let p = self.plugins.iter().find(|p| p.manifest.name == plugin)?;
        p.manifest
            .sources
            .iter()
            .any(|s| s.id == source)
            .then_some(p)
    }

    /// What `get` makes of the cached answer for `key`; with no request made yet, starts `request` and gives `None`.
    fn lookup<T>(
        &self,
        key: &str,
        plugin: &Plugin,
        request: Value,
        get: impl FnOnce(&Data) -> Option<T>,
    ) -> Option<T> {
        let mut s = self.state.lock();
        match s.cache.get(key) {
            Some((_, data)) => data.as_ref().and_then(get),
            None => {
                self.start(&mut s, key, plugin, request);
                None
            }
        }
    }

    /// Sends `request` to `plugin` on a worker thread and caches the answer under `key`, unless a later request for
    /// `key` was made meanwhile. A failed request is not cached (the next read asks again), except a failed playlist
    /// list, which stays empty so that a broken plugin is not asked at every rebuild of the tree.
    fn start(&self, s: &mut State, key: &str, plugin: &Plugin, request: Value) {
        s.next += 1;
        let stamp = s.next;
        s.cache.insert(key.to_owned(), (stamp, None));
        let (state, notify, key) = (self.state.clone(), self.notify.clone(), key.to_owned());
        let plugin = plugin.clone();
        std::thread::spawn(move || {
            let lists = request["op"] == "playlists";
            let source = request["source"].as_str().unwrap_or_default();
            let answer = plugin
                .call(&request, LIST_TIMEOUT)
                .and_then(|reply| read(&plugin, source, lists, &reply));
            let (data, outcome) = match answer {
                Ok(d) => (Some(d), Ok(())),
                Err(e) => (lists.then(|| Data::Lists(Vec::new())), Err(e)),
            };
            let mut s = state.lock();
            if s.cache.get(&key).map(|e| e.0) != Some(stamp) {
                return;
            }
            match data {
                Some(d) => s.cache.insert(key, (stamp, Some(d))),
                None => s.cache.remove(&key),
            };
            drop(s);
            notify(outcome);
        });
    }
}

impl Library for Sources {
    fn kind(&self) -> &str {
        self.inner.kind()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let mut all = self.inner.playlists()?;
        let at = all
            .iter()
            .position(|p| is_history(&p.id))
            .unwrap_or(all.len());
        let mut mine = Vec::new();
        for plugin in &self.plugins {
            for SourceDecl { id, name, search } in &plugin.manifest.sources {
                let folder = format!("{PREFIX}{}:{id}", plugin.manifest.name);
                mine.push(Playlist {
                    id: folder.clone(),
                    name: plugin.tr(name),
                    depth: 0,
                    is_folder: true,
                });
                if *search {
                    mine.push(Playlist {
                        id: format!("{folder}:search"),
                        name: plugin.tr("Search"),
                        depth: 1,
                        is_folder: false,
                    });
                }
                let listed = self
                    .lookup(
                        &folder,
                        plugin,
                        json!({"op": "playlists", "source": id}),
                        |d| match d {
                            Data::Lists(l) => Some(l.clone()),
                            Data::Tracks(_) => None,
                        },
                    )
                    .unwrap_or_default();
                mine.extend(nest(&listed).into_iter().map(|(l, depth)| Playlist {
                    id: format!("{folder}:list:{}", l.id),
                    name: l.name.clone(),
                    depth: depth + 1,
                    is_folder: l.folder,
                }));
            }
        }
        all.splice(at..at, mine);
        Ok(all)
    }

    /// The whole library is `inner`'s (no source adds to it): its shared list, not a copy.
    fn tracks_all(&self) -> Result<std::sync::Arc<Vec<Track>>, String> {
        self.inner.tracks_all()
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let Some(id) = playlist.filter(|id| id.starts_with(PREFIX)) else {
            return self.inner.tracks(playlist);
        };
        let Some((plugin, source, kind)) = parse(id) else {
            return Err(format!("unknown playlist {id}"));
        };
        let plugin = self
            .source(plugin, source)
            .ok_or_else(|| format!("no plugin source {plugin}:{source}"))?;
        let tracks = |d: &Data| match d {
            Data::Tracks(t) => Some(t.clone()),
            Data::Lists(_) => None,
        };
        Ok(match kind {
            Kind::Folder => None,
            Kind::Search => self
                .state
                .lock()
                .cache
                .get(id)
                .and_then(|(_, d)| d.as_ref().and_then(tracks)),
            Kind::List(list) => self.lookup(
                id,
                plugin,
                json!({"op": "tracks", "source": source, "playlist": list}),
                tracks,
            ),
        }
        .unwrap_or_default())
    }

    fn search(&self, playlist: &str, query: &str) -> bool {
        let Some((plugin, source, Kind::Search)) = parse(playlist) else {
            return false;
        };
        let (Some(plugin), false) = (self.source(plugin, source), query.trim().is_empty()) else {
            return true;
        };
        let request = json!({"op": "search", "source": source, "query": query.trim()});
        self.start(&mut self.state.lock(), playlist, plugin, request);
        true
    }

    fn analysis(&self, track: &Track) -> Option<crate::analysis::Analysis> {
        self.inner.analysis(track)
    }

    fn analysis_reader(&self) -> Option<crate::library::AnalysisReader> {
        self.inner.analysis_reader()
    }
}

#[cfg(test)]
mod tests;
