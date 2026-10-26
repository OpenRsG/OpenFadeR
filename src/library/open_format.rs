//! Our open, documented export of the Collection (`docs/library-format.md`): one JSON file (`"format": "openfader-library"`,
//! versioned) with every track, its analysis parts a collection file holds (cues, beat grid, phrases), tags and the playlist
//! tree; one CSV with a row per track; an M3U8 of every track with a file ([`super::playlist_file`]). Files are created, never
//! overwritten, and removed again when writing fails.

use super::{Track, playlist_file, snapshot::Snapshot};
use crate::analysis::{Cue, Phrases};
use crate::{paths, store};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

pub const FORMAT: &str = "openfader-library";
/// Bumped when a field changes meaning or goes away; added fields do not bump it.
pub const VERSION: u32 = 1;

/// The JSON file.
#[derive(Serialize, Deserialize)]
pub struct Doc {
    pub format: String,
    pub version: u32,
    pub tracks: Vec<Item>,
    /// Top-level folders and playlists.
    pub playlists: Vec<Node>,
}

/// One track.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Item {
    /// Unique in the file; what playlists list. `t<n>` for a Collection row.
    pub id: String,
    pub file: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub label: String,
    /// The key as Camelot notation when analysed, else as the source names it.
    pub key: String,
    /// 0..=23: `(camelot number - 1) * 2 + (1 for B)` ([`crate::analysis::key::key_id`]); `None` when `key` is no key.
    pub key_id: Option<u8>,
    pub bpm_x100: i64,
    pub length_s: i64,
    /// 0..=5 stars.
    pub rating: u8,
    pub comment: String,
    pub date_added: String,
    pub bitrate: u32,
    pub sample_rate: u32,
    /// Track colour name (Pink, Red, Orange, Yellow, Green, Aqua, Blue, Purple).
    pub color: Option<String>,
    /// `(category, tag)`.
    pub tags: Vec<(String, String)>,
    pub cues: Vec<Cue>,
    /// One row per beat: `[time_ms, bpm_x100, bar_pos]`.
    pub grid: Vec<[u32; 3]>,
    pub phrases: Option<Phrases>,
}

/// A folder or playlist.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    pub folder: bool,
    /// `Item::id`s in playlist order (empty for a folder).
    pub tracks: Vec<String>,
    pub children: Vec<Node>,
}

/// The tracks of `snap` as items; `detail` adds cues, grid and phrases.
fn items(snap: &Snapshot, detail: bool) -> Vec<Item> {
    let (cats, tagged) = snap.mytags();
    let mut tags: HashMap<usize, Vec<(String, String)>> = HashMap::new();
    for &(t, c, g) in tagged {
        if let Some((name, list)) = cats.get(c)
            && let Some(tag) = list.get(g)
        {
            tags.entry(t).or_default().push((name.clone(), tag.clone()));
        }
    }
    let mut seen = HashSet::new();
    snap.tracks()
        .iter()
        .enumerate()
        .map(|(i, (t, a))| {
            let mut id = store::key(t);
            if !seen.insert(id.clone()) {
                id = format!("{id}#{i}");
                seen.insert(id.clone());
            }
            let key = a
                .as_ref()
                .and_then(|a| a.key.clone())
                .unwrap_or_else(|| t.key.clone());
            let mut item = Item {
                id,
                file: t.file.as_ref().map(|f| f.display().to_string()),
                title: t.title.clone(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                genre: t.genre.clone(),
                label: t.label.clone(),
                key_id: crate::analysis::key::key_id(&key),
                key,
                bpm_x100: t.bpm_x100,
                length_s: t.length_s,
                rating: t.rating,
                comment: t.comment.clone(),
                date_added: t.date_added.clone(),
                bitrate: t.bitrate,
                sample_rate: t.sample_rate,
                color: usize::from(snap.color(i))
                    .checked_sub(1)
                    .and_then(|c| store::TRACK_COLORS.get(c))
                    .map(|c| c.0.to_owned()),
                tags: tags.remove(&i).unwrap_or_default(),
                ..Item::default()
            };
            if let (true, Some(a)) = (detail, a) {
                item.cues = a.cues.clone();
                item.grid = a
                    .beats
                    .iter()
                    .map(|b| [b.time_ms, u32::from(b.bpm_x100), u32::from(b.bar_pos)])
                    .collect();
                item.phrases = a.phrases.clone();
            }
            item
        })
        .collect()
}

/// The playlist tree of `snap` (its flat depth-first list nested), members by `ids`.
fn tree(snap: &Snapshot, ids: &[String]) -> Vec<Node> {
    let mut root: Vec<Node> = Vec::new();
    let mut path: Vec<usize> = Vec::new();
    for (p, members) in snap.playlists() {
        path.truncate(p.depth);
        let mut level = &mut root;
        for &i in &path {
            level = &mut level[i].children;
        }
        level.push(Node {
            name: p.name.clone(),
            folder: p.is_folder,
            tracks: members.iter().filter_map(|&m| ids.get(m).cloned()).collect(),
            children: Vec::new(),
        });
        path.push(level.len() - 1);
    }
    root
}

/// The JSON document of `snap`.
pub fn document(snap: &Snapshot) -> Doc {
    let tracks = items(snap, true);
    let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
    Doc {
        format: FORMAT.into(),
        version: VERSION,
        playlists: tree(snap, &ids),
        tracks,
    }
}

/// Appends `s` as one CSV field (RFC 4180: quoted when it holds a comma, quote or line break; quotes doubled).
fn field(out: &mut String, s: &str) {
    if s.contains([',', '"', '\n', '\r']) {
        out.push('"');
        out.push_str(&s.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(s);
    }
}

/// One row per track, `\r\n` line ends, the header first. Tags are `category: tag` joined by `; `.
pub fn csv(snap: &Snapshot) -> String {
    let mut out = String::from(
        "id,title,artist,album,genre,label,bpm,key,key_id,length_s,rating,color,date_added,bitrate,sample_rate,comment,tags,file\r\n",
    );
    for t in items(snap, false) {
        let bpm = Track {
            bpm_x100: t.bpm_x100,
            ..Track::default()
        }
        .bpm_text();
        let tags = (t.tags.iter())
            .map(|(c, g)| format!("{c}: {g}"))
            .collect::<Vec<_>>()
            .join("; ");
        let cols = [
            t.id,
            t.title,
            t.artist,
            t.album,
            t.genre,
            t.label,
            bpm,
            t.key,
            t.key_id.map(|k| k.to_string()).unwrap_or_default(),
            t.length_s.to_string(),
            t.rating.to_string(),
            t.color.unwrap_or_default(),
            t.date_added,
            t.bitrate.to_string(),
            t.sample_rate.to_string(),
            t.comment,
            tags,
            t.file.unwrap_or_default(),
        ];
        for (i, c) in cols.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            field(&mut out, c);
        }
        out.push_str("\r\n");
    }
    out
}

/// Creates `path` (never an existing file), has `fill` write it, removes it when that fails.
fn create(path: &Path, fill: impl FnOnce(&mut dyn Write) -> Result<(), String>) -> Result<(), String> {
    let file = paths::at(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path),
        path,
    )?;
    let mut w = std::io::BufWriter::new(file);
    fill(&mut w)
        .and_then(|()| paths::at(w.flush(), path))
        .inspect_err(|_| drop(std::fs::remove_file(path)))
}

/// Writes the JSON document of `snap` to the new file `path`; returns the number of tracks.
pub fn write_json(path: &Path, snap: &Snapshot) -> Result<usize, String> {
    let doc = document(snap);
    create(path, |w| paths::at(serde_json::to_writer(w, &doc), path))?;
    Ok(doc.tracks.len())
}

/// Writes the CSV of `snap` to the new file `path`; returns the number of tracks.
pub fn write_csv(path: &Path, snap: &Snapshot) -> Result<usize, String> {
    create(path, |w| paths::at(w.write_all(csv(snap).as_bytes()), path))?;
    Ok(snap.len())
}

/// Writes an M3U8 of the tracks of `snap` that have a file to the new file `path`; returns how many.
pub fn write_m3u8(path: &Path, snap: &Snapshot) -> Result<usize, String> {
    let tracks: Vec<Track> = snap.tracks().iter().map(|(t, _)| t.clone()).collect();
    playlist_file::write(path, &tracks, playlist_file::Format::M3u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Analysis, Beat};
    use crate::library::{Library, Playlist};

    struct Lib;

    impl Library for Lib {
        fn kind(&self) -> &str {
            "test"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            let p = |id: &str, depth, is_folder| Playlist {
                id: id.into(),
                name: id.into(),
                depth,
                is_folder,
            };
            Ok(vec![p("Sets", 0, true), p("Warm", 1, false), p("All", 0, false)])
        }
        fn tracks(&self, pl: Option<&str>) -> Result<Vec<Track>, String> {
            let t = |title: &str| Track {
                title: title.into(),
                artist: "A, \"B\"".into(),
                comment: "line1\nline2".into(),
                rating: 4,
                bpm_x100: 12850,
                key: "8A".into(),
                ..Track::default()
            };
            Ok(match pl {
                None | Some("All") => vec![t("one"), t("two")],
                Some("Warm") => vec![t("two")],
                _ => vec![],
            })
        }
        fn analysis(&self, t: &Track) -> Option<Analysis> {
            (t.title == "one").then(|| Analysis {
                beats: vec![Beat {
                    bar_pos: 1,
                    bpm_x100: 12850,
                    time_ms: 40,
                }],
                cues: vec![Cue {
                    hot_cue: 1,
                    time_ms: 900,
                    loop_end_ms: None,
                    color: Some([1, 2, 3]),
                    comment: None,
                }],
                ..Analysis::default()
            })
        }
        fn analysis_reader(&self) -> Option<crate::library::AnalysisReader> {
            Some(std::sync::Arc::new(|t| Lib.analysis(t)))
        }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ofr-open-format-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn json_round_trips_with_tracks_cues_grid_and_nested_playlists() {
        let snap = Snapshot::take(&Lib).unwrap();
        let d = dir("json");
        let path = d.join("c.json");
        assert_eq!(write_json(&path, &snap), Ok(2));
        assert!(write_json(&path, &snap).is_err(), "never overwrites");

        let doc: Doc = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!((doc.format.as_str(), doc.version), (FORMAT, VERSION));
        let one = doc.tracks.iter().find(|t| t.title == "one").unwrap();
        assert_eq!(one.key_id, Some(14));
        assert_eq!(one.cues[0].time_ms, 900);
        assert_eq!(one.grid, [[40, 12850, 1]]);
        assert_eq!(one.artist, "A, \"B\"");
        let ids: HashSet<_> = doc.tracks.iter().map(|t| &t.id).collect();
        assert_eq!(ids.len(), 2, "ids are unique");

        let sets = &doc.playlists[0];
        assert!(sets.folder && sets.children[0].name == "Warm");
        let two = doc.tracks.iter().find(|t| t.title == "two").unwrap();
        assert_eq!(sets.children[0].tracks, [two.id.clone()]);
        assert_eq!(doc.playlists[1].tracks.len(), 2);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn csv_quotes_commas_quotes_and_line_breaks() {
        let text = csv(&Snapshot::take(&Lib).unwrap());
        let mut lines = text.split("\r\n");
        assert!(lines.next().unwrap().starts_with("id,title,artist,"));
        let first = lines.next().unwrap();
        assert!(first.contains(r#","A, ""B""","#), "{first}");
        assert!(text.contains("\"line1\nline2\""));
        assert!(first.contains(",128.50,8A,14,"));
        let mut s = String::new();
        field(&mut s, "plain");
        field(&mut s, "a,b");
        assert_eq!(s, "plain\"a,b\"");
    }
}
