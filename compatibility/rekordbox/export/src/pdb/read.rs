//! Reads an existing `export.pdb` / `exportExt.pdb` back into the writer's model ([`Db`], [`Tags`]), so a device library can be
//! extended in place: tracks, the playlist tree with its entries, the play histories the players wrote, and MyTag. Row layouts
//! are the ones [`super::rows`] encodes (spec: crate-digger `rekordbox_pdb.ksy`; observed: real rekordbox 7 exports); page and
//! string decoding is `compat_rekordbox_common::pdb`'s.
//!
//! What is not carried (inferred: our writer does not model it, so rewriting `export.pdb` drops it): tables 4, 9, 10, 14 and 15,
//! which rekordbox leaves empty in a fresh export, and the per-track constants the writer always sets. Table 19 (one row: track
//! count, creation date, version) is rewritten from the library; its date is kept.

use super::db::{Db, History, Item, Node, Tags};
use crate::settings::DeviceConfig;
use compat_rekordbox_common::pdb::{rows, string_at, u16_at, u32_at};
use std::collections::HashMap;
use std::path::Path;

fn names(
    d: &[u8],
    ty: u32,
    at: impl Fn(usize) -> Result<(u32, usize), String>,
) -> Result<HashMap<u32, String>, String> {
    rows(d, ty)?
        .into_iter()
        .map(|r| at(r).and_then(|(id, ofs)| Ok((id, string_at(d, ofs)?))))
        .collect()
}

/// The library at `<root>/PIONEER/rekordbox/export.pdb` (and its MyTag file when present), configured with `config`.
pub fn open(root: &Path, config: &DeviceConfig) -> Result<(Db, Tags), String> {
    let dir = root.join("PIONEER/rekordbox");
    let rd =
        |f: &str| std::fs::read(dir.join(f)).map_err(|e| format!("{}: {e}", dir.join(f).display()));
    let mut db = parse(&rd("export.pdb")?)?;
    db.config = config.clone();
    let tags = match rd("exportExt.pdb") {
        Ok(e) => parse_tags(&e)?,
        Err(_) => Tags::default(),
    };
    Ok((db, tags))
}

/// `export.pdb` bytes → [`Db`] (with the default [`DeviceConfig`]). A file that stops before the last page of a table is an
/// error, whatever tables are read.
pub(crate) fn parse(d: &[u8]) -> Result<Db, String> {
    let (page, tables) = (u32_at(d, 4)? as usize, u32_at(d, 8)? as usize);
    for t in 0..tables {
        let last = u32_at(d, 28 + 16 * t + 12)? as usize;
        if page == 0 || d.len() / page <= last {
            return Err(format!(
                "pdb: truncated ({} bytes, a table ends at page {last})",
                d.len()
            ));
        }
    }
    let mut db = Db {
        tracks: tracks(d)?,
        nodes: nodes(d)?,
        histories: histories(d)?,
        ..Db::default()
    };
    // The library's creation date (table 19, written by rekordbox and by us), else the first track's analysis date.
    let created = match rows(d, 19)?.first() {
        Some(&r) => string_at(d, r + 12)?,
        None => String::new(),
    };
    match db.tracks.iter().find(|t| !t.analyze_date.is_empty()) {
        _ if !created.is_empty() => db.analyze_date = created,
        Some(first) => db.analyze_date.clone_from(&first.analyze_date),
        None => {}
    }
    Ok(db)
}

fn tracks(d: &[u8]) -> Result<Vec<Item>, String> {
    let artists = names(d, 2, |r| {
        let ofs = if u16_at(d, r)? & 4 != 0 {
            u16_at(d, r + 0x0a)? as usize
        } else {
            *d.get(r + 9).ok_or("pdb: truncated artist")? as usize
        };
        Ok((u32_at(d, r + 4)?, r + ofs))
    })?;
    let albums = names(d, 3, |r| {
        let ofs = if u16_at(d, r)? & 4 != 0 {
            u16_at(d, r + 0x16)? as usize
        } else {
            *d.get(r + 0x15).ok_or("pdb: truncated album")? as usize
        };
        Ok((u32_at(d, r + 0x0c)?, r + ofs))
    })?;
    let genres = names(d, 1, |r| Ok((u32_at(d, r)?, r + 4)))?;
    let keys = names(d, 5, |r| Ok((u32_at(d, r)?, r + 8)))?;
    let artwork = names(d, 13, |r| Ok((u32_at(d, r)?, r + 4)))?;
    let name = |m: &HashMap<u32, String>, id| m.get(&id).cloned().unwrap_or_default();

    rows(d, 0)?
        .into_iter()
        .map(|r| {
            let s = |i: usize| string_at(d, r + u16_at(d, r + 0x5e + 2 * i)? as usize);
            Ok(Item {
                id: u32_at(d, r + 0x48)?,
                title: s(17)?,
                artist: name(&artists, u32_at(d, r + 0x44)?),
                album: name(&albums, u32_at(d, r + 0x40)?),
                genre: name(&genres, u32_at(d, r + 0x3c)?),
                key: name(&keys, u32_at(d, r + 0x20)?),
                artwork: artwork.get(&u32_at(d, r + 0x1c)?).cloned(),
                color: *d.get(r + 0x58).ok_or("pdb: truncated track")?,
                rating: *d.get(r + 0x59).ok_or("pdb: truncated track")?,
                bpm_x100: u32_at(d, r + 0x38)?,
                duration_s: u16_at(d, r + 0x54)?,
                sample_rate: u32_at(d, r + 0x08)?,
                bitrate: u32_at(d, r + 0x30)?,
                file_size: u32_at(d, r + 0x10)?,
                file_type: u16_at(d, r + 0x5a)?,
                comment: s(16)?,
                date_added: s(10)?,
                analyze_date: s(15)?,
                analyze_path: s(14)?,
                filename: s(19)?,
                file_path: s(20)?,
            })
        })
        .collect()
}

/// The playlist tree (parent u4, 0, sort_order u4, id u4, is_folder u4, name) with its entries (index, track, playlist).
fn nodes(d: &[u8]) -> Result<Vec<Node>, String> {
    let mut tree = Vec::new();
    for r in rows(d, 7)? {
        tree.push((
            u32_at(d, r + 8)?,
            Node {
                id: u32_at(d, r + 12)?,
                parent: u32_at(d, r)?,
                is_folder: u32_at(d, r + 16)? != 0,
                name: string_at(d, r + 20)?,
                tracks: Vec::new(),
            },
        ));
    }
    tree.sort_by_key(|(order, n)| (*order, n.id));
    let mut entries: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    for r in rows(d, 8)? {
        entries
            .entry(u32_at(d, r + 8)?)
            .or_default()
            .push((u32_at(d, r)?, u32_at(d, r + 4)?));
    }
    Ok(tree
        .into_iter()
        .map(|(_, mut n)| {
            let mut e = entries.remove(&n.id).unwrap_or_default();
            e.sort_by_key(|e| e.0);
            n.tracks = e.into_iter().map(|e| e.1).collect();
            n
        })
        .collect())
}

/// Histories: playlist (id, name), entries (track, playlist, index).
fn histories(d: &[u8]) -> Result<Vec<History>, String> {
    let mut out = Vec::new();
    for r in rows(d, 11)? {
        out.push(History {
            id: u32_at(d, r)?,
            name: string_at(d, r + 4)?,
            tracks: Vec::new(),
        });
    }
    let mut played: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    for r in rows(d, 12)? {
        played
            .entry(u32_at(d, r + 4)?)
            .or_default()
            .push((u32_at(d, r + 8)?, u32_at(d, r)?));
    }
    for h in &mut out {
        let mut e = played.remove(&h.id).unwrap_or_default();
        e.sort_by_key(|e| e.0);
        h.tracks = e.into_iter().map(|e| e.1).collect();
    }
    Ok(out)
}

/// `exportExt.pdb` bytes → MyTag categories, their tags and the `(track id, category, tag)` assignments.
pub(crate) fn parse_tags(d: &[u8]) -> Result<Tags, String> {
    // Tag row (see `rows::tag`): category id (0 = this row is a category) u4 @12, position u4 @16, id u4 @20, name @0x1f.
    let (mut cats, mut tags): (Vec<(u32, u32, String)>, Vec<(u32, u32, u32, String)>) =
        Default::default();
    for r in rows(d, 3)? {
        let (cat, pos, id, name) = (
            u32_at(d, r + 12)?,
            u32_at(d, r + 16)?,
            u32_at(d, r + 20)?,
            string_at(d, r + 0x1f)?,
        );
        if cat == 0 {
            cats.push((pos, id, name))
        } else {
            tags.push((cat, pos, id, name))
        }
    }
    cats.sort_by_key(|c| c.0);
    tags.sort_by_key(|t| (t.0, t.1));
    let mut at: HashMap<u32, (usize, usize)> = HashMap::new();
    let mut out = Tags::default();
    for (c, (_, id, name)) in cats.iter().enumerate() {
        let mine: Vec<_> = tags.iter().filter(|t| t.0 == *id).collect();
        for (i, t) in mine.iter().enumerate() {
            at.insert(t.2, (c, i));
        }
        out.categories
            .push((name.clone(), mine.iter().map(|t| t.3.clone()).collect()));
    }
    for r in rows(d, 4)? {
        if let Some(&(c, i)) = at.get(&u32_at(d, r + 8)?) {
            out.assigned.push((u32_at(d, r + 4)?, c, i));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_and_tags_survive_encode_then_parse() {
        let item = |id, artist: &str, album: &str| Item {
            id,
            title: format!("T{id} é"),
            artist: artist.into(),
            album: album.into(),
            genre: "House".into(),
            key: "8A".into(),
            artwork: (id == 1).then(|| "/PIONEER/Artwork/00001/a1.jpg".into()),
            color: 3,
            rating: 4,
            bpm_x100: 12850,
            duration_s: 301,
            sample_rate: 44100,
            bitrate: 320,
            file_size: 1234567,
            file_type: 1,
            comment: "c".into(),
            date_added: "2026-01-02".into(),
            analyze_date: "2026-02-03".into(),
            analyze_path: format!("/PIONEER/USBANLZ/P001/{id:08X}/ANLZ0000.DAT"),
            filename: format!("f{id}.mp3"),
            file_path: format!("/Contents/{artist}/{album}/f{id}.mp3"),
        };
        let db = Db {
            tracks: vec![item(1, "A", "Alb"), item(2, "B", "")],
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
                    tracks: vec![2, 1],
                },
            ],
            analyze_date: "2026-02-03".into(),
            histories: vec![History {
                id: 9,
                name: "HISTORY 001".into(),
                tracks: vec![2, 2, 1],
            }],
            ..Db::default()
        };
        let back = parse(&db.encode().unwrap()).unwrap();
        assert_eq!(format!("{:?}", back.tracks), format!("{:?}", db.tracks));
        assert_eq!(format!("{:?}", back.nodes), format!("{:?}", db.nodes));
        assert_eq!(
            format!("{:?}", back.histories),
            format!("{:?}", db.histories)
        );

        let tags = Tags {
            categories: vec![
                ("Mood".into(), vec!["Dark".into(), "Bright".into()]),
                ("Use".into(), vec!["Warmup".into()]),
            ],
            assigned: vec![(2, 0, 1), (1, 1, 0)],
        };
        let t = parse_tags(&tags.encode().unwrap()).unwrap();
        assert_eq!(t.categories, tags.categories);
        assert_eq!(t.assigned, tags.assigned);
    }
}
