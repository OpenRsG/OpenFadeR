//! The table sets: [`Db::encode`] = `export.pdb` (tracks and everything they reference, the playlist tree, the constant
//! `columns` / sort tables), [`Tags::encode`] = `exportExt.pdb` (MyTag). Ids are assigned here, in order of first use.

use super::{Table, build, rows};
use crate::Names;
use crate::settings::DeviceConfig;
use std::collections::HashMap;

/// One track as the device library stores it; names are resolved to ids by [`Db::encode`].
#[derive(Clone, Debug, Default)]
pub struct Item {
    /// 1-based, unique.
    pub id: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub key: String,
    /// `/PIONEER/Artwork/...` path of the cover, if any; equal paths share one artwork row.
    pub artwork: Option<String>,
    /// 1..=8 (Colors table), 0 = none.
    pub color: u8,
    pub rating: u8,
    pub bpm_x100: u32,
    pub duration_s: u16,
    pub sample_rate: u32,
    pub bitrate: u32,
    pub file_size: u32,
    pub file_type: u16,
    pub comment: String,
    pub date_added: String,
    pub analyze_path: String,
    pub filename: String,
    pub file_path: String,
    /// `YYYY-MM-DD` of the track's analysis; empty = [`Db::analyze_date`].
    pub analyze_date: String,
}

/// A playlist or folder: `tracks` are [`Item::id`]s in playlist order (empty for folders). Siblings are sorted by
/// their position in [`Db::nodes`].
#[derive(Clone, Debug, Default)]
pub struct Node {
    /// 1-based, unique.
    pub id: u32,
    /// 0 = top level.
    pub parent: u32,
    pub name: String,
    pub is_folder: bool,
    pub tracks: Vec<u32>,
}

#[derive(Default)]
pub struct Db {
    pub tracks: Vec<Item>,
    pub nodes: Vec<Node>,
    /// `YYYY-MM-DD` written as every track's analysis date.
    pub analyze_date: String,
    /// Play histories (tables 11 / 12), carried over from the device so rewriting the library keeps them.
    pub histories: Vec<History>,
    /// What the device is configured with: its category / sort / column lists, colour names, device name, background colour.
    pub config: DeviceConfig,
    /// Hot Cue Bank Lists to write into the OneLibrary ([`crate::bank`]); `export.pdb` has no table for them.
    pub banks: Vec<crate::bank::Resolved>,
}

/// A play history the players wrote (`export.pdb` tables 11 and 12): `tracks` are [`Item::id`]s in play order.
#[derive(Clone, Debug, Default)]
pub struct History {
    pub id: u32,
    pub name: String,
    pub tracks: Vec<u32>,
}

fn table(ty: u32, rows: Vec<Vec<u8>>) -> Table {
    Table {
        ty,
        rows,
        shift: matches!(ty, 0 | 2 | 3 | 19),
    }
}

impl Db {
    /// `export.pdb`: tables 0..=19 in type order (unused ones empty, as in a real export).
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let (mut artists, mut albums, mut genres, mut keys, mut art) =
            <(Names, HashMap<(u32, &str), u32>, Names, Names, Names)>::default();
        let (mut album_rows, mut track_rows) = (Vec::new(), Vec::new());
        for t in &self.tracks {
            let artist_id = artists.id(&t.artist);
            let album_id = if t.album.is_empty() {
                0
            } else {
                let next = albums.len() as u32 + 1;
                *albums
                    .entry((artist_id, t.album.as_str()))
                    .or_insert_with(|| {
                        album_rows.push(rows::album(next, artist_id, &t.album));
                        next
                    })
            };
            let row = rows::TrackRow {
                id: t.id,
                title: &t.title,
                artist_id,
                album_id,
                genre_id: genres.id(&t.genre),
                key_id: keys.id(&t.key),
                artwork_id: t.artwork.as_deref().map_or(0, |p| art.id(p)),
                color_id: t.color,
                rating: t.rating,
                bpm_x100: t.bpm_x100,
                duration_s: t.duration_s,
                sample_rate: t.sample_rate,
                bitrate: t.bitrate,
                file_size: t.file_size,
                file_type: t.file_type,
                comment: &t.comment,
                date_added: &t.date_added,
                analyze_date: if t.analyze_date.is_empty() {
                    &self.analyze_date
                } else {
                    &t.analyze_date
                },
                analyze_path: &t.analyze_path,
                filename: &t.filename,
                file_path: &t.file_path,
            };
            track_rows.push(rows::track(&row));
        }
        let named = |n: &Names| {
            n.rows
                .iter()
                .map(|(id, s)| rows::named(*id, s))
                .collect::<Vec<_>>()
        };
        let mut sort_of: HashMap<u32, u32> = HashMap::new();
        let (mut nodes, mut entries) = (Vec::new(), Vec::new());
        for n in &self.nodes {
            let order = sort_of.entry(n.parent).or_insert(0);
            nodes.push(rows::playlist_node(
                n.parent,
                *order,
                n.id,
                n.is_folder,
                &n.name,
            ));
            *order += 1;
            entries.extend(
                n.tracks
                    .iter()
                    .enumerate()
                    .map(|(i, &t)| rows::playlist_entry(i as u32 + 1, t, n.id)),
            );
        }
        let colors = self
            .config
            .colors
            .iter()
            .enumerate()
            .map(|(i, c)| rows::color(i as u8 + 1, c))
            .collect();
        let histories = self
            .histories
            .iter()
            .map(|h| rows::named(h.id, &h.name))
            .collect();
        let history_entries = self
            .histories
            .iter()
            .flat_map(|h| {
                h.tracks
                    .iter()
                    .enumerate()
                    .map(|(i, &t)| rows::history_entry(i as u32 + 1, t, h.id))
            })
            .collect();
        build(&[
            table(0, track_rows),
            table(1, named(&genres)),
            table(
                2,
                artists
                    .rows
                    .iter()
                    .map(|(id, s)| rows::artist(*id, s))
                    .collect(),
            ),
            table(3, album_rows),
            table(4, Vec::new()),
            table(
                5,
                keys.rows.iter().map(|(id, s)| rows::key(*id, s)).collect(),
            ),
            table(6, colors),
            table(7, nodes),
            table(8, entries),
            table(9, Vec::new()),
            table(10, Vec::new()),
            table(11, histories),
            table(12, history_entries),
            table(
                13,
                art.rows.iter().map(|(id, s)| rows::named(*id, s)).collect(),
            ),
            table(14, Vec::new()),
            table(15, Vec::new()),
            table(16, rows::columns()),
            table(17, rows::u16_rows(&self.config.table17())),
            table(18, rows::u16_rows(&self.config.table18())),
            table(
                19,
                vec![rows::library_info(
                    self.tracks.len() as u32,
                    &self.analyze_date,
                )],
            ),
        ])
    }
}

/// MyTag: categories with their tags, and which tracks carry which tag (`tags[category].1[tag]`).
#[derive(Default)]
pub struct Tags {
    pub categories: Vec<(String, Vec<String>)>,
    /// `(track id, category index, tag index)`
    pub assigned: Vec<(u32, usize, usize)>,
}

/// The one row rekordbox keeps in table 7 of `exportExt.pdb` (observed; the u4 at offset 24 looks like a random id).
const TABLE7_ROW: [u8; 60] = {
    let mut r = [0u8; 60];
    r[1] = 0x07;
    let tail = [
        0xa4, 0x2f, 0x7a, 0xba, 0x03, 0x22, 0x23, 0x24, 0x25, 0x26, 0x03, 0x03, 0x03, 0x03, 0x03,
    ];
    let mut i = 0;
    while i < tail.len() {
        r[24 + i] = tail[i];
        i += 1;
    }
    r
};

impl Tags {
    /// `exportExt.pdb`: table types 0..=8 (3 = tags, 4 = tag_tracks). Category ids are 1.., tag ids follow them.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut next = self.categories.len() as u32 + 1;
        let (mut tags, mut tag_ids) = (Vec::new(), Vec::new()); // tag_ids: per category, the id of each tag
        for (c, (name, list)) in self.categories.iter().enumerate() {
            tags.push(rows::tag(0, c as u32, c as u32 + 1, true, name));
            let ids: Vec<u32> = (next..next + list.len() as u32).collect();
            next += list.len() as u32;
            tags.extend(
                list.iter()
                    .zip(&ids)
                    .enumerate()
                    .map(|(i, (name, &id))| rows::tag(c as u32 + 1, i as u32, id, false, name)),
            );
            tag_ids.push(ids);
        }
        let tag_tracks: Vec<Vec<u8>> = self
            .assigned
            .iter()
            .map(|&(t, c, i)| rows::tag_track(t, tag_ids[c][i]))
            .collect();
        let mut tables: Vec<Table> = (0..9)
            .map(|ty| Table {
                ty,
                rows: Vec::new(),
                shift: ty == 3,
            })
            .collect();
        tables[3].rows = tags;
        tables[4].rows = tag_tracks;
        tables[7].rows = vec![TABLE7_ROW.to_vec()];
        build(&tables)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compat_rekordbox_common::pdb::{rows as read_rows, string_at, u16_at, u32_at};

    #[test]
    fn mytag_file_lists_categories_tags_and_assignments() {
        let t = Tags {
            categories: vec![
                ("Mood".into(), vec!["Dark".into(), "Bright".into()]),
                ("Use".into(), vec!["Warmup".into()]),
            ],
            assigned: vec![(7, 0, 1), (7, 1, 0)],
        };
        let d = t.encode().unwrap();
        let at = read_rows(&d, 3).unwrap(); // ext table type 3 = tags
        // Category row, its tags, next category, its tags; names sit at offset 0x1f.
        let names: Vec<_> = at
            .iter()
            .map(|&r| string_at(&d, r + 0x1f).unwrap())
            .collect();
        assert_eq!(names, ["Mood", "Dark", "Bright", "Use", "Warmup"]);
        let ids: Vec<_> = at.iter().map(|&r| u32_at(&d, r + 20).unwrap()).collect();
        assert_eq!(ids, [1, 3, 4, 2, 5]); // categories 1..=2, then tags
        let links: Vec<_> = read_rows(&d, 4)
            .unwrap()
            .into_iter()
            .map(|r| (u32_at(&d, r + 4).unwrap(), u32_at(&d, r + 8).unwrap()))
            .collect();
        assert_eq!(links, [(7, 4), (7, 5)]);
        assert_eq!(read_rows(&d, 7).unwrap().len(), 1);
    }

    #[test]
    fn names_share_ids_and_playlist_entries_are_one_based() {
        let item = |id, artist: &str, genre: &str| Item {
            id,
            title: format!("T{id}"),
            artist: artist.into(),
            genre: genre.into(),
            key: "8A".into(),
            ..Default::default()
        };
        let db = Db {
            tracks: vec![
                item(1, "A", "House"),
                item(2, "A", "Techno"),
                item(3, "", "House"),
            ],
            nodes: vec![
                Node {
                    id: 1,
                    parent: 0,
                    name: "F".into(),
                    is_folder: true,
                    tracks: vec![],
                },
                Node {
                    id: 2,
                    parent: 1,
                    name: "P".into(),
                    is_folder: false,
                    tracks: vec![3, 1],
                },
            ],
            analyze_date: "2026-01-01".into(),
            ..Default::default()
        };
        let d = db.encode().unwrap();
        assert_eq!(read_rows(&d, 2).unwrap().len(), 1); // artist "A" once; "" is no artist
        assert_eq!(read_rows(&d, 1).unwrap().len(), 2);
        assert_eq!(read_rows(&d, 5).unwrap().len(), 1);
        assert_eq!(read_rows(&d, 6).unwrap().len(), 8); // the colour names
        assert_eq!(read_rows(&d, 16).unwrap().len(), 27);
        let entries: Vec<_> = read_rows(&d, 8)
            .unwrap()
            .into_iter()
            .map(|r| {
                (
                    u32_at(&d, r).unwrap(),
                    u32_at(&d, r + 4).unwrap(),
                    u32_at(&d, r + 8).unwrap(),
                )
            })
            .collect();
        assert_eq!(entries, [(1, 3, 2), (2, 1, 2)]);
    }

    /// The device's category / sort lists and colour names reach tables 17, 18 and 6 (default lists: the real export's rows).
    #[test]
    fn the_device_config_sets_the_browse_tables_and_colour_names() {
        let words = |d: &[u8], ty| {
            read_rows(d, ty)
                .unwrap()
                .into_iter()
                .map(|r| [0, 2, 4, 6].map(|o| u16_at(d, r + o).unwrap()))
                .collect::<Vec<_>>()
        };
        let mut db = Db::default();
        let d = db.encode().unwrap();
        assert_eq!(
            (words(&d, 17), words(&d, 18)),
            (db.config.table17(), db.config.table18())
        );
        assert_eq!(words(&d, 17).iter().filter(|r| r[3] != 0).count(), 10);

        db.config.set("categories", "5,2").unwrap();
        db.config.set("sorts", "1").unwrap();
        db.config.set("color.3", "Peak").unwrap();
        let d = db.encode().unwrap();
        let t17 = words(&d, 17);
        assert_eq!(&t17[t17.len() - 2..], [[17, 5, 99, 1], [2, 2, 2, 2]]);
        assert_eq!(words(&d, 18).last().unwrap(), &[26, 1, 256, 0]);
        let names: Vec<_> = read_rows(&d, 6)
            .unwrap()
            .into_iter()
            .map(|r| string_at(&d, r + 8).unwrap())
            .collect();
        assert_eq!(names[..4], ["Pink", "Red", "Peak", "Yellow"]);
    }
}
