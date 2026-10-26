//! `RekordboxXml`: the reader and its [`Source`] impl (evidence: see the crate docs).

use openfader_plugin::library::{
    playlist_tree, Analysis, Beat, Cue, Playlist, PlaylistRow, Source, Track,
};
use compat_rekordbox_common::{at, location, tags, Edge, PathMap};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct RekordboxXml {
    tracks: Vec<Track>,
    tree: Vec<PlaylistRow>,
    /// Playlist id → indexes into `tracks`, in playlist order.
    members: HashMap<String, Vec<usize>>,
    /// Audio file → beat grid and cues from TEMPO / POSITION_MARK.
    grids: HashMap<PathBuf, (Vec<Beat>, Vec<Cue>)>,
}

/// Beats of one TRACK from its `(inizio_s, bpm, beats_per_bar, battito)` list over `length_s`.
fn beats(tempo: &[(f64, f64, u16, u16)], length_s: f64) -> Vec<Beat> {
    let mut out = Vec::new();
    for (i, &(start, bpm, per_bar, battito)) in tempo.iter().enumerate() {
        if bpm <= 0.0 {
            continue;
        }
        let end = tempo.get(i + 1).map_or(length_s, |n| n.0);
        let step = 60.0 / bpm;
        let mut k = 0u32;
        while start + f64::from(k) * step < end {
            let pos = (battito.max(1) - 1 + k as u16) % per_bar.max(1) + 1;
            out.push(Beat {
                bar_pos: pos,
                bpm_x100: (bpm * 100.0).round() as u16,
                time_ms: ((start + f64::from(k) * step) * 1000.0).round() as u32,
            });
            k += 1;
        }
    }
    out
}

impl RekordboxXml {
    pub fn open(path: &Path, map: &PathMap) -> Result<Self, String> {
        let xml = at(std::fs::read_to_string(path), path)?;
        at(Self::parse(&xml, map), path)
    }

    fn parse(xml: &str, map: &PathMap) -> Result<Self, String> {
        let tags = tags(xml)?;
        if !tags.iter().any(|t| t.name == "DJ_PLAYLISTS") {
            return Err("not a rekordbox XML export (no <DJ_PLAYLISTS>)".into());
        }
        let (mut tracks, mut by_id, mut by_location) = (Vec::new(), HashMap::new(), HashMap::new());
        let mut in_collection = false;
        let mut tree: Vec<PlaylistRow> = Vec::new();
        // Playlist id → its `Key`s as written, and which playlists key by Location.
        let (mut keys, mut location_keyed): (HashMap<String, Vec<String>>, HashSet<String>) =
            Default::default();
        // Open NODEs, innermost last; the root folder NODE has no id of its own.
        let mut open: Vec<Option<String>> = Vec::new();
        // Per track (parallel to `tracks`): TEMPO tuples and cues; `cur` = the open TRACK.
        let mut raw: Vec<(Vec<(f64, f64, u16, u16)>, Vec<Cue>)> = Vec::new();
        let mut cur: Option<usize> = None;
        for t in tags {
            let a = |k: &str| t.attrs.get(k).map_or("", String::as_str);
            let num = |k: &str| {
                a(k).trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .unwrap_or(0.0)
            };
            match (t.name.as_str(), t.edge) {
                ("COLLECTION", e) => in_collection = e == Edge::Open,
                ("TRACK", Edge::Open | Edge::Empty) if in_collection => {
                    by_id.insert(a("TrackID").to_owned(), tracks.len());
                    by_location.insert(a("Location").to_owned(), tracks.len());
                    tracks.push(Track {
                        title: a("Name").to_owned(),
                        artist: a("Artist").to_owned(),
                        album: a("Album").to_owned(),
                        key: a("Tonality").to_owned(),
                        genre: a("Genre").to_owned(),
                        label: a("Label").to_owned(),
                        bpm_x100: (num("AverageBpm") * 100.0).round() as i64,
                        length_s: num("TotalTime") as i64,
                        file: location(a("Location"), map),
                        rating: (num("Rating") as u32 / 51).min(5) as u8,
                        comment: a("Comments").to_owned(),
                        date_added: a("DateAdded").to_owned(),
                        bitrate: num("BitRate") as u32,
                        sample_rate: num("SampleRate") as u32,
                        ..Track::default()
                    });
                    raw.push(Default::default());
                    cur = (t.edge == Edge::Open).then(|| tracks.len() - 1);
                }
                ("TRACK", Edge::Open | Edge::Empty) => {
                    if let Some(Some(id)) = open.last() {
                        keys.entry(id.clone())
                            .or_default()
                            .push(a("Key").to_owned());
                    }
                }
                ("NODE", Edge::Close) => {
                    open.pop();
                }
                ("NODE", e) => {
                    let id = (!open.is_empty()).then(|| tree.len().to_string());
                    if let Some(id) = &id {
                        tree.push(PlaylistRow {
                            id: id.clone(),
                            name: a("Name").to_owned(),
                            is_folder: a("Type") == "0",
                            parent: open.last().cloned().flatten(),
                            order: tree.len() as i64,
                        });
                        if a("KeyType") == "1" {
                            location_keyed.insert(id.clone());
                        }
                    }
                    if e == Edge::Open {
                        open.push(id);
                    }
                }
                ("TEMPO", Edge::Empty | Edge::Open) if cur.is_some() => {
                    let n = a("Metro").split_once('/').map_or("4", |p| p.0);
                    raw[cur.unwrap()].0.push((
                        num("Inizio"),
                        num("Bpm"),
                        n.trim().parse().unwrap_or(4),
                        num("Battito") as u16,
                    ));
                }
                ("POSITION_MARK", Edge::Empty | Edge::Open)
                    if cur.is_some() && matches!(a("Type"), "0" | "4") =>
                {
                    let num_i = num("Num") as i64;
                    let color = ["Red", "Green", "Blue"]
                        .iter()
                        .any(|k| t.attrs.contains_key(*k))
                        .then(|| [num("Red") as u8, num("Green") as u8, num("Blue") as u8]);
                    raw[cur.unwrap()].1.push(Cue {
                        hot_cue: (num_i + 1).max(0) as u16,
                        time_ms: (num("Start") * 1000.0).round() as u32,
                        loop_end_ms: (a("Type") == "4")
                            .then(|| (num("End") * 1000.0).round() as u32),
                        color,
                        comment: Some(a("Name").to_owned()).filter(|n| !n.is_empty()),
                    });
                }
                ("TRACK", Edge::Close) => cur = None,
                _ => {}
            }
        }
        let members = keys
            .into_iter()
            .map(|(id, ks)| {
                let index = if location_keyed.contains(&id) {
                    &by_location
                } else {
                    &by_id
                };
                (
                    id,
                    ks.iter().filter_map(|k| index.get(k).copied()).collect(),
                )
            })
            .collect();
        let grids = tracks
            .iter()
            .zip(raw)
            .filter_map(|(t, (tempo, cues))| {
                Some((t.file.clone()?, (beats(&tempo, t.length_s as f64), cues)))
            })
            .collect();
        Ok(Self {
            tracks,
            tree,
            members,
            grids,
        })
    }
}

impl Source for RekordboxXml {
    fn kind(&self) -> String {
        "rekordbox.xml".into()
    }

    /// Folders and playlists in document order.
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(playlist_tree(&self.tree))
    }

    /// All tracks by title, or one playlist's tracks in document order.
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        Ok(match playlist {
            None => {
                let mut v = self.tracks.clone();
                v.sort_by(|a, b| a.title.cmp(&b.title));
                v
            }
            Some(id) => self
                .members
                .get(id)
                .into_iter()
                .flatten()
                .map(|&i| self.tracks[i].clone())
                .collect(),
        })
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        let (beats, cues) = self.grids.get(track.file.as_ref()?)?.clone();
        Some(Analysis {
            beats,
            cues,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compat_rekordbox_common::parse_path_map;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS Version="1.0.0">
  <PRODUCT Name="rekordbox" Version="7.0.0" Company="AlphaTheta"/>
  <COLLECTION Entries="4">
    <TRACK TrackID="1" Name="Zed &amp; Co" Artist="A" Genre="House" Tonality="8A" AverageBpm="128.00" TotalTime="300"
           Location="file://localhost/home/user/Music/Zed%20%26%20Co%20%C3%A9.mp3" Rating="255" Comments="top" DateAdded="2026-01-02"
           BitRate="320" SampleRate="44100">
      <TEMPO Inizio="0.025" Bpm="128.00" Metro="4/4" Battito="1"/>
      <POSITION_MARK Name="" Type="0" Start="1.5" Num="0"/>
    </TRACK>
    <TRACK TrackID="2" Name="Alpha" Artist="B" Genre="" Tonality="Am" AverageBpm="97.5" TotalTime="61" Location="file://localhost/C:/Users/x/Music/Pack%201/a.mp3" Rating="102"/>
    <TRACK TrackID="3" Name="Mid" Artist="C" AverageBpm="140" TotalTime="10" Location="file:///tmp/m.flac" Rating="51"/>
    <TRACK TrackID="4" Name="Unrated" AverageBpm="0" TotalTime="0" Location="file://localhost/tmp/u.wav" Rating="0"/>
  </COLLECTION>
  <PLAYLISTS>
    <NODE Type="0" Name="ROOT" Count="3">
      <NODE Type="1" Name="Loose" KeyType="0" Entries="2"><TRACK Key="3"/><TRACK Key="1"/></NODE>
      <NODE Type="0" Name="Packs" Count="2">
        <NODE Type="1" Name="By location" KeyType="1" Entries="2">
          <TRACK Key="file://localhost/tmp/u.wav"/><TRACK Key="file://localhost/home/user/Music/Zed%20%26%20Co%20%C3%A9.mp3"/>
        </NODE>
        <NODE Type="0" Name="Empty folder" Count="0"/>
      </NODE>
      <NODE Type="1" Name="Dangling" KeyType="0" Entries="2"><TRACK Key="99"/><TRACK Key="2"/></NODE>
    </NODE>
  </PLAYLISTS>
</DJ_PLAYLISTS>"#;

    fn write(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openfader-xml-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rekordbox.xml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn collection_playlists_paths_and_ratings() {
        let p = write("a", XML);
        let lib = RekordboxXml::open(&p, &Vec::new()).unwrap();
        assert_eq!(lib.kind(), "rekordbox.xml");

        let all = lib.tracks(None).unwrap();
        assert_eq!(
            all.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["Alpha", "Mid", "Unrated", "Zed & Co"]
        );
        let zed = all.iter().find(|t| t.title == "Zed & Co").unwrap();
        // Percent-encoding (incl. UTF-8 and `%26`) is decoded and `file://localhost` dropped.
        assert_eq!(
            zed.file.as_deref(),
            Some(Path::new("/home/user/Music/Zed & Co é.mp3"))
        );
        assert_eq!(
            (
                zed.key.as_str(),
                zed.genre.as_str(),
                zed.comment.as_str(),
                zed.date_added.as_str()
            ),
            ("8A", "House", "top", "2026-01-02")
        );
        assert_eq!(
            (
                zed.bpm_x100,
                zed.length_s,
                zed.bitrate,
                zed.sample_rate,
                zed.analysis_ref.is_none()
            ),
            (12800, 300, 320, 44100, true)
        );
        // Rating 0/51/102/153/204/255 maps to 0 to 5 stars.
        let stars: Vec<_> = all.iter().map(|t| (t.title.as_str(), t.rating)).collect();
        assert_eq!(
            stars,
            [("Alpha", 2), ("Mid", 1), ("Unrated", 0), ("Zed & Co", 5)]
        );
        assert_eq!(all[0].bpm_x100, 9750);
        // `file:///x` and a drive path without a map.
        assert_eq!(all[1].file.as_deref(), Some(Path::new("/tmp/m.flac")));
        assert_eq!(
            all[0].file.as_deref(),
            Some(Path::new("C:/Users/x/Music/Pack 1/a.mp3"))
        );

        // The ROOT NODE is not listed; folders nest; document order is kept.
        let pls = lib.playlists().unwrap();
        let shape: Vec<_> = pls
            .iter()
            .map(|p| (p.name.as_str(), p.depth, p.is_folder))
            .collect();
        assert_eq!(
            shape,
            [
                ("Loose", 0, false),
                ("Packs", 0, true),
                ("By location", 1, false),
                ("Empty folder", 1, true),
                ("Dangling", 0, false)
            ]
        );
        let titles = |name: &str| {
            let id = &pls.iter().find(|p| p.name == name).unwrap().id;
            lib.tracks(Some(id))
                .unwrap()
                .into_iter()
                .map(|t| t.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(titles("Loose"), ["Mid", "Zed & Co"]); // playlist order, not title order
        assert_eq!(titles("By location"), ["Unrated", "Zed & Co"]); // KeyType 1
        assert_eq!(titles("Dangling"), ["Alpha"]); // unknown key dropped
        assert!(titles("Packs").is_empty() && titles("Empty folder").is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle
    }

    #[test]
    fn path_map_applies_and_garbage_is_rejected() {
        let p = write("b", XML);
        let map = parse_path_map("C:/Users/x/Music=>/home/y/Music").unwrap();
        let lib = RekordboxXml::open(&p, &map).unwrap();
        let alpha = lib.tracks(None).unwrap().remove(0);
        assert_eq!(
            alpha.file.as_deref(),
            Some(Path::new("/home/y/Music/Pack 1/a.mp3"))
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle

        let p = write("c", "<html><body/></html>");
        assert!(RekordboxXml::open(&p, &Vec::new())
            .err()
            .unwrap()
            .contains("DJ_PLAYLISTS"));
        let _ = std::fs::remove_dir_all(p.parent().unwrap()); // cleanup: Windows may still hold a handle
    }
}
