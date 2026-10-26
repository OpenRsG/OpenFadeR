//! rekordbox's own collection database (`master.db`, SQLCipher 4) as a library plugin, read only.
//!
//! Table/column names as in the task spec (`djmdKey.ScaleName`, `djmdGenre.Name`); not observed against a real
//! `master.db` here: the tests run on a synthetic fixture with the same names. Also not observed (inferred from the
//! public pyrekordbox schema): `djmdContent.ImagePath` (relative to `share/` like `AnalysisDataPath`), `Rating` (taken
//! as 0 to 5 stars), `Commnt`, `DateCreated`, `BitRate`, `SampleRate`, `djmdAlbum.Name` via `djmdContent.AlbumID`.
//! Histories: `djmdHistory(ID, Seq, Name, Attribute, ParentID, rb_local_deleted)` and `djmdSongHistory(HistoryID,
//! ContentID, TrackNo, rb_local_deleted)` per pyrekordbox `masterdb/models.py` @
//! 43137cb91f8297c768d2a8bdce5b27b2225f0a2d; `Attribute = 1` as a folder is inferred from `djmdPlaylist`.
//! Paths: `FolderPath` goes through the `path_map` setting (else `OPENFADER_PATH_MAP`) when set.
//!
//! Children: [`smart`] (smart-list XML → SQL).

mod smart;

use openfader_plugin::library::{
    Analysis, LibraryPlugin, Playlist, PlaylistRow, Probe, Source, Track, playlist_tree,
};
use compat_rekordbox_common::{
    PathMap, anlz,
    list::{add_histories, analysis_ref, existing, history_raw},
    path_map, remap_path, setting,
    sql::{ReadDb, open_encrypted, query, track_from_row},
};
use rusqlite::{params, params_from_iter};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

/// First bytes of a plain (unencrypted) SQLite file.
const PLAIN_SQLITE: &[u8] = b"SQLite format 3\0";

pub struct MasterDb;

impl LibraryPlugin for MasterDb {
    /// A file named `master.db` (or with extension `db`) whose first bytes are not the SQLite magic: an encrypted one.
    fn probe(path: &Path) -> Option<Probe> {
        let named = path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("master.db"))
            || path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("db"));
        if !named || !path.is_file() {
            return None;
        }
        let mut head = [0u8; 16];
        std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
        (head != *PLAIN_SQLITE).then(|| Probe {
            score: 60,
            kind: "collection database".into(),
        })
    }

    fn open(path: &Path, settings: &Value) -> Result<Box<dyn Source>, String> {
        let key = setting(settings, "key").ok_or("set the database key in the plugin settings")?;
        let map = path_map(&setting(settings, "path_map").unwrap_or_default())?;
        let share = path.parent().unwrap_or(path).join("share");
        Ok(Box::new(Db {
            conn: open_encrypted(path, &key)?,
            share,
            path_map: map,
        }))
    }

    fn analysis_ref(reference: &str, _settings: &Value) -> Option<Analysis> {
        anlz::load_track(Path::new(reference)).ok()
    }
}

struct Db {
    conn: ReadDb,
    /// `share/` folder that `AnalysisDataPath` is relative to.
    share: PathBuf,
    /// Remaps `FolderPath` prefixes written on another OS; empty = paths as stored.
    path_map: PathMap,
}

impl Source for Db {
    fn kind(&self) -> String {
        "master.db".into()
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        anlz::load_track(Path::new(track.analysis_ref.as_deref()?)).ok()
    }

    /// Folders (`Attribute = 1`), plain playlists (`0`) and smart lists (`4`, listed like plain playlists; see
    /// [`smart`]), siblings by `Seq`; then the histories (`djmdHistory`, same `Attribute`/`ParentID`/`Seq` shape,
    /// inferred from pyrekordbox) under "Histories".
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let rows = |sql: &str| {
            query(&self.conn, sql, [], |r| {
                Ok(PlaylistRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    is_folder: r.get::<_, i64>(2)? == 1,
                    parent: r.get(3)?,
                    order: r.get(4)?,
                })
            })
        };
        let mut all = rows(
            "select ID, Name, Attribute, ParentID, Seq from djmdPlaylist where rb_local_deleted = 0 and Attribute in (0, 1, 4)",
        )?;
        add_histories(
            &mut all,
            rows(
                "select ID, coalesce(Name, ''), Attribute, ParentID, Seq from djmdHistory where rb_local_deleted = 0",
            )?,
        );
        Ok(playlist_tree(&all))
    }

    /// All tracks, or one playlist's tracks in playlist order (a smart list: its conditions' matches by title).
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let base = "select coalesce(c.Title, c.FileNameL, ''), coalesce(a.Name, ''), coalesce(k.ScaleName, ''), \
                    coalesce(g.Name, ''), coalesce(c.BPM, 0), coalesce(c.Length, 0), coalesce(c.Rating, 0), \
                    coalesce(c.Commnt, ''), coalesce(c.DateCreated, ''), coalesce(c.BitRate, 0), \
                    coalesce(c.SampleRate, 0), coalesce(al.Name, ''), c.FolderPath, c.AnalysisDataPath, c.ImagePath \
                    from djmdContent c left join djmdArtist a on a.ID = c.ArtistID left join djmdAlbum al on al.ID = c.AlbumID \
                    left join djmdKey k on k.ID = c.KeyID left join djmdGenre g on g.ID = c.GenreID";
        let row = |r: &rusqlite::Row| {
            let path = |i| r.get::<_, Option<String>>(i);
            Ok(Track {
                file: path(12)?
                    .filter(|p| !p.is_empty())
                    .map(|p| PathBuf::from(remap_path(&p, &self.path_map))),
                analysis_ref: analysis_ref(&self.share, path(13)?.as_deref()),
                artwork: existing(&self.share, path(14)?.as_deref()),
                ..track_from_row(r)?
            })
        };
        match playlist {
            None => query(
                &self.conn,
                &format!("{base} where c.rb_local_deleted = 0 order by c.Title"),
                [],
                row,
            ),
            Some(id) if history_raw(id).is_some() => query(
                &self.conn,
                &format!(
                    "{base} join djmdSongHistory sh on sh.ContentID = c.ID \
                     where sh.HistoryID = ?1 and sh.rb_local_deleted = 0 and c.rb_local_deleted = 0 order by sh.TrackNo"
                ),
                params![history_raw(id)],
                row,
            ),
            Some(id) => {
                let smart_xml = query(
                    &self.conn,
                    "select SmartList from djmdPlaylist where ID = ?1 and Attribute = 4 and rb_local_deleted = 0",
                    params![id],
                    |r| r.get::<_, Option<String>>(0),
                )?;
                if let Some(xml) = smart_xml.into_iter().next() {
                    let (cond, args) = smart::where_clause(xml.as_deref().unwrap_or(""))?;
                    return query(
                        &self.conn,
                        &format!("{base} where c.rb_local_deleted = 0 and {cond} order by c.Title"),
                        params_from_iter(args),
                        row,
                    );
                }
                query(
                    &self.conn,
                    &format!(
                        "{base} join djmdSongPlaylist sp on sp.ContentID = c.ID \
                         where sp.PlaylistID = ?1 and sp.rb_local_deleted = 0 and c.rb_local_deleted = 0 order by sp.TrackNo"
                    ),
                    params![id],
                    row,
                )
            }
        }
    }
}
