//! iTunes / Music `Library.xml` as a read-only library plugin (an Apple property list; rekordbox's Media Browser [iTunes] tree).
//!
//! Evidence (observed format of iTunes' exported library, public convention): `plist/dict` with `Tracks` (a dict keyed by
//! track id; each value a dict of `Track ID, Name, Artist, Album, Genre, Total Time` (ms), `BPM`, `Rating` (0 to 100 in
//! steps of 20), `Comments`, `Location` (`file://localhost/…` percent-encoded URI), `Bit Rate` (kbps), `Sample Rate`,
//! `Date Added` (`2015-04-03T10:20:30Z`)) and `Playlists` (an array of dicts: `Name`, `Playlist Persistent ID`,
//! `Parent Persistent ID`, `Folder`, `Master`, `Distinguished Kind`, `Playlist Items` = `Track ID` dicts).
//! Inferred: the built-in lists (`Master`, `Distinguished Kind`: Music, Movies, …) are not shown, tracks without a
//! `Location` (streamed / cloud) are dropped, and a track has no musical key (iTunes has none).
//! `Grouping` becomes `Track::label` only when asked for (the `convert_grouping` setting, "Convert iTunes Grouping to
//! rekordbox Label", X02).

use compat_rekordbox_common::{PathMap, at, location, path_map};
use openfader_plugin::library::{
    LibraryPlugin, Playlist, PlaylistRow, Probe, Source, Track, playlist_tree,
};
use openfader_plugin::markup::{Edge, Tag, tags};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// A plist value (only what the library needs).
#[derive(Debug, PartialEq)]
enum Value {
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    /// `string`, `integer`, `real`, `date`, `data` content, or `"true"` / `"false"`.
    Leaf(String),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn text(&self, key: &str) -> &str {
        match self.get(key) {
            Some(Value::Leaf(s)) => s,
            _ => "",
        }
    }

    fn num(&self, key: &str) -> i64 {
        self.text(key).trim().parse::<f64>().map_or(0, |n| n as i64)
    }

    fn flag(&self, key: &str) -> bool {
        self.text(key) == "true"
    }

    fn items(&self, key: &str) -> &[Value] {
        match self.get(key) {
            Some(Value::Array(a)) => a,
            _ => &[],
        }
    }
}

struct Parser(std::iter::Peekable<std::vec::IntoIter<Tag>>);

impl Parser {
    /// `text` is the content of a leaf element whose opening tag was just read; consumes its closing tag.
    fn leaf(&mut self, text: String) -> Result<String, String> {
        match self.0.next() {
            Some(t) if t.edge == Edge::Close => Ok(text),
            _ => Err("malformed property list".into()),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        let t = self.0.next().ok_or("unexpected end of property list")?;
        match (t.edge, t.name.as_str()) {
            // `<true/>`, `<false/>`; other empty tags read as false
            (Edge::Empty, n) => Ok(Value::Leaf(
                if n == "true" { "true" } else { "false" }.into(),
            )),
            (Edge::Open, "dict") => {
                let mut d = Vec::new();
                loop {
                    match self.0.next() {
                        Some(t) if t.edge == Edge::Close => return Ok(Value::Dict(d)),
                        Some(k) if k.edge == Edge::Open && k.name == "key" => {
                            let key = self.leaf(k.text)?;
                            d.push((key, self.value()?));
                        }
                        _ => return Err("malformed dict in property list".into()),
                    }
                }
            }
            (Edge::Open, "array") => {
                let mut a = Vec::new();
                while !matches!(self.0.peek(), Some(t) if t.edge == Edge::Close) {
                    a.push(self.value()?);
                }
                self.0.next();
                Ok(Value::Array(a))
            }
            (Edge::Open, _) => Ok(Value::Leaf(self.leaf(t.text)?)),
            _ => Err("malformed property list".into()),
        }
    }
}

fn parse_plist(text: &str) -> Result<Value, String> {
    let mut all = tags(text)?;
    let start = all
        .iter()
        .position(|t| t.edge == Edge::Open && t.name == "plist")
        .ok_or("not an iTunes library (no <plist>)")?;
    all.drain(..=start);
    Parser(all.into_iter().peekable()).value()
}

pub struct ITunes {
    /// (track id, track) in id order.
    tracks: Vec<(String, Track)>,
    rows: Vec<PlaylistRow>,
    /// Playlist persistent id → track ids in order.
    items: HashMap<String, Vec<String>>,
}

impl ITunes {
    /// `grouping_label`: iTunes "Grouping" becomes the track's `label` (rekordbox Preferences > Advanced > iTunes).
    pub fn open(path: &Path, map: &PathMap, grouping_label: bool) -> Result<Self, String> {
        let bytes = at(std::fs::read(path), path)?;
        at(
            Self::parse(&String::from_utf8_lossy(&bytes), map, grouping_label),
            path,
        )
    }

    fn parse(text: &str, map: &PathMap, grouping_label: bool) -> Result<Self, String> {
        let root = parse_plist(text)?;
        let mut tracks: Vec<(String, Track)> = Vec::new();
        if let Some(Value::Dict(all)) = root.get("Tracks") {
            for (id, t) in all {
                let Some(loc) = Some(t.text("Location"))
                    .filter(|l| !l.is_empty())
                    .and_then(|l| location(l, map))
                else {
                    continue;
                };
                tracks.push((
                    id.clone(),
                    Track {
                        title: t.text("Name").to_owned(),
                        artist: t.text("Artist").to_owned(),
                        album: t.text("Album").to_owned(),
                        genre: t.text("Genre").to_owned(),
                        label: if grouping_label {
                            t.text("Grouping").to_owned()
                        } else {
                            String::new()
                        },
                        bpm_x100: t.num("BPM") * 100,
                        length_s: (t.num("Total Time") + 500) / 1000,
                        file: Some(loc),
                        rating: (t.num("Rating") / 20).clamp(0, 5) as u8,
                        comment: t.text("Comments").to_owned(),
                        date_added: t.text("Date Added").get(..10).unwrap_or("").to_owned(),
                        bitrate: t.num("Bit Rate").max(0) as u32,
                        sample_rate: t.num("Sample Rate").max(0) as u32,
                        ..Track::default()
                    },
                ));
            }
        }
        tracks.sort_by_key(|(id, _)| id.parse::<i64>().unwrap_or(i64::MAX));
        let mut rows = Vec::new();
        let mut items = HashMap::new();
        for (order, p) in root.items("Playlists").iter().enumerate() {
            if p.flag("Master") || p.get("Distinguished Kind").is_some() {
                continue;
            }
            let id = p.text("Playlist Persistent ID").to_owned();
            let ids = p
                .items("Playlist Items")
                .iter()
                .map(|i| i.text("Track ID").to_owned())
                .collect();
            items.insert(id.clone(), ids);
            rows.push(PlaylistRow {
                id,
                name: p.text("Name").to_owned(),
                is_folder: p.flag("Folder"),
                parent: Some(p.text("Parent Persistent ID"))
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                order: order as i64,
            });
        }
        Ok(Self {
            tracks,
            rows,
            items,
        })
    }
}

impl Source for ITunes {
    fn kind(&self) -> String {
        "iTunes".into()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(playlist_tree(&self.rows))
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let Some(id) = playlist else {
            return Ok(self.tracks.iter().map(|(_, t)| t.clone()).collect());
        };
        let by_id: HashMap<&str, &Track> =
            self.tracks.iter().map(|(i, t)| (i.as_str(), t)).collect();
        Ok(self
            .items
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|i| by_id.get(i.as_str()).map(|t| (*t).clone()))
            .collect())
    }
}

pub struct ITunesPlugin;

impl LibraryPlugin for ITunesPlugin {
    fn probe(path: &Path) -> Option<Probe> {
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
        {
            return None;
        }
        let mut head = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(4096)
            .read_to_end(&mut head)
            .ok()?;
        String::from_utf8_lossy(&head)
            .contains("<plist")
            .then(|| Probe {
                score: 60,
                kind: "iTunes".into(),
            })
    }

    fn open(path: &Path, settings: &Json) -> Result<Box<dyn Source>, String> {
        let map = path_map(
            settings
                .get("path_map")
                .and_then(Json::as_str)
                .unwrap_or(""),
        )?;
        let grouping = settings
            .get("convert_grouping")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        Ok(Box::new(ITunes::open(path, &map, grouping)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Major Version</key><integer>1</integer>
<key>Tracks</key><dict>
 <key>20</key><dict><key>Track ID</key><integer>20</integer><key>Name</key><string>B &amp; B</string><key>Artist</key><string>Band</string>
  <key>Total Time</key><integer>200400</integer><key>BPM</key><integer>128</integer><key>Rating</key><integer>80</integer><key>Grouping</key><string>Lbl</string>
  <key>Date Added</key><date>2015-04-03T10:20:30Z</date><key>Location</key><string>file://localhost/Users/user/Music/a%20b.mp3</string></dict>
 <key>10</key><dict><key>Track ID</key><integer>10</integer><key>Name</key><string>Cloud</string></dict>
 <key>11</key><dict><key>Track ID</key><integer>11</integer><key>Name</key><string>Second</string><key>Comments</key><string></string>
  <key>Location</key><string>file://localhost/Users/user/Music/c.flac</string></dict>
</dict>
<key>Playlists</key><array>
 <dict><key>Name</key><string>Library</string><key>Master</key><true/><key>Playlist Persistent ID</key><string>M</string></dict>
 <dict><key>Name</key><string>Music</string><key>Distinguished Kind</key><integer>4</integer><key>Playlist Persistent ID</key><string>D</string></dict>
 <dict><key>Name</key><string>Sets</string><key>Playlist Persistent ID</key><string>F</string><key>Folder</key><true/></dict>
 <dict><key>Name</key><string>Friday</string><key>Playlist Persistent ID</key><string>P</string><key>Parent Persistent ID</key><string>F</string>
  <key>Playlist Items</key><array><dict><key>Track ID</key><integer>11</integer></dict><dict><key>Track ID</key><integer>20</integer></dict><dict><key>Track ID</key><integer>10</integer></dict></array></dict>
</array></dict></plist>"#;

    #[test]
    fn reads_tracks_and_the_playlist_tree() {
        let lib = ITunes::parse(LIB, &Vec::new(), false).unwrap();
        let all = lib.tracks(None).unwrap();
        // The cloud track (no Location) is dropped; tracks come in id order.
        assert_eq!(
            all.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["Second", "B & B"]
        );
        let t = &all[1];
        assert_eq!(
            (
                t.artist.as_str(),
                t.bpm_x100,
                t.length_s,
                t.rating,
                t.date_added.as_str()
            ),
            ("Band", 12800, 200, 4, "2015-04-03")
        );
        assert_eq!(
            t.file.as_deref(),
            Some(Path::new("/Users/user/Music/a b.mp3"))
        );

        let tree = lib.playlists().unwrap();
        assert_eq!(
            tree.iter()
                .map(|p| (p.name.as_str(), p.depth, p.is_folder))
                .collect::<Vec<_>>(),
            [("Sets", 0, true), ("Friday", 1, false)]
        );
        let friday = lib.tracks(Some("P")).unwrap();
        assert_eq!(
            friday.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["Second", "B & B"]
        ); // playlist order, the dropped track skipped
    }

    #[test]
    fn grouping_is_the_label_only_when_converted() {
        let label = |on| {
            ITunes::parse(LIB, &Vec::new(), on)
                .unwrap()
                .tracks(None)
                .unwrap()[1]
                .label
                .clone()
        };
        assert_eq!((label(false).as_str(), label(true).as_str()), ("", "Lbl"));
    }

    #[test]
    fn rejects_other_xml() {
        assert!(ITunes::parse("<DJ_PLAYLISTS/>", &Vec::new(), false).is_err());
        assert!(ITunes::parse("<plist><dict><key>Tracks</key>", &Vec::new(), false).is_err());
    }

    #[test]
    fn probe_wants_a_plist() {
        let dir = std::env::temp_dir().join(format!("openfader-itunes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Library.xml");
        std::fs::write(&p, LIB).unwrap();
        let hit = ITunesPlugin::probe(&p).unwrap();
        assert_eq!((hit.score, hit.kind.as_str()), (60, "iTunes"));
        std::fs::write(&p, "<DJ_PLAYLISTS/>").unwrap();
        assert!(ITunesPlugin::probe(&p).is_none());
        let _ = std::fs::remove_dir_all(dir); // cleanup: Windows may still hold a handle
    }
}
