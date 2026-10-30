//! Reading an Engine DJ library: the `Engine Library/Database2/m.db` of Engine DJ desktop (`~/Music/Engine Library`) or of a
//! device, schema 2.18.0 to 3.x. Read only: the file is opened read-only and never changed.
//!
//! | Engine | Host |
//! |---|---|
//! | `Track.path` (relative to `Engine Library`, `../Contents/...` on a device) | `file`, made absolute; a row without a path (streaming) is skipped |
//! | `title`, `artist`, `album`, `genre`, `label`, `comment`, `bitrate`, `length` (s) | the same |
//! | `bpmAnalyzed`, else `bpm` | `bpm_x100` |
//! | `key` (Engine id, 0 = 8B, 1 = 8A, ...) | `key` in Camelot ([`perf::camelot`]) |
//! | `rating` 0 to 100 | stars, one per 20 |
//! | `dateAdded` (unix seconds) | `date_added` `YYYY-MM-DD` (UTC) |
//! | sample rate in `trackData` | `sample_rate` |
//! | `beatData`, `quickCues`, `loops` | [`Analysis`] beats and cues ([`perf::beats_from`], [`perf::cues_from`]) |
//! | `Playlist` (`parentListId`, `nextListId`), `PlaylistEntity` (`nextEntityId`) | the playlist tree and the tracks of a list |
//!
//! Chains: a row's `next...Id` names the row after it (0: the last one), so a chain starts at the row no other row names. A
//! playlist with child playlists is a folder (Engine has no folder flag). Playlists with `isPersisted = 0` are left out
//! [inferred from libdjinterop's `Playlist` column name: Engine uses it for temporary lists]. Not read: waveforms, artwork,
//! smart lists, the history and streaming databases. Layouts: see [`crate::blob`].

use crate::{blob, perf};
use openfader_plugin::library::{Analysis, Playlist, Probe, Source, Track};
use rusqlite::{params, types::ValueRef, Connection, OpenFlags, OptionalExtension, Row};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Name of the source, for the window title.
const KIND: &str = "Engine DJ";

/// Header of every SQLite file.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// The `m.db` that `path` names: the file itself (in a `Database2` folder), the `Database2` or `Engine Library` folder, or the
/// root of a device that holds an `Engine Library`. `None` when there is no such file.
pub fn locate(path: &Path) -> Option<PathBuf> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let db = if path.is_dir() {
        match name {
            "Engine Library" => path.join("Database2").join("m.db"),
            "Database2" => path.join("m.db"),
            _ => crate::database(path),
        }
    } else if name == "m.db"
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|d| d == "Database2")
    {
        path.to_path_buf()
    } else {
        return None;
    };
    db.is_file().then_some(db)
}

/// Whether `path` is an Engine library: the name and the first bytes, never the database.
pub fn probe(path: &Path) -> Option<Probe> {
    let db = locate(path)?;
    let mut head = [0u8; 16];
    std::fs::File::open(db).ok()?.read_exact(&mut head).ok()?;
    (&head == SQLITE_MAGIC).then(|| Probe {
        score: if path.is_dir() { 90 } else { 95 },
        kind: KIND.into(),
    })
}

/// Opens the library at `path` (see [`locate`]).
pub fn open(path: &Path) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(open_engine(path)?))
}

fn open_engine(path: &Path) -> Result<Engine, String> {
    let file = locate(path)
        .ok_or_else(|| format!("{}: no Engine Library/Database2/m.db here", path.display()))?;
    open_at(&file, false)
}

/// Opens the database file `file` itself (in a `Database2` folder, whatever its name: a staged `m.db.ofnew` too). `immutable`:
/// as [`crate::schema::open_immutable`], for a file on a device that must not be touched.
pub fn open_file(file: &Path, immutable: bool) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(open_at(file, immutable)?))
}

fn open_at(file: &Path, immutable: bool) -> Result<Engine, String> {
    let err = |e: rusqlite::Error| format!("{}: {e}", file.display());
    let db = if immutable {
        crate::schema::open_immutable(file)
    } else {
        Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)
    }
    .map_err(err)?;
    let (uuid, major, minor, patch): (Option<String>, i64, i64, i64) = db
        .query_row(
            "SELECT uuid, schemaVersionMajor, schemaVersionMinor, schemaVersionPatch FROM Information",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| format!("{} is not an Engine library: {e}", file.display()))?;
    if (major, minor, patch) < (2, 18, 0) || major > 3 {
        return Err(format!(
            "{} has schema version {major}.{minor}.{patch}: this plugin reads 2.18.0 to 3.x (Engine DJ 2.0 and newer)",
            file.display()
        ));
    }
    // `locate` found the file in `Engine Library/Database2`.
    let base = file
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Ok(Engine {
        db,
        uuid: uuid.unwrap_or_default(),
        base,
        file: file.to_path_buf(),
    })
}

struct Engine {
    db: Connection,
    /// `Information.uuid`: the database id a `PlaylistEntity` of this library carries.
    uuid: String,
    /// The `Engine Library` folder, which `Track.path` is relative to.
    base: PathBuf,
    /// The `m.db` that was opened.
    file: PathBuf,
}

/// The columns of [`Engine::track`]; `PerformanceData` is a table in 3.x and a view over `Track` in 2.x.
const TRACK: &str = "SELECT t.id, t.path, t.title, t.artist, t.album, t.genre, t.label, t.comment, t.bpmAnalyzed, t.bpm, \
     t.key, t.rating, t.length, t.bitrate, t.dateAdded, p.trackData \
     FROM Track t LEFT JOIN PerformanceData p ON p.trackId = t.id";

fn sql(e: rusqlite::Error) -> String {
    format!("Engine DJ library: {e}")
}

/// Column `i` as text; numbers print, anything else is empty.
fn text(r: &Row, i: usize) -> String {
    match r.get_ref(i) {
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
        Ok(ValueRef::Integer(n)) => n.to_string(),
        Ok(ValueRef::Real(f)) => f.to_string(),
        _ => String::new(),
    }
}

/// Column `i` as a number: SQLite is loosely typed, so a text that reads as one counts. `None` for NULL.
fn num(r: &Row, i: usize) -> Option<f64> {
    match r.get_ref(i) {
        Ok(ValueRef::Integer(n)) => Some(n as f64),
        Ok(ValueRef::Real(f)) => Some(f),
        Ok(ValueRef::Text(t)) => std::str::from_utf8(t).ok()?.trim().parse().ok(),
        _ => None,
    }
}

fn blob(r: &Row, i: usize) -> Option<Vec<u8>> {
    match r.get_ref(i) {
        Ok(ValueRef::Blob(b)) => Some(b.to_vec()),
        _ => None,
    }
}

/// `YYYY-MM-DD` of a `dateAdded`: unix seconds, or a text that starts with a date; empty when there is none.
fn date(r: &Row, i: usize) -> String {
    if let Some(d) = num(r, i)
        .filter(|&s| s > 0.0)
        .and_then(|s| chrono::DateTime::from_timestamp(s as i64, 0))
    {
        return d.format("%Y-%m-%d").to_string();
    }
    let t = text(r, i);
    t.get(..10)
        .filter(|d| d.as_bytes().get(4) == Some(&b'-'))
        .unwrap_or("")
        .to_owned()
}

/// The ids of one chain in order. `items` are `(id, id of the next, payload)`; a chain starts at the item no other item names
/// and ends at a next that is no item (0). Items a broken chain leaves out (a cycle, a second start) follow, by id.
fn chain<T>(items: Vec<(i64, i64, T)>) -> Vec<T> {
    let named: HashSet<i64> = items.iter().map(|i| i.1).collect();
    let mut slots: BTreeMap<i64, (i64, Option<T>)> = items
        .into_iter()
        .map(|(id, next, p)| (id, (next, Some(p))))
        .collect();
    let starts: Vec<i64> = slots
        .keys()
        .filter(|k| !named.contains(k))
        .copied()
        .collect();
    let all: Vec<i64> = slots.keys().copied().collect();
    let mut out = Vec::with_capacity(all.len());
    for start in starts.into_iter().chain(all) {
        let mut cur = start;
        while let Some((next, slot)) = slots.get_mut(&cur) {
            let Some(p) = slot.take() else { break };
            out.push(p);
            cur = *next;
        }
    }
    out
}

/// Adds the children of `parent` to `out`, depth first.
fn walk(
    kids: &HashMap<i64, Vec<(i64, String)>>,
    parent: i64,
    depth: usize,
    seen: &mut HashSet<i64>,
    out: &mut Vec<Playlist>,
) {
    for (id, name) in kids.get(&parent).into_iter().flatten() {
        if !seen.insert(*id) {
            continue;
        }
        out.push(Playlist {
            id: id.to_string(),
            name: name.clone(),
            depth,
            is_folder: kids.contains_key(id),
        });
        walk(kids, *id, depth + 1, seen, out);
    }
}

impl Engine {
    /// `Track.path` as an absolute path: relative to `Engine Library`, `..` and `.` folded away (the file may not be there).
    fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base.join(p)
        };
        let mut out = PathBuf::new();
        for c in joined.components() {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                c => out.push(c),
            }
        }
        out
    }

    /// The track of a [`TRACK`] row; `None` for one without a path (a streaming track).
    fn track(&self, r: &Row) -> Option<Track> {
        let path = text(r, 1);
        if path.is_empty() {
            return None;
        }
        let bpm = num(r, 8).filter(|&b| b > 0.0).or(num(r, 9)).unwrap_or(0.0);
        let id = num(r, 0)? as i64;
        Some(Track {
            title: text(r, 2),
            artist: text(r, 3),
            album: text(r, 4),
            genre: text(r, 5),
            label: text(r, 6),
            comment: text(r, 7),
            bpm_x100: (bpm * 100.0).round() as i64,
            key: num(r, 10)
                .and_then(|k| perf::camelot(k as i64))
                .unwrap_or_default(),
            rating: ((num(r, 11).unwrap_or(0.0).max(0.0) + 10.0) / 20.0).min(5.0) as u8,
            length_s: num(r, 12).unwrap_or(0.0) as i64,
            bitrate: num(r, 13).unwrap_or(0.0).max(0.0) as u32,
            date_added: date(r, 14),
            sample_rate: blob(r, 15)
                .as_deref()
                .and_then(blob::parse_track_data)
                .map_or(0, |(rate, _)| rate.clamp(0.0, f64::from(u32::MAX)) as u32),
            file: Some(self.resolve(&path)),
            analysis_ref: Some(format!("{id}\t{}", self.file.display())),
            ..Default::default()
        })
    }
}

/// `<id>\t<database path>`: what `Track::analysis_ref` holds, so the analysis needs no open handle.
fn parse_ref(reference: &str) -> Option<(i64, &str)> {
    let (id, db) = reference.split_once('\t')?;
    Some((id.parse().ok()?, db))
}

/// The analysis behind a reference from [`Source::tracks`]; opens the database itself.
pub fn analysis_ref(reference: &str) -> Option<Analysis> {
    let (id, db) = parse_ref(reference)?;
    open_engine(Path::new(db)).ok()?.analysis_of(id)
}

impl Source for Engine {
    fn kind(&self) -> String {
        KIND.into()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let mut st = self
            .db
            .prepare(
                "SELECT id, title, parentListId, nextListId FROM Playlist WHERE COALESCE(isPersisted, 1) <> 0",
            )
            .map_err(sql)?;
        let rows: Vec<(i64, String, i64, i64)> = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    text(r, 1),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        let known: HashSet<i64> = rows.iter().map(|r| r.0).collect();
        // A list whose parent is missing (or not persisted) sits at the top.
        let mut groups: HashMap<i64, Vec<(i64, i64, (i64, String))>> = HashMap::new();
        for (id, title, parent, next) in rows {
            let parent = if known.contains(&parent) { parent } else { 0 };
            groups
                .entry(parent)
                .or_default()
                .push((id, next, (id, title)));
        }
        let kids: HashMap<i64, Vec<(i64, String)>> =
            groups.into_iter().map(|(p, g)| (p, chain(g))).collect();
        let mut out = Vec::with_capacity(known.len());
        walk(&kids, 0, 0, &mut HashSet::new(), &mut out);
        Ok(out)
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let Some(list) = playlist else {
            let mut st = self
                .db
                .prepare(&format!("{TRACK} ORDER BY t.id"))
                .map_err(sql)?;
            let rows = st.query_map([], |r| Ok(self.track(r))).map_err(sql)?;
            return rows.filter_map(|t| t.map_err(sql).transpose()).collect();
        };
        let list: i64 = list
            .parse()
            .map_err(|_| format!("Engine DJ library: no playlist {list}"))?;
        let mut st = self
            .db
            .prepare("SELECT id, trackId, nextEntityId, databaseUuid FROM PlaylistEntity WHERE listId = ?1")
            .map_err(sql)?;
        let entities: Vec<(i64, i64, (i64, String))> = st
            .query_map([list], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    (r.get::<_, Option<i64>>(1)?.unwrap_or(0), text(r, 3)),
                ))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        let mut by_id = self
            .db
            .prepare(&format!("{TRACK} WHERE t.id = ?1"))
            .map_err(sql)?;
        // An entity of another database names the track there: find its copy by origin.
        let mut by_origin = self
            .db
            .prepare(&format!(
                "{TRACK} WHERE t.originTrackId = ?1 AND t.originDatabaseUuid = ?2"
            ))
            .map_err(sql)?;
        let mut out = Vec::new();
        for (track, uuid) in chain(entities) {
            let found = if uuid.is_empty() || uuid == self.uuid {
                by_id.query_row([track], |r| Ok(self.track(r))).optional()
            } else {
                by_origin
                    .query_row(params![track, uuid], |r| Ok(self.track(r)))
                    .optional()
            };
            out.extend(found.map_err(sql)?.flatten());
        }
        Ok(out)
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        self.analysis_of(parse_ref(track.analysis_ref.as_deref()?)?.0)
    }
}

impl Engine {
    /// The analysis of track row `id`; the path is the row's, resolved as in [`Engine::track`].
    fn analysis_of(&self, id: i64) -> Option<Analysis> {
        let (path, track_data, beat_data, quick, loops, key) = self
            .db
            .query_row(
                "SELECT t.path, p.trackData, p.beatData, p.quickCues, p.loops, t.key \
                 FROM Track t LEFT JOIN PerformanceData p ON p.trackId = t.id WHERE t.id = ?1",
                [id],
                |r| {
                    Ok((
                        text(r, 0),
                        blob(r, 1),
                        blob(r, 2),
                        blob(r, 3),
                        blob(r, 4),
                        num(r, 5),
                    ))
                },
            )
            .optional()
            .ok()??;
        let beat = beat_data.as_deref().and_then(blob::parse_beat_data);
        let data = track_data.as_deref().and_then(blob::parse_track_data);
        let rate = data
            .map(|d| d.0)
            .or(beat.as_ref().map(|b| b.0))
            .filter(|&r| r > 0.0);
        let total = beat
            .as_ref()
            .map(|b| b.1)
            .filter(|&s| s > 0.0)
            .or(data.map(|d| d.1 as f64))
            .unwrap_or(0.0);
        let mut a = Analysis {
            path: (!path.is_empty()).then(|| self.resolve(&path).to_string_lossy().into_owned()),
            key: key.and_then(|k| perf::camelot(k as i64)),
            ..Default::default()
        };
        if let Some(rate) = rate {
            if let Some((_, _, grid)) = &beat {
                a.beats = perf::beats_from(grid, rate, total);
            }
            let (slots, main) = quick
                .as_deref()
                .and_then(blob::parse_quick_cues)
                .unwrap_or_default();
            let loops = loops
                .as_deref()
                .and_then(blob::parse_loops)
                .unwrap_or_default();
            a.cues = perf::cues_from(&slots, main, &loops, rate);
        }
        (a.key.is_some() || !a.beats.is_empty() || !a.cues.is_empty()).then_some(a)
    }
}
