//! Reads a Traktor collection: the desktop `collection.nml` (Traktor's root folder, `Documents/Native Instruments/Traktor
//! <version>/`) or the one this plugin writes on a device (`Traktor/collection.nml`). Layout and field meanings are the ones
//! [`crate::entry`] documents for the writer; the reading side adds:
//!
//! * Files: `VOLUME` + `DIR` (`/:` separators) + `FILE`. Candidates, first existing wins: the device root (when the NML sits
//!   in a folder named `Traktor`, the folder above it is the device), a drive letter (`C:/...`), `/Volumes/<VOLUME>/...`
//!   (macOS externals), then the plain absolute path (the macOS boot volume). Nothing existing: the first candidate.
//! * Grid: the `TYPE` 4 marker is the anchor, `TEMPO BPM` the tempo; beats are expanded both ways from the anchor, the anchor
//!   being bar position 1 (the writer puts the first downbeat there), up to `PLAYTIME_FLOAT` (or `PLAYTIME`; without a length
//!   only the beats up to the anchor). One tempo per track: Traktor 4's per-marker tempo is not read.
//! * Cues: `TYPE` 0, 1, 2, 3 are cues (load point and fades included), 5 a loop (`LEN` after `START`); `HOTCUE` h >= 0 is hot
//!   cue h + 1, -1 a memory cue. The name `n.n.` (Traktor's own default) and an empty one read as no comment.
//! * Playlists: ids are the names from the root joined with `/`; members are `PRIMARYKEY KEY`s matched to the `VOLUME + DIR +
//!   FILE` of an entry, in file order; keys with no entry are skipped.

use crate::entry::open_key;
use crate::xml::El;
use openfader_plugin::library::{Analysis, Beat, Cue, Playlist, Probe, Source, Track};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Source name.
pub const KIND: &str = "Traktor";

/// The NML a path names: the file itself, or the one inside a folder (`collection.nml`, `Traktor/collection.nml`).
pub fn nml_file(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        [path.join("collection.nml"), path.join(crate::FILE)]
            .into_iter()
            .find(|p| p.is_file())
    } else {
        path.is_file().then(|| path.to_path_buf())
    }
}

/// Whether the first bytes of `file` open an NML.
fn looks_like_nml(file: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 1024];
    let n = std::fs::File::open(file)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).contains("<NML")
}

pub fn probe(path: &Path) -> Option<Probe> {
    let file = nml_file(path)?;
    let named = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("nml"));
    if !(path.is_dir() || named) || !looks_like_nml(&file) {
        return None;
    }
    Some(Probe {
        score: if path.is_dir() { 70 } else { 95 },
        kind: KIND.into(),
    })
}

/// Folders of a `/:`-separated `DIR`.
pub(crate) fn parts(dir: &str) -> Vec<&str> {
    dir.split("/:").filter(|p| !p.is_empty()).collect()
}

fn resolve(device: Option<&Path>, vol: &str, dir: &str, file: &str) -> PathBuf {
    let rel: PathBuf = parts(dir).iter().chain([&file]).collect();
    let mut cands: Vec<PathBuf> = Vec::new();
    cands.extend(device.map(|d| d.join(&rel)));
    if vol.len() == 2 && vol.ends_with(':') {
        cands.push(PathBuf::from(format!("{vol}/")).join(&rel));
    }
    if !vol.is_empty() {
        cands.push(Path::new("/Volumes").join(vol).join(&rel));
    }
    cands.push(Path::new("/").join(&rel));
    let first = cands[0].clone();
    cands.into_iter().find(|p| p.exists()).unwrap_or(first)
}

pub(crate) fn key_of(e: &El) -> Option<String> {
    let l = e.child("LOCATION")?;
    Some(format!(
        "{}{}{}",
        l.get("VOLUME")?,
        l.get("DIR")?,
        l.get("FILE")?
    ))
}

/// `MUSICAL_KEY VALUE`, else the value of the Open Key text in `INFO KEY` ("10m", "3d").
fn key_value(e: &El) -> Option<u8> {
    if let Some(v) = e.child("MUSICAL_KEY").and_then(|k| k.get("VALUE")) {
        return v.parse().ok().filter(|v| *v < 24);
    }
    let open = e.child("INFO")?.get("KEY")?;
    let (n, minor) = match open.strip_suffix('m') {
        Some(n) => (n, true),
        None => (open.strip_suffix('d')?, false),
    };
    let n: u32 = n.parse().ok().filter(|n| (1..=12).contains(n))?;
    let major = (n - 1) * 7 % 12;
    Some(if minor { (major + 9) % 12 + 12 } else { major } as u8)
}

fn camelot(v: u8) -> String {
    let (minor, pc) = (v >= 12, i32::from(v % 12));
    let base = if minor { 9 } else { 0 };
    format!(
        "{}{}",
        (7 * (pc - base) + 7).rem_euclid(12) + 1,
        if minor { 'A' } else { 'B' }
    )
}

/// `2024/3/5` → `2024-03-05`.
fn iso(s: &str) -> String {
    let n: Vec<u32> = s.split('/').filter_map(|p| p.trim().parse().ok()).collect();
    match n[..] {
        [y, m, d] => format!("{y:04}-{m:02}-{d:02}"),
        _ => String::new(),
    }
}

fn num<T: std::str::FromStr>(e: Option<&El>, key: &str) -> Option<T> {
    e?.get(key)?.parse().ok()
}

fn float(e: Option<&El>, key: &str) -> f64 {
    num(e, key).unwrap_or(0.0)
}

fn track(e: &El, device: Option<&Path>, key: &str) -> Option<Track> {
    let l = e.child("LOCATION")?;
    let info = e.child("INFO");
    let text = |k: &str| info.and_then(|i| i.get(k)).unwrap_or("").to_string();
    let length = {
        let f = float(info, "PLAYTIME_FLOAT");
        if f > 0.0 {
            f
        } else {
            float(info, "PLAYTIME")
        }
    };
    Some(Track {
        title: e.get("TITLE").unwrap_or("").into(),
        artist: e.get("ARTIST").unwrap_or("").into(),
        album: e
            .child("ALBUM")
            .and_then(|a| a.get("TITLE"))
            .unwrap_or("")
            .into(),
        key: info
            .and_then(|i| i.get("KEY"))
            .map(str::to_owned)
            .or_else(|| key_value(e).map(open_key))
            .unwrap_or_default(),
        genre: text("GENRE"),
        label: text("LABEL"),
        bpm_x100: (float(e.child("TEMPO"), "BPM") * 100.0).round() as i64,
        length_s: length.round() as i64,
        file: Some(resolve(
            device,
            l.get("VOLUME").unwrap_or(""),
            l.get("DIR")?,
            l.get("FILE")?,
        )),
        analysis_ref: Some(key.into()),
        rating: (float(info, "RANKING") / 51.0).round().min(5.0) as u8,
        comment: text("COMMENT"),
        date_added: iso(&text("IMPORT_DATE")),
        bitrate: (float(info, "BITRATE") / 1000.0).round() as u32,
        ..Track::default()
    })
}

struct Nml {
    nml: El,
    tracks: Vec<Track>,
    /// Collection entry of each track, same order.
    at: Vec<usize>,
    by_key: HashMap<String, usize>,
}

pub fn open(path: &Path) -> Result<Box<dyn Source>, String> {
    let file =
        nml_file(path).ok_or_else(|| format!("{}: no Traktor collection", path.display()))?;
    let nml = crate::load(&file)?;
    let device = file
        .parent()
        .filter(|d| d.file_name().is_some_and(|n| n == "Traktor"))
        .and_then(Path::parent);
    let mut s = Nml {
        tracks: Vec::new(),
        at: Vec::new(),
        by_key: HashMap::new(),
        nml: El::new("NML"),
    };
    if let Some(coll) = nml.child("COLLECTION") {
        for (i, e) in coll
            .kids
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name == "ENTRY")
        {
            let Some(key) = key_of(e) else { continue };
            let reference = format!("{}\t{key}", file.display());
            if let Some(t) = track(e, device, &reference) {
                s.by_key.insert(key, s.tracks.len());
                s.tracks.push(t);
                s.at.push(i);
            }
        }
    }
    s.nml = nml;
    Ok(Box::new(s))
}

/// Playlists below the `SUBNODES` `nodes`, depth first, with the key list of each (`id` of a playlist, its member keys).
fn walk(nodes: &El, path: &mut Vec<String>, out: &mut Vec<(Playlist, Vec<String>)>) {
    for n in nodes.kids.iter().filter(|n| n.name == "NODE") {
        let name = n.get("NAME").unwrap_or("").to_string();
        let depth = path.len();
        path.push(name.clone());
        let id = path.join("/");
        match n.get("TYPE") {
            Some("FOLDER") => {
                out.push((
                    Playlist {
                        id,
                        name,
                        depth,
                        is_folder: true,
                    },
                    Vec::new(),
                ));
                if let Some(sub) = n.child("SUBNODES") {
                    walk(sub, path, out);
                }
            }
            Some("PLAYLIST") => {
                let keys = n
                    .child("PLAYLIST")
                    .map(|p| {
                        p.kids
                            .iter()
                            .filter_map(|e| e.child("PRIMARYKEY")?.get("KEY").map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push((
                    Playlist {
                        id,
                        name,
                        depth,
                        is_folder: false,
                    },
                    keys,
                ));
            }
            _ => {}
        }
        path.pop();
    }
}

/// Folders and playlists of an NML under its `$ROOT` folder, depth first, each with its member keys.
pub(crate) fn lists(nml: &El) -> Vec<(Playlist, Vec<String>)> {
    let mut out = Vec::new();
    let root = nml
        .child("PLAYLISTS")
        .and_then(|p| {
            p.kids
                .iter()
                .find(|n| n.get("TYPE") == Some("FOLDER") && n.get("NAME") == Some("$ROOT"))
        })
        .and_then(|r| r.child("SUBNODES"));
    if let Some(r) = root {
        walk(r, &mut Vec::new(), &mut out);
    }
    out
}

impl Nml {
    fn lists(&self) -> Vec<(Playlist, Vec<String>)> {
        lists(&self.nml)
    }
}

impl Source for Nml {
    fn kind(&self) -> String {
        KIND.into()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(self.lists().into_iter().map(|l| l.0).collect())
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let Some(id) = playlist else {
            return Ok(self.tracks.clone());
        };
        let (_, keys) = self
            .lists()
            .into_iter()
            .find(|(p, _)| p.id == id && !p.is_folder)
            .ok_or_else(|| format!("no playlist {id}"))?;
        Ok(keys
            .iter()
            .filter_map(|k| self.by_key.get(k))
            .map(|&i| self.tracks[i].clone())
            .collect())
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        let (_, key) = track.analysis_ref.as_deref()?.split_once('\t')?;
        entry_analysis(&self.nml.child("COLLECTION")?.kids[self.at[*self.by_key.get(key)?]])
    }
}

/// The analysis behind a reference from [`Source::tracks`]: loads the NML itself.
pub fn analysis_ref(reference: &str) -> Option<Analysis> {
    let (file, key) = reference.split_once('\t')?;
    let nml = crate::load(Path::new(file)).ok()?;
    let e = nml
        .child("COLLECTION")?
        .kids
        .iter()
        .find(|e| e.name == "ENTRY" && key_of(e).as_deref() == Some(key))?;
    entry_analysis(e)
}

/// More beats than this in one grid is a corrupt file, not a track.
const MAX_BEATS: f64 = 100_000.0;

pub(crate) fn entry_analysis(e: &El) -> Option<Analysis> {
    let mut a = Analysis::default();
    let mut grid = None;
    for c in e.kids.iter().filter(|k| k.name == "CUE_V2") {
        let start = float(Some(c), "START");
        let ty: u8 = num(Some(c), "TYPE").unwrap_or(0);
        if ty == 4 {
            grid.get_or_insert(start);
            continue;
        }
        let hot: i32 = num(Some(c), "HOTCUE").unwrap_or(-1);
        let len = float(Some(c), "LEN");
        let ms = |v: f64| v.max(0.0).round() as u32;
        a.cues.push(Cue {
            hot_cue: if hot >= 0 { hot as u16 + 1 } else { 0 },
            time_ms: ms(start),
            loop_end_ms: (ty == 5).then(|| ms(start + len)),
            color: None,
            comment: c
                .get("NAME")
                .filter(|n| !n.is_empty() && *n != "n.n.")
                .map(str::to_owned),
        });
    }
    let bpm = float(e.child("TEMPO"), "BPM");
    if let (Some(anchor), true) = (grid, bpm.is_finite() && bpm > 0.0) {
        let step = 60_000.0 / bpm;
        let info = e.child("INFO");
        let len_ms = {
            let f = float(info, "PLAYTIME_FLOAT");
            (if f > 0.0 { f } else { float(info, "PLAYTIME") }) * 1000.0
        };
        let before = (anchor / step + 1e-6).floor();
        let after = if len_ms > anchor {
            ((len_ms - anchor) / step).floor()
        } else {
            0.0
        };
        // NaN and infinite values fail this test too.
        if before.is_finite() && after.is_finite() && before + after < MAX_BEATS {
            let (before, after) = (before as i64, after as i64);
            a.beats = (-before..=after)
                .map(|i| Beat {
                    bar_pos: i.rem_euclid(4) as u16 + 1,
                    bpm_x100: (bpm * 100.0).round() as u16,
                    time_ms: (anchor + i as f64 * step).round() as u32,
                })
                .collect();
        }
    }
    a.key = key_value(e).map(camelot);
    Some(a).filter(|a| !a.beats.is_empty() || !a.cues.is_empty() || a.key.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::musical_key;

    #[test]
    fn key_maps_round_trip() {
        for v in 0..24u8 {
            assert_eq!(
                key_value(&El::new("E").with(El::new("INFO").attr("KEY", open_key(v)))),
                Some(v)
            );
            let c = camelot(v);
            assert_eq!(musical_key(&c), Some(v), "{c}");
        }
    }

    #[test]
    fn resolves_separators_and_dates() {
        assert_eq!(
            resolve(None, "C:", "/:Users/:me/:", "a.mp3"),
            PathBuf::from("C:/Users/me/a.mp3")
        );
        assert_eq!(iso("2024/3/5"), "2024-03-05");
    }
}
