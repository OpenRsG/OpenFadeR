//! Cover art (base): finding a picture for a track, keeping it, and telling which picture a track shows.
//!
//! A plugin with `"artwork": true` answers `{"op": "artwork", "artist", "album", "title", "duration"}` (`duration` in whole
//! seconds, 0 when unknown) with `{"candidates": [{"url": …, "thumb": …, "data": …, "size": …, "source": …}]}`
//! ([`Candidate::parse`], documented in `docs/plugins.md`). [`search`] asks every enabled artwork plugin in turn;
//! [`Fetch::missing`] does that for a whole list and keeps the first picture each album gets; the app's picker
//! lets the user choose and calls [`save`].
//!
//! Where a picture comes from, in the order [`resolve`] tries: the cover kept in the store for the track's album (or
//! the track itself, [`Store::artwork_key`]), the artwork the library lists for it, and a folder image beside the file
//! ([`folder`]: no network). Kept pictures are copies in `<data dir>/artwork/` named after their key ([`save`]), so
//! a moved or deleted source file does not lose them. With `embed` a picture is also written into the audio file's tag
//! ([`crate::analysis::tag::write_cover`]).
//!
//! Politeness: a plugin is asked one request at a time, [`Gate::polite`] keeps [`GAP`] between two requests to one plugin,
//! downloads (one after the other) carry a user agent naming the app, and a lookup every plugin answered with nothing is
//! remembered as a miss ([`Store::set_artwork_miss`]) so [`Fetch::missing`] does not ask again for 30 days. A plugin that
//! failed (no network) is never remembered as a miss.
//!
//! Children: [`folder`] (images beside the file), [`image`] (what a picture is: type, download, base64).

pub mod folder;
pub mod image;

use crate::{analysis::cache::fnv1a, library::Track, paths::at, plugin::Plugin, store::Store};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, Instant},
};

/// Time between two requests to one plugin.
pub const GAP: Duration = Duration::from_millis(1100);
/// A plugin may need several requests of its own (a search, then each hit's pictures).
const ASK_TIMEOUT: Duration = Duration::from_secs(90);
/// Candidates taken from one plugin's answer.
const PER_PLUGIN: usize = 8;

/// What a plugin is asked about: a track's tags.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub artist: String,
    pub album: String,
    pub title: String,
    /// Whole seconds; 0 when unknown.
    pub duration: u32,
}

impl Query {
    pub fn of(t: &Track) -> Self {
        Self {
            artist: t.artist.trim().to_owned(),
            album: t.album.trim().to_owned(),
            title: t.title.trim().to_owned(),
            duration: t.length_s.clamp(0, i64::from(u32::MAX)) as u32,
        }
    }
}

/// Where a candidate's picture is.
#[derive(Clone, Debug, PartialEq)]
pub enum Art {
    /// A web address (`http` or `https`).
    Url(String),
    /// The picture itself.
    Bytes(Vec<u8>),
}

/// One picture a plugin offers for a track.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// The plugin that offered it.
    pub plugin: String,
    /// Where it comes from, as shown ("MusicBrainz", "iTunes"): the plugin's `source`, else its name.
    pub source: String,
    /// Pixels on the longer side; 0 when the plugin does not say.
    pub size: u32,
    pub art: Art,
    /// A small picture to show in the picker instead of the whole one (an address).
    pub thumb: Option<String>,
}

impl Candidate {
    /// The candidates of a plugin's reply: `url` (an `http` / `https` address) or `data` (base64 bytes), `thumb` (an
    /// address), `size` (pixels, longer side) and `source` (label). Entries with neither a usable `url` nor `data` are
    /// left out; at most [`PER_PLUGIN`] are taken.
    pub fn parse(plugin: &str, reply: &Value) -> Vec<Self> {
        let web = |v: &Value| {
            v.as_str()
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                .map(str::to_owned)
        };
        reply["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let art = match (web(&c["url"]), c["data"].as_str().and_then(image::base64)) {
                    (Some(u), _) => Art::Url(u),
                    (None, Some(b)) if !b.is_empty() => Art::Bytes(b),
                    _ => return None,
                };
                Some(Self {
                    plugin: plugin.to_owned(),
                    source: c["source"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or(plugin)
                        .to_owned(),
                    size: c["size"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
                    art,
                    thumb: web(&c["thumb"]),
                })
            })
            .take(PER_PLUGIN)
            .collect()
    }

    /// The picture's bytes: the plugin's own, or a download; the small `thumb` instead when asked for and the plugin gave one.
    pub fn bytes(&self, thumb: bool) -> Result<Vec<u8>, String> {
        let url = match (&self.art, thumb.then(|| self.thumb.as_ref()).flatten()) {
            (Art::Bytes(b), _) => return Ok(b.clone()),
            (_, Some(t)) => t,
            (Art::Url(u), None) => u,
        };
        image::download(url)
    }
}

/// Keeps [`GAP`] between the requests made for one plugin, whichever thread makes them.
pub struct Gate {
    gap: Duration,
    next: Mutex<HashMap<String, Instant>>,
}

impl Gate {
    pub fn new(gap: Duration) -> Self {
        Self {
            gap,
            next: Mutex::default(),
        }
    }

    /// The gate every lookup of the app goes through ([`GAP`]).
    pub fn polite() -> &'static Gate {
        static POLITE: LazyLock<Gate> = LazyLock::new(|| Gate::new(GAP));
        &POLITE
    }

    /// Returns when `plugin` may be asked again; the caller's turn counts as made.
    pub fn wait(&self, plugin: &str) {
        let at = {
            let mut next = self.next.lock();
            let at = next.get(plugin).copied().unwrap_or_else(Instant::now);
            next.insert(plugin.to_owned(), at.max(Instant::now()) + self.gap);
            at
        };
        std::thread::sleep(at.saturating_duration_since(Instant::now()));
    }
}

/// What [`search`] found.
#[derive(Default)]
pub struct Found {
    /// In the order of the plugins, each plugin's own order inside.
    pub candidates: Vec<Candidate>,
    /// A message per plugin that failed.
    pub errors: Vec<String>,
    /// Plugins asked.
    pub asked: usize,
}

/// Asks each of `plugins` (the ones with `artwork`) for pictures, one after the other, as [`Gate`] allows.
pub fn search<'a>(plugins: impl IntoIterator<Item = &'a Plugin>, q: &Query, gate: &Gate) -> Found {
    let mut found = Found::default();
    for p in plugins.into_iter().filter(|p| p.manifest.artwork) {
        gate.wait(&p.manifest.name);
        found.asked += 1;
        let request = json!({"op": "artwork", "artist": q.artist, "album": q.album, "title": q.title, "duration": q.duration});
        match p.call(&request, ASK_TIMEOUT) {
            Ok(reply) => found
                .candidates
                .extend(Candidate::parse(&p.manifest.name, &reply)),
            Err(e) => found.errors.push(e),
        }
    }
    found
}

/// The folder the pictures are kept in.
pub fn cache_dir() -> Option<PathBuf> {
    crate::paths::data_dir().map(|d| d.join("artwork"))
}

/// Keeps `bytes` as the cover of `t` (its album, see [`Store::artwork_key`]): a file in `dir` named after the key and the
/// picture, so a cover chosen later does not overwrite the earlier one and undo can bring it back; the store records the new
/// one (one undo step). Returns the file. ponytail: replaced covers stay in the folder (a few hundred KB each); add a sweep
/// of files no store row names if that ever matters.
pub fn save(store: &Store, dir: &Path, t: &Track, bytes: &[u8]) -> Result<PathBuf, String> {
    let ext = image::kind(bytes)
        .ok_or("this is not a picture the app can show (JPEG, PNG, GIF or WebP)")?
        .ext;
    let key = store.artwork_key(t);
    let stem = format!(
        "{:016x}-{:016x}",
        fnv1a(key.bytes()),
        fnv1a(bytes.iter().copied())
    );
    at(std::fs::create_dir_all(dir), dir)?;
    let (path, tmp) = (
        dir.join(format!("{stem}.{ext}")),
        dir.join(format!("{stem}.part")),
    );
    at(std::fs::write(&tmp, bytes), &tmp)?;
    at(std::fs::rename(&tmp, &path), &path)?;
    store.action("Cover art", || store.set_artwork(&key, &path))?;
    Ok(path)
}

/// The picture `t` shows: its kept cover, else the artwork its library lists, else a folder image beside its file
/// ([`folder::of`]). Only files that exist count.
pub fn resolve(store: &Store, t: &Track) -> Option<PathBuf> {
    let kept = store.artwork(&store.artwork_key(t)).ok().flatten();
    kept.into_iter()
        .chain(t.artwork.clone())
        .find(|p| p.is_file())
        .or_else(|| folder::of(t.file.as_deref()?))
}

/// Writes `bytes` into `file`'s tag as its front cover ([`crate::analysis::tag::write_cover`]); `Ok(false)` = it held that
/// already. Only JPEG and PNG pictures go into a tag.
pub fn embed(file: &Path, bytes: &[u8]) -> Result<bool, String> {
    match image::kind(bytes) {
        Some(k) if matches!(k.ext, "jpg" | "png") => {
            crate::analysis::tag::write_cover(file, bytes, k.mime)
        }
        _ => Err("only JPEG and PNG pictures can be written into a tag".into()),
    }
}

/// What [`Fetch::missing`] did.
#[derive(Default, Debug, PartialEq)]
pub struct Fetched {
    /// Albums (or tracks without one) that got a cover.
    pub found: usize,
    /// Albums nothing offered a cover for (remembered).
    pub missed: usize,
    /// Albums skipped: they show a picture already, or a recent lookup found nothing.
    pub skipped: usize,
    /// Albums where something went wrong (a plugin failed, a download failed): not remembered as misses.
    pub failed: usize,
    /// The first of those messages.
    pub error: Option<String>,
}

/// A run of lookups: the plugins to ask, the gate they are asked through, and where covers are kept.
pub struct Fetch<'a> {
    pub store: &'a Store,
    pub plugins: Vec<Plugin>,
    pub gate: &'a Gate,
    pub dir: &'a Path,
}

impl Fetch<'_> {
    /// Gives every album of `tracks` that shows no picture ([`resolve`]) and was not looked up in vain lately the first
    /// picture the plugins offer for it, kept as [`save`] does. `progress(done, total)` runs before each lookup and ends
    /// the run by returning `false`; `embed(track, bytes)` runs for each track of a found album.
    pub fn missing(
        &self,
        tracks: &[Track],
        progress: &mut dyn FnMut(usize, usize) -> bool,
        embed: &mut dyn FnMut(&Track, &[u8]),
    ) -> Fetched {
        let mut out = Fetched::default();
        // key -> index in `groups`, or None for an album that is not wanted.
        let mut seen: HashMap<String, Option<usize>> = HashMap::new();
        let mut groups: Vec<(String, Vec<&Track>)> = Vec::new();
        for t in tracks {
            let key = self.store.artwork_key(t);
            match seen.get(&key) {
                Some(Some(i)) => groups[*i].1.push(t),
                Some(None) => {}
                None => {
                    let wanted = resolve(self.store, t).is_none()
                        && !self.store.artwork_missed(&key).unwrap_or(false);
                    seen.insert(key.clone(), wanted.then_some(groups.len()));
                    if wanted {
                        groups.push((key, vec![t]));
                    } else {
                        out.skipped += 1;
                    }
                }
            }
        }
        for (n, (key, group)) in groups.iter().enumerate() {
            if !progress(n, groups.len()) {
                break;
            }
            let found = search(&self.plugins, &Query::of(group[0]), self.gate);
            let mut error = found.errors.first().cloned();
            let bytes = found.candidates.iter().find_map(|c| match c.bytes(false) {
                Ok(b) if image::kind(&b).is_some() => Some(b),
                Ok(_) => {
                    error.get_or_insert(format!("{}: a candidate is not a picture", c.plugin));
                    None
                }
                Err(e) => {
                    error.get_or_insert(e);
                    None
                }
            });
            match (bytes, error) {
                (Some(b), _) => match save(self.store, self.dir, group[0], &b) {
                    Ok(_) => {
                        out.found += 1;
                        group.iter().for_each(|t| embed(t, &b));
                    }
                    Err(e) => {
                        out.failed += 1;
                        out.error.get_or_insert(e);
                    }
                },
                (None, None) if found.asked > 0 => {
                    out.missed += 1;
                    let _ = self.store.set_artwork_miss(key);
                }
                (None, e) => {
                    out.failed += 1;
                    out.error.get_or_insert(
                        e.unwrap_or_else(|| "no artwork plugin is switched on".into()),
                    );
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
