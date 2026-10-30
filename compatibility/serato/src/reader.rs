//! Reading a Serato library: the desktop one (`~/Music/_Serato_`) and the one on a drive root (`<root>/_Serato_`) have the same
//! layout, the one the export writes ([`database`], [`crate_file`]).
//!
//! | Field | Source |
//! |---|---|
//! | title, artist, album, genre, label, key, comment | `tsng`, `tart`, `talb`, `tgen`, `tlbl`, `tkey`, `tcom` |
//! | bpm, length, bitrate, sample rate | `tbpm`, `tlen`, `tbit`, `tsmp` texts |
//! | date added | `uadd`, else `tadd` |
//! | file | `pfil` below the drive root |
//! | cues, loops, beat grid | `Serato Markers2` / `Serato BeatGrid` of the audio file ([`crate::tag`]) |
//! | Camelot key | `tkey`, as Camelot text or as a note name |
//!
//! Serato keeps no rating in these files, so `rating` stays 0. Smart crates (`SmartCrates/*.scrate`) are not read.
//!
//! A path is relative to the drive root (the task description adds: on macOS absolute without the leading slash). The drive root
//! of a desktop library is not known from `_Serato_` alone, so a path is tried below the folder holding `_Serato_` and each of
//! its ancestors, and the first existing file wins (inferred); a missing file is placed below the folder holding `_Serato_`.
//!
//! The playlist tree is the crate file names split at `%%`, siblings in case-insensitive name order; a folder that has no crate
//! file of its own is made up from the names below it. A crate that has crates below it is listed as a folder, and its own
//! tracks are a first child of the same name whose id is the folder's id and `/` (the host reads no tracks of a folder).

use crate::tag::{self, markers};
use crate::{crate_file, database};
use openfader_plugin::library::{Analysis, Capabilities, Playlist, Probe, Source, Track};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const KIND: &str = "Serato DJ";

/// `("_Serato_", "database V2")`.
fn names() -> (&'static str, &'static str) {
    database::FILE.split_once('/').unwrap_or_default()
}

/// A `database V2` file starts with its `vrsn` record.
fn is_database(file: &Path) -> bool {
    let mut tag = [0; 4];
    File::open(file)
        .and_then(|mut f| f.read_exact(&mut tag))
        .is_ok()
        && &tag == b"vrsn"
}

/// The folder that holds `_Serato_` (the drive root of a device) and how sure we are that `path` is a library: the `_Serato_`
/// folder or the `database V2` file in it, or a folder that holds one.
fn locate(path: &Path) -> Option<(PathBuf, u8)> {
    let (dir, db) = names();
    let named = |p: &Path, n: &str| p.file_name().is_some_and(|f| f == n);
    if path.is_file() {
        let serato = path.parent()?;
        return (named(path, db) && named(serato, dir) && is_database(path))
            .then(|| (serato.parent().unwrap_or(Path::new("")).to_path_buf(), 95));
    }
    if named(path, dir) && is_database(&path.join(db)) {
        return Some((path.parent().unwrap_or(Path::new("")).to_path_buf(), 95));
    }
    is_database(&path.join(dir).join(db)).then(|| (path.to_path_buf(), 70))
}

pub fn probe(path: &Path) -> Option<Probe> {
    locate(path).map(|(_, score)| Probe {
        score,
        kind: KIND.into(),
    })
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        // Eight hot cue slots; memory cues are dropped by the export (counted in its notes), memory loops are kept as loops.
        hot_cues: Some(8),
        // The database has no rating.
        rating_max: 0,
        ..Capabilities::default()
    }
}

/// `k` as a Camelot key (`8A`): Camelot text (`8a`, `08A`) or a note name (`Am`, `F#m`, `Bb`, `C major`, `Ebmin`).
fn camelot(k: &str) -> Option<String> {
    let k = k.trim();
    if let Some(letter) = k.chars().last().filter(char::is_ascii_alphabetic) {
        let digits = &k[..k.len() - 1];
        if let (Ok(n @ 1..=12), 'A' | 'B') = (digits.parse::<u8>(), letter.to_ascii_uppercase()) {
            return Some(format!("{n}{}", letter.to_ascii_uppercase()));
        }
    }
    let mut c = k.chars();
    let note: i32 = match c.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = c.as_str();
    let mode = rest.trim_start_matches(['#', '♯', 'b', '♭']);
    let accidentals = rest[..rest.len() - mode.len()]
        .chars()
        .map(|a| if matches!(a, '#' | '♯') { 1 } else { -1 })
        .sum::<i32>();
    let mode = mode.trim().to_lowercase();
    let minor = if mode.is_empty() || mode.starts_with("maj") {
        false
    } else if mode.starts_with('m') {
        true
    } else {
        return None;
    };
    // Fifths from C major (8B) and the relative major of a minor key, three semitones up.
    let pc = (note + accidentals + if minor { 3 } else { 0 }).rem_euclid(12);
    Some(format!(
        "{}{}",
        (7 + pc * 7 % 12) % 12 + 1,
        if minor { 'A' } else { 'B' }
    ))
}

/// The path of `rel` (normalised): the first existing file below `bases`, an absolute path as it is.
fn resolve(bases: &[PathBuf], rel: &str) -> PathBuf {
    if Path::new(rel).is_absolute() {
        return rel.into();
    }
    let at = |b: &PathBuf| b.join(rel);
    bases
        .iter()
        .map(at)
        .find(|p| p.exists())
        .unwrap_or_else(|| bases.first().map_or_else(|| rel.into(), at))
}

/// The tree of the crate names (file stems): see the module doc.
fn tree(stems: &[String]) -> Vec<Playlist> {
    let have: HashSet<&str> = stems.iter().map(String::as_str).collect();
    let mut paths: Vec<Vec<&str>> = stems.iter().map(|s| s.split("%%").collect()).collect();
    paths.sort_by_cached_key(|p| p.iter().map(|n| n.to_lowercase()).collect::<Vec<_>>());
    let parents: HashSet<String> = paths
        .iter()
        .flat_map(|p| (1..p.len()).map(move |n| p[..n].join("%%")))
        .collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in &paths {
        for depth in 0..p.len() {
            let id = p[..=depth].join("%%");
            if seen.insert(id.clone()) {
                let folder = parents.contains(&id);
                // The host reads no tracks of a folder: a crate with crates below it also gets a child of its own name.
                let own = (folder && have.contains(id.as_str())).then(|| Playlist {
                    id: format!("{id}/"),
                    name: p[depth].to_string(),
                    depth: depth + 1,
                    is_folder: false,
                });
                out.push(Playlist {
                    is_folder: folder,
                    id,
                    name: p[depth].to_string(),
                    depth,
                });
                out.extend(own);
            }
        }
    }
    out
}

struct Library {
    /// The folder that holds `_Serato_`.
    root: PathBuf,
    /// `root` and its ancestors, for [`resolve`].
    bases: Vec<PathBuf>,
    tracks: Vec<Track>,
    /// Normalised path → index in `tracks`.
    at: HashMap<String, usize>,
}

pub fn open(path: &Path) -> Result<Box<dyn Source>, String> {
    let (root, _) =
        locate(path).ok_or_else(|| format!("{}: not a Serato library", path.display()))?;
    let file = root.join(database::FILE);
    let buf = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let bases: Vec<PathBuf> = root.ancestors().map(Path::to_path_buf).collect();
    let (mut tracks, mut at) = (Vec::new(), HashMap::new());
    for (key, mut t) in database::read(&buf).map_err(|e| format!("{}: {e}", file.display()))? {
        if at.contains_key(&key) {
            continue;
        }
        let file = resolve(&bases, &key);
        t.analysis_ref = Some(reference(&file, t.length_s, &t.key));
        t.file = Some(file);
        at.insert(key, tracks.len());
        tracks.push(t);
    }
    Ok(Box::new(Library {
        root,
        bases,
        tracks,
        at,
    }))
}

impl Source for Library {
    fn kind(&self) -> String {
        KIND.into()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let stems: Vec<String> = fs::read_dir(self.root.join(crate_file::DIR))
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let p = e.ok()?.path();
                let crate_ext = p.extension()?.to_str()?.eq_ignore_ascii_case("crate");
                let stem = p.file_stem()?.to_str()?;
                (crate_ext && !stem.starts_with('.')).then(|| stem.to_string())
            })
            .collect();
        // A folder's own crate may be the empty one that shows the nesting: no child for that.
        let mut rows = tree(&stems);
        rows.retain(|p| {
            !p.id.ends_with('/') || self.tracks(Some(&p.id)).is_ok_and(|t| !t.is_empty())
        });
        Ok(rows)
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let Some(id) = playlist else {
            return Ok(self.tracks.clone());
        };
        // `A/` is the child of folder `A` that holds the tracks of the crate `A` itself.
        let id = id.strip_suffix('/').unwrap_or(id);
        if id.is_empty() || id.contains(['/', '\\']) {
            return Err(format!("no crate {id:?}"));
        }
        let file = self.root.join(crate_file::DIR).join(format!("{id}.crate"));
        let buf = match fs::read(&file) {
            Ok(b) => b,
            // A folder made up from the names below it has no file.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(format!("{}: {e}", file.display())),
        };
        let members = crate_file::paths(&buf).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(members
            .iter()
            .map(|p| {
                let key = database::norm(p);
                match self.at.get(&key) {
                    Some(&i) => self.tracks[i].clone(),
                    // A member the database does not list: the crate still names its file.
                    None => {
                        let file = resolve(&self.bases, &key);
                        Track {
                            title: Path::new(&key)
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            analysis_ref: Some(reference(&file, 0, "")),
                            file: Some(file),
                            ..Track::default()
                        }
                    }
                }
            })
            .collect())
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        analyse(track.file.as_deref()?, track.length_s, &track.key)
    }
}

/// What `Track::analysis_ref` holds: the audio file, whose tags are the analysis, with the length and key the database holds
/// (`<length s>\t<key>\t<file>`), so the analysis needs no open library.
fn reference(file: &Path, length_s: i64, key: &str) -> String {
    format!("{length_s}\t{key}\t{}", file.display())
}

/// The analysis behind a [`reference`].
pub fn analysis_ref(reference: &str) -> Option<Analysis> {
    let mut p = reference.splitn(3, '\t');
    let (length_s, key, file) = (p.next()?.parse().ok()?, p.next()?, p.next()?);
    analyse(Path::new(file), length_s, key)
}

/// Cues, grid and key of `file`: its tags, the database's `length_s` bounding the grid and `key` as the database spells it.
fn analyse(file: &Path, length_s: i64, key: &str) -> Option<Analysis> {
    let stored = tag::read(file).ok()?;
    let end_ms = u32::try_from(length_s.max(0).saturating_mul(1000)).unwrap_or(u32::MAX);
    let cues = stored
        .markers2
        .as_deref()
        .map(markers::cues)
        .unwrap_or_default();
    let beats = stored
        .grid
        .as_deref()
        .map(|g| markers::grid_beats(g, end_ms))
        .unwrap_or_default();
    let key = camelot(key);
    if cues.is_empty() && beats.is_empty() && key.is_none() {
        return None;
    }
    Some(Analysis {
        path: Some(file.to_string_lossy().into_owned()),
        beats,
        cues,
        key,
        ..Analysis::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_in_any_notation_become_camelot() {
        let k = |s| camelot(s).unwrap_or_default();
        assert_eq!(
            [k("Am"), k("C"), k("F#m"), k("Bb"), k("Abm"), k("G♭ major")],
            ["8A", "8B", "11A", "6B", "1A", "2B"]
        );
        assert_eq!(
            [k("08a"), k("12B"), k("Dm"), k("D")],
            ["8A", "12B", "7A", "10B"]
        );
        assert_eq!(camelot("13A"), None);
        assert_eq!(camelot("x"), None);
        assert_eq!(camelot(""), None);
    }

    #[test]
    fn crate_names_make_a_depth_first_tree_with_made_up_folders() {
        let stems: Vec<String> = ["b", "A%%x%%deep", "A%%y", "aa", "C%%z"]
            .map(String::from)
            .into();
        let t: Vec<(String, usize, bool)> = tree(&stems)
            .into_iter()
            .map(|p| (p.id, p.depth, p.is_folder))
            .collect();
        let row = |id: &str, depth, folder| (id.to_string(), depth, folder);
        assert_eq!(
            t,
            [
                row("A", 0, true),
                row("A%%x", 1, true),
                row("A%%x%%deep", 2, false),
                row("A%%y", 1, false),
                row("aa", 0, false),
                row("b", 0, false),
                row("C", 0, true),
                row("C%%z", 1, false),
            ]
        );
    }
}
