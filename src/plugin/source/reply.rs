//! The source requests' answers as the library types: `playlists` replies as [`Listed`] rows nested into a tree,
//! `tracks` / `search` replies as [`Track`]s, and a [`Track`] back as the JSON plugins get in requests.

use super::super::Plugin;
use crate::library::{Remote, Track};
use serde_json::{Value, json};

/// Folders nested deeper than this in a plugin's playlists are not listed.
pub(super) const MAX_DEPTH: usize = 8;

/// A playlist or folder as a plugin lists it.
#[derive(Clone)]
pub(super) struct Listed {
    pub id: String,
    pub name: String,
    pub folder: bool,
    pub parent: Option<String>,
}

pub(super) enum Data {
    Lists(Vec<Listed>),
    Tracks(Vec<Track>),
}

/// The answer to a `playlists` (`lists`) or `tracks` / `search` request.
pub(super) fn read(
    plugin: &Plugin,
    source: &str,
    lists: bool,
    reply: &Value,
) -> Result<Data, String> {
    let name = &plugin.manifest.name;
    let field = if lists { "playlists" } else { "tracks" };
    let rows = reply
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{name}: the reply has no `{field}` list"))?;
    Ok(if lists {
        Data::Lists(
            rows.iter()
                .filter_map(|r| {
                    Some(Listed {
                        id: text(r, "id").filter(|id| !id.is_empty())?,
                        name: text(r, "name").unwrap_or_default(),
                        folder: r["folder"].as_bool().unwrap_or(false),
                        parent: text(r, "parent"),
                    })
                })
                .collect(),
        )
    } else {
        Data::Tracks(
            rows.iter()
                .filter_map(|r| track(plugin, source, r))
                .collect(),
        )
    })
}

/// `v[key]` as text: a string, or a number written out.
fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A track as a plugin lists it (see `docs/plugins.md`); `None` without both a file and an id (nothing to play).
fn track(plugin: &Plugin, source: &str, v: &Value) -> Option<Track> {
    let path = |k| {
        text(v, k)
            .filter(|p| !p.is_empty())
            .map(|p| plugin.dir.join(p)) // an absolute path stays as it is
    };
    let id = text(v, "id").filter(|id| !id.is_empty());
    let file = path("file");
    if file.is_none() && id.is_none() {
        return None;
    }
    let s = |k| text(v, k).unwrap_or_default();
    Some(Track {
        title: text(v, "title").or_else(|| id.clone()).unwrap_or_default(),
        artist: s("artist"),
        album: s("album"),
        genre: s("genre"),
        key: s("key"),
        bpm_x100: (v["bpm"].as_f64().unwrap_or(0.0).max(0.0) * 100.0).round() as i64,
        length_s: v["length"].as_f64().unwrap_or(0.0).max(0.0).round() as i64,
        comment: s("url"),
        artwork: path("artwork"),
        file,
        remote: id.map(|id| Remote {
            plugin: plugin.manifest.name.clone(),
            source: source.to_owned(),
            id,
        }),
        ..Track::default()
    })
}

/// A track as plugins see it in requests: `title`, `artist`, `album`, `genre`, `key`, `bpm`, `length` (seconds),
/// `file` (null without one) and `source` (`{plugin, source, id}` for a plugin track, else null).
pub fn track_json(t: &Track) -> Value {
    json!({
        "title": t.title,
        "artist": t.artist,
        "album": t.album,
        "genre": t.genre,
        "key": t.key,
        "bpm": t.bpm_x100 as f64 / 100.0,
        "length": t.length_s,
        "file": t.file.as_ref().map(|f| f.to_string_lossy()),
        "source": t.remote.as_ref().map(|r| json!({"plugin": r.plugin, "source": r.source, "id": r.id})),
    })
}

/// The playlists of one source in tree order with their depth below the source's folder; a row whose parent is not
/// listed counts as top level, rows in a parent cycle are dropped.
pub(super) fn nest(rows: &[Listed]) -> Vec<(&Listed, usize)> {
    fn walk<'a>(
        rows: &'a [Listed],
        parent: Option<&str>,
        depth: usize,
        out: &mut Vec<(&'a Listed, usize)>,
    ) {
        if depth >= MAX_DEPTH {
            return;
        }
        for r in rows.iter().filter(|r| match (&r.parent, parent) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            (Some(a), None) => !rows.iter().any(|x| x.id == *a),
            _ => false,
        }) {
            out.push((r, depth));
            if r.folder {
                walk(rows, Some(&r.id), depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(rows, None, 0, &mut out);
    out
}
