//! Downloading a plugin track: the plugin's `fetch` request writes the audio into a per-source folder, and a finished
//! download found there is reused without asking the plugin again.

use super::super::{Registry, registry};
use crate::library::Remote;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Time a download may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(900);
/// Extensions of files a plugin may leave in the download folder while it is still writing them.
const UNFINISHED: [&str; 3] = ["part", "tmp", "ytdl"];

/// `id` as a file name: ASCII letters, digits, `-` and `_` stay, every other byte becomes `%XX`, so two ids never
/// share a name.
pub(super) fn file_name(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The finished download of `name` in `dir`: a non-empty file whose name without extension is `name`.
pub(super) fn downloaded(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem().is_some_and(|s| s == name)
                && !crate::paths::has_extension(p, &UNFINISHED)
                && p.metadata().is_ok_and(|m| m.is_file() && m.len() > 0)
        })
}

/// The audio file of a plugin track: the earlier download if there is one, else what the plugin's `fetch` request
/// writes into `<data dir>/plugins/<plugin>/<source>/`. Blocks until the download is done: call it from a worker.
pub fn fetch(r: &Remote) -> Result<PathBuf, String> {
    let base = crate::paths::data_dir().ok_or("set HOME or XDG_DATA_HOME to download")?;
    fetch_in(registry(), &base.join("plugins"), r)
}

/// [`fetch`] with the plugin registry and the download root given.
pub fn fetch_in(registry: &Registry, root: &Path, r: &Remote) -> Result<PathBuf, String> {
    let plugin = registry
        .get(&r.plugin)
        .ok_or_else(|| format!("plugin {} is not installed", r.plugin))?;
    let dir = root.join(&r.plugin).join(&r.source);
    let name = file_name(&r.id);
    if let Some(done) = downloaded(&dir, &name) {
        return Ok(done);
    }
    crate::paths::at(std::fs::create_dir_all(&dir), &dir)?;
    let reply = plugin.call(
        &json!({"op": "fetch", "source": r.source, "id": r.id, "dir": dir.to_string_lossy(), "name": name}),
        FETCH_TIMEOUT,
    )?;
    let file = reply["file"]
        .as_str()
        .map(|f| dir.join(f)) // an absolute path stays as it is
        .ok_or_else(|| format!("{}: the reply has no `file`", r.plugin))?;
    if !file.is_file() {
        return Err(format!("{}: {} was not written", r.plugin, file.display()));
    }
    Ok(file)
}
