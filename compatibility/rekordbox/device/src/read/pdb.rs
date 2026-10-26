//! The Device Library: `PIONEER/rekordbox/export.pdb` (DeviceSQL), read-only. The byte readers are
//! [`compat_rekordbox_common::pdb`], whose doc cites the layout; the table layouts below are from the same spec
//! (`rekordbox_pdb.ksy`). History playlists: page types 11 (`history_playlists`: id u4, name string) and 12
//! (`history_entries`: track_id, playlist_id, entry_index u4); they carry no folders or order field.

use super::{PDB, analysis};
use compat_rekordbox_common::list::{add_histories, analysis_ref, existing, history_raw, under};
use compat_rekordbox_common::pdb::{rows, string_at, u16_at, u32_at};
use openfader_plugin::library::{Analysis, Playlist, PlaylistRow, Source, Track, playlist_tree};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const T_TRACKS: u32 = 0;
const T_GENRES: u32 = 1;
const T_ARTISTS: u32 = 2;
const T_ALBUMS: u32 = 3;
const T_KEYS: u32 = 5;
const T_PLAYLIST_TREE: u32 = 7;
const T_PLAYLIST_ENTRIES: u32 = 8;
const T_HISTORY_PLAYLISTS: u32 = 11;
const T_HISTORY_ENTRIES: u32 = 12;
const T_ARTWORK: u32 = 13;

struct TrackRow {
    id: u32,
    artist_id: u32,
    album_id: u32,
    key_id: u32,
    genre_id: u32,
    artwork_id: u32,
    title: String,
    bpm_x100: i64,
    length_s: i64,
    file: String,
    anlz: String,
    comment: String,
    date_added: String,
    rating: u8,
    bitrate: u32,
    sample_rate: u32,
}

pub struct Export {
    root: PathBuf,
    tracks: Vec<TrackRow>,
    artists: HashMap<u32, String>,
    albums: HashMap<u32, String>,
    keys: HashMap<u32, String>,
    genres: HashMap<u32, String>,
    /// artwork id → path relative to the USB root (spec: artwork_row).
    artwork: HashMap<u32, String>,
    tree: Vec<PlaylistRow>,
    /// (playlist_id, entry_index, track_id)
    entries: Vec<(u32, u32, u32)>,
    history: Vec<PlaylistRow>,
    /// Same shape as `entries`, for the history playlists.
    history_entries: Vec<(u32, u32, u32)>,
}

impl Export {
    /// Reads `<usb_root>/PIONEER/rekordbox/export.pdb`.
    pub fn open(usb_root: &Path) -> Result<Export, String> {
        let p = usb_root.join(PDB);
        let d = compat_rekordbox_common::at(std::fs::read(&p), &p)?;
        Self::parse(&d, usb_root)
    }

    fn parse(d: &[u8], root: &Path) -> Result<Export, String> {
        let mut tracks = Vec::new();
        for r in rows(d, T_TRACKS)? {
            // Fixed part is 0x5e bytes, then 21 u16 string offsets (spec `ofs_strings`):
            // date_added #10, analyze_path #14, comment #16, title #17, file_path #20.
            let s = |i: usize| -> Result<String, String> {
                string_at(d, r + u16_at(d, r + 0x5e + 2 * i)? as usize)
            };
            tracks.push(TrackRow {
                id: u32_at(d, r + 0x48)?,
                artist_id: u32_at(d, r + 0x44)?,
                album_id: u32_at(d, r + 0x40)?,
                key_id: u32_at(d, r + 0x20)?,
                genre_id: u32_at(d, r + 0x3c)?,
                artwork_id: u32_at(d, r + 0x1c)?,
                bpm_x100: u32_at(d, r + 0x38)? as i64,
                length_s: u16_at(d, r + 0x54)? as i64,
                rating: *d.get(r + 0x59).ok_or("pdb: truncated track")?,
                bitrate: u32_at(d, r + 0x30)?,
                sample_rate: u32_at(d, r + 0x08)?,
                title: s(17)?,
                anlz: s(14)?,
                file: s(20)?,
                comment: s(16)?,
                date_added: s(10)?,
            });
        }
        let mut artists = HashMap::new();
        for r in rows(d, T_ARTISTS)? {
            let ofs = if u16_at(d, r)? & 4 != 0 {
                u16_at(d, r + 0x0a)? as usize
            } else {
                *d.get(r + 9).ok_or("pdb: truncated artist")? as usize
            };
            artists.insert(u32_at(d, r + 4)?, string_at(d, r + ofs)?);
        }
        // Album row (spec album_row): subtype u2, index_shift u2, 0 u4, artist_id u4, id u4 @0xc, 0 u4, 0x03, ofs_name_near u1 @0x15,
        // ofs_name_far u2 @0x16 used when the subtype has bit 2.
        let mut albums = HashMap::new();
        for r in rows(d, T_ALBUMS)? {
            let ofs = if u16_at(d, r)? & 4 != 0 {
                u16_at(d, r + 0x16)? as usize
            } else {
                *d.get(r + 0x15).ok_or("pdb: truncated album")? as usize
            };
            albums.insert(u32_at(d, r + 0x0c)?, string_at(d, r + ofs)?);
        }
        // Genre row: id u4, name string inline. Key row: id u4, id2 u4, name string inline (spec).
        let mut genres = HashMap::new();
        for r in rows(d, T_GENRES)? {
            genres.insert(u32_at(d, r)?, string_at(d, r + 4)?);
        }
        let mut keys = HashMap::new();
        for r in rows(d, T_KEYS)? {
            keys.insert(u32_at(d, r)?, string_at(d, r + 8)?);
        }
        let mut artwork = HashMap::new();
        for r in rows(d, T_ARTWORK)? {
            artwork.insert(u32_at(d, r)?, string_at(d, r + 4)?);
        }
        let mut tree = Vec::new();
        for r in rows(d, T_PLAYLIST_TREE)? {
            let parent = u32_at(d, r)?; // 0 = top level
            tree.push(PlaylistRow {
                id: u32_at(d, r + 12)?.to_string(),
                name: string_at(d, r + 20)?,
                is_folder: u32_at(d, r + 16)? != 0,
                parent: (parent != 0).then(|| parent.to_string()),
                order: u32_at(d, r + 8)? as i64,
            });
        }
        let mut entries = Vec::new();
        for r in rows(d, T_PLAYLIST_ENTRIES)? {
            entries.push((u32_at(d, r + 8)?, u32_at(d, r)?, u32_at(d, r + 4)?));
        }
        // History playlist row: id u4, name string inline. History entry row: track_id u4,
        // playlist_id u4, entry_index u4 (spec: history_playlist_row, history_entry_row). The
        // table has no order field, so playlists sort by id; no folders exist.
        let mut history = Vec::new();
        for r in rows(d, T_HISTORY_PLAYLISTS)? {
            let id = u32_at(d, r)?;
            history.push(PlaylistRow {
                id: id.to_string(),
                name: string_at(d, r + 4)?,
                is_folder: false,
                parent: None,
                order: id as i64,
            });
        }
        let mut history_entries = Vec::new();
        for r in rows(d, T_HISTORY_ENTRIES)? {
            history_entries.push((u32_at(d, r + 4)?, u32_at(d, r + 8)?, u32_at(d, r)?));
        }
        Ok(Export {
            root: root.to_path_buf(),
            tracks,
            artists,
            albums,
            keys,
            genres,
            artwork,
            tree,
            entries,
            history,
            history_entries,
        })
    }

    fn track(&self, t: &TrackRow) -> Track {
        let name = |m: &HashMap<u32, String>, id| m.get(&id).cloned().unwrap_or_default();
        Track {
            title: t.title.clone(),
            artist: name(&self.artists, t.artist_id),
            album: name(&self.albums, t.album_id),
            key: name(&self.keys, t.key_id),
            genre: name(&self.genres, t.genre_id),
            bpm_x100: t.bpm_x100,
            length_s: t.length_s,
            file: under(&self.root, Some(&t.file)),
            analysis_ref: analysis_ref(&self.root, Some(&t.anlz)),
            artwork: existing(
                &self.root,
                self.artwork.get(&t.artwork_id).map(String::as_str),
            ),
            rating: t.rating.min(5),
            comment: t.comment.clone(),
            date_added: t.date_added.clone(),
            bitrate: t.bitrate,
            sample_rate: t.sample_rate,
            ..Track::default()
        }
    }
}

impl Source for Export {
    fn kind(&self) -> String {
        "USB (Device Library)".into()
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        analysis(track)
    }

    /// Siblings by `sort_order`, folders included; the history playlists follow under "Histories".
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let mut all = self.tree.clone();
        add_histories(&mut all, self.history.clone());
        Ok(playlist_tree(&all))
    }

    /// `None`: all tracks sorted by title. `Some(id)`: playlist (or history) entries in `entry_index` order.
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        Ok(match playlist {
            None => {
                let mut v: Vec<&TrackRow> = self.tracks.iter().collect();
                v.sort_by(|a, b| a.title.cmp(&b.title));
                v.into_iter().map(|t| self.track(t)).collect()
            }
            Some(id) => {
                let (list, id) = match history_raw(id) {
                    Some(h) => (&self.history_entries, h),
                    None => (&self.entries, id),
                };
                let Ok(id) = id.parse::<u32>() else {
                    return Ok(Vec::new());
                };
                let by_id: HashMap<u32, &TrackRow> =
                    self.tracks.iter().map(|t| (t.id, t)).collect();
                let mut e: Vec<&(u32, u32, u32)> = list.iter().filter(|e| e.0 == id).collect();
                e.sort_by_key(|e| e.1);
                e.iter()
                    .filter_map(|e| by_id.get(&e.2))
                    .map(|t| self.track(t))
                    .collect()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compat_rekordbox_common::pdb::HEAP;

    /// Track row with known rating/bitrate/etc.; `title` is string #17.
    fn track_row(id: u32, title: &str) -> Vec<u8> {
        let w16 = |d: &mut [u8], p: usize, v: u16| d[p..p + 2].copy_from_slice(&v.to_le_bytes());
        let w32 = |d: &mut [u8], p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
        let short = |s: &str| [&[(s.len() as u8 + 1) << 1 | 1][..], s.as_bytes()].concat();

        let mut track = vec![0u8; 0x5e + 42];
        w32(&mut track, 0x08, 44100);
        w32(&mut track, 0x1c, 7); // artwork_id
        w32(&mut track, 0x30, 320);
        w32(&mut track, 0x38, 12800);
        w32(&mut track, 0x48, id);
        track[0x59] = 4; // rating
        let mut strings = vec![short(""); 21];
        strings[10] = short("2026-01-02");
        strings[14] = short("/A/x.DAT");
        strings[16] = short("nice one");
        strings[17] = short(title);
        strings[20] = short("/F/x.mp3");
        for (i, s) in strings.iter().enumerate() {
            let at = track.len() as u16;
            w16(&mut track, 0x5e + 2 * i, at);
            track.extend(s);
        }
        track
    }

    /// 1024-byte pages (0 = header; per table an empty first page and a data page holding up to
    /// 16 rows back to back) for `(page type, rows)` tables.
    fn build(tables: &[(u32, Vec<Vec<u8>>)]) -> Vec<u8> {
        const L: usize = 1024;
        let w32 = |d: &mut [u8], p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
        let mut d = vec![0u8; (1 + 2 * tables.len()) * L];
        w32(&mut d, 4, L as u32);
        w32(&mut d, 8, tables.len() as u32);
        for (t, (ty, rows)) in tables.iter().enumerate() {
            let (empty, data) = (1 + 2 * t, 2 + 2 * t);
            w32(&mut d, 28 + 16 * t, *ty);
            w32(&mut d, 28 + 16 * t + 8, empty as u32);
            w32(&mut d, 28 + 16 * t + 12, data as u32);
            for (page, next) in [(empty, data), (data, data + 1)] {
                let p = page * L;
                w32(&mut d, p + 4, page as u32);
                w32(&mut d, p + 8, *ty);
                w32(&mut d, p + 12, next as u32);
                d[p + 0x1b] = 0x24;
            }
            let p = data * L;
            w32(
                &mut d,
                p + 0x18,
                rows.len() as u32 | (rows.len() as u32) << 13,
            ); // offsets, rows
            let mut at = 0;
            for (i, row) in rows.iter().enumerate() {
                d[p + HEAP + at..p + HEAP + at + row.len()].copy_from_slice(row);
                let o = p + L - 6 - 2 * i;
                d[o..o + 2].copy_from_slice(&(at as u16).to_le_bytes());
                at += row.len();
            }
            d[p + L - 4..p + L - 2].copy_from_slice(&((1u32 << rows.len()) - 1).to_le_bytes()[..2]); // all present
        }
        d
    }

    /// A track row and the artwork row it points at.
    fn with_track_and_artwork() -> Vec<u8> {
        let short = |s: &str| [&[(s.len() as u8 + 1) << 1 | 1][..], s.as_bytes()].concat();
        let art = [
            7u32.to_le_bytes().to_vec(),
            short("/PIONEER/Artwork/00001/a1.jpg"),
        ]
        .concat();
        build(&[(T_TRACKS, vec![track_row(1, "T")]), (T_ARTWORK, vec![art])])
    }

    #[test]
    fn track_columns_and_artwork() {
        let d = with_track_and_artwork();
        let dir = std::env::temp_dir().join(format!("openfader-pdb-{}", std::process::id()));
        let e = Export::parse(&d, &dir).unwrap();
        let t = &e.tracks[0];
        assert_eq!(
            (t.rating, t.bitrate, t.sample_rate, t.bpm_x100),
            (4, 320, 44100, 12800)
        );
        assert_eq!(
            (t.comment.as_str(), t.date_added.as_str(), t.title.as_str()),
            ("nice one", "2026-01-02", "T")
        );
        assert_eq!(e.artwork.len(), 1);
        // The artwork row resolves, but the file does not exist yet: no artwork.
        let first = e.tracks(None).unwrap().remove(0);
        assert_eq!(first.artwork, None);
        assert_eq!(
            first.analysis_ref,
            Some(dir.join("A/x.DAT").to_string_lossy().into_owned())
        );
        let jpg = dir.join("PIONEER/Artwork/00001/a1.jpg");
        std::fs::create_dir_all(jpg.parent().unwrap()).unwrap();
        std::fs::write(&jpg, [0xff, 0xd8]).unwrap();
        assert_eq!(e.tracks(None).unwrap()[0].artwork, Some(jpg));
        let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
    }

    fn u32s(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// History tables (spec: history_playlist_row = id, name; history_entry_row = track_id,
    /// playlist_id, entry_index) are listed under a trailing "Histories" folder, entries by index.
    #[test]
    fn history_playlists_and_entries() {
        let short = |s: &str| [&[(s.len() as u8 + 1) << 1 | 1][..], s.as_bytes()].concat();
        let tables = [
            (T_TRACKS, vec![track_row(1, "A"), track_row(2, "B")]),
            (
                T_PLAYLIST_TREE,
                vec![[u32s(&[0, 0, 0, 5, 0]), short("Mix")].concat()],
            ),
            (
                T_HISTORY_PLAYLISTS,
                vec![[u32s(&[3]), short("HISTORY 001")].concat()],
            ),
            (
                T_HISTORY_ENTRIES,
                vec![u32s(&[2, 3, 1]), u32s(&[1, 3, 2]), u32s(&[1, 4, 1])],
            ),
        ];
        let e = Export::parse(&build(&tables), Path::new("/x")).unwrap();
        let pls = e.playlists().unwrap();
        let names: Vec<_> = pls
            .iter()
            .map(|p| (p.name.as_str(), p.depth, p.id.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Mix", 0, "5"),
                ("Histories", 0, "history:"),
                ("HISTORY 001", 1, "history:3")
            ]
        );
        let titles = |id: &str| {
            e.tracks(Some(id))
                .unwrap()
                .into_iter()
                .map(|t| t.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(titles("history:3"), ["B", "A"]); // entry_index order
        assert!(titles("3").is_empty() && titles("history:").is_empty()); // ids do not cross namespaces
        assert_eq!((e.tracks.len(), e.tree.len(), e.entries.len()), (2, 1, 0)); // histories are not playlists
    }

    #[test]
    fn truncated_file_is_an_error() {
        let d = with_track_and_artwork();
        assert!(Export::parse(&d[..d.len() - 1], Path::new("/x")).is_err());
        assert!(Export::parse(&d[..20], Path::new("/x")).is_err());
    }
}
