//! Read-only access to a USB/SD "OneLibrary" export (`PIONEER/rekordbox/exportLibrary.db`), a
//! SQLCipher 4 database. Its key is not the `master.db` key: it is `rekordbox_common::onelibrary_key`.
//! Table/column names observed in a real export
//! (including `key(key_id, name)` holding Camelot names like "10A" and `genre(genre_id, name)`,
//! both joined from `content.key_id` / `content.genre_id`, and `album(album_id, name, artist_id, image_id,
//! isComplation, nameForSearch)` joined from `content.album_id`, NULL on some tracks); `attribute = 1` meaning folder is
//! inferred from `master.db` (the sample had no folders). Also observed on `content`: `djComment`,
//! `rating` (0 on every track of the sample), `dateAdded` (`YYYY-MM-DD`), `bitrate` (kbps),
//! `samplingRate` (Hz), and `image_id` → `image(image_id, path)` with a USB-root-relative path
//! (`/PIONEER/Artwork/…`); `image_id` is NULL for tracks without artwork. Histories, observed in
//! the schema of a real export (0 rows in the sample, so the row semantics are inferred from
//! `playlist`): `history(history_id, sequenceNo, name, attribute, history_id_parent)` and
//! `history_content(history_id, content_id, sequenceNo)`; column names also match pyrekordbox
//! `onelibrary/models.py` @ 43137cb91f8297c768d2a8bdce5b27b2225f0a2d.

use super::{analysis, ONE};
use openfader_plugin::library::{playlist_tree, Analysis, Playlist, PlaylistRow, Source, Track};
use rekordbox_common::list::{add_histories, analysis_ref, existing, history_raw, under};
use rekordbox_common::sql::{open_encrypted, query, track_from_row};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How long a derived connection stays shared: long enough that the browser's own read and the device's automatic
/// cue-sync job (both start within a mount's first moment, `Auto::Link` in `app/device/scan.rs`) pay the SQLCipher key
/// derivation (PBKDF2, the slow part of opening a OneLibrary) once between them instead of three or more times over;
/// short enough that nothing here holds `exportLibrary.db` open once that burst is over (Windows cannot replace an open
/// file, and a later export rewrites this one).
/// shortcut: a 3 s window, not a real completion signal between the two readers; raise it if a slower stick needs it,
/// or replace it with an explicit "reuse the browser's read" handoff if the duplicate read moves further apart in time.
const CACHE_TTL: Duration = Duration::from_secs(3);

struct Cached {
    at: Instant,
    sig: (u64, Option<SystemTime>),
    conn: Arc<Mutex<Connection>>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(Default::default);

/// File length and mtime: a cache key that misses as soon as the stick is rewritten, even within [`CACHE_TTL`].
fn signature(p: &Path) -> (u64, Option<SystemTime>) {
    std::fs::metadata(p).map_or((0, None), |m| (m.len(), m.modified().ok()))
}

/// A connection to the OneLibrary at `path`, derived once per [`CACHE_TTL`] window and shared with any other opener of
/// the same file in that window; a changed file (different length or mtime) is always a cache miss. A wrong key fails
/// here and nothing is cached, as before.
fn conn_cached(path: &Path, key: &str) -> Result<Arc<Mutex<Connection>>, String> {
    let sig = signature(path);
    if let Some(c) = CACHE.lock().unwrap().get(path) {
        if c.sig == sig && c.at.elapsed() < CACHE_TTL {
            return Ok(c.conn.clone());
        }
    }
    let conn = Arc::new(Mutex::new(open_encrypted(path, key)?));
    let at = Instant::now();
    CACHE.lock().unwrap().insert(
        path.to_owned(),
        Cached {
            at,
            sig,
            conn: conn.clone(),
        },
    );
    let evict = path.to_owned();
    std::thread::spawn(move || {
        std::thread::sleep(CACHE_TTL);
        let mut cache = CACHE.lock().unwrap();
        if cache.get(&evict).is_some_and(|c| c.at == at) {
            cache.remove(&evict);
        }
    });
    Ok(conn)
}

/// Holds no connection of its own: [`Self::conn`] derives (or reuses a just-derived) one each call, see [`conn_cached`].
pub struct OneLibrary {
    key: String,
    root: PathBuf,
}

impl OneLibrary {
    pub fn open(usb_root: &Path, key: &str) -> Result<Self, String> {
        let this = Self {
            key: key.to_owned(),
            root: usb_root.to_owned(),
        };
        this.conn()?; // a wrong key or a damaged file fails here, as before
        Ok(this)
    }

    fn conn(&self) -> Result<Arc<Mutex<Connection>>, String> {
        conn_cached(&self.root.join(ONE), &self.key)
    }
}

impl Source for OneLibrary {
    fn kind(&self) -> String {
        "USB (OneLibrary)".into()
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        analysis(track)
    }

    /// Siblings by `sequenceNo`; the `history` rows (same shape) follow under "Histories".
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let conn = self.conn()?;
        let conn = conn.lock().unwrap();
        let rows = |sql: &str| {
            query(&conn, sql, [], |r| {
                Ok(PlaylistRow {
                    id: r.get::<_, i64>(0)?.to_string(),
                    name: r.get(1)?,
                    is_folder: r.get::<_, i64>(2)? == 1,
                    parent: r.get::<_, Option<i64>>(3)?.map(|p| p.to_string()),
                    order: r.get(4)?,
                })
            })
        };
        let mut all = rows(
            "select playlist_id, coalesce(name, ''), attribute, playlist_id_parent, sequenceNo from playlist",
        )?;
        add_histories(
            &mut all,
            rows(
                "select history_id, coalesce(name, ''), coalesce(attribute, 0), history_id_parent, coalesce(sequenceNo, 0) from history",
            )?,
        );
        Ok(playlist_tree(&all))
    }

    /// All tracks by title, or one playlist's tracks in `sequenceNo` order.
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let conn = self.conn()?;
        let conn = conn.lock().unwrap();
        let base = "select coalesce(c.title, ''), coalesce(a.name, ''), coalesce(k.name, ''), coalesce(g.name, ''), \
                    coalesce(c.bpmx100, 0), coalesce(c.length, 0), coalesce(c.rating, 0), coalesce(c.djComment, ''), \
                    coalesce(c.dateAdded, ''), coalesce(c.bitrate, 0), coalesce(c.samplingRate, 0), coalesce(al.name, ''), \
                    c.path, c.analysisDataFilePath, i.path \
                    from content c left join artist a on a.artist_id = c.artist_id_artist \
                    left join album al on al.album_id = c.album_id \
                    left join \"key\" k on k.key_id = c.key_id left join genre g on g.genre_id = c.genre_id \
                    left join image i on i.image_id = c.image_id";
        let row = |r: &rusqlite::Row| {
            let path = |i| r.get::<_, Option<String>>(i);
            Ok(Track {
                file: under(&self.root, path(12)?.as_deref()),
                analysis_ref: analysis_ref(&self.root, path(13)?.as_deref()),
                artwork: existing(&self.root, path(14)?.as_deref()),
                ..track_from_row(r)?
            })
        };
        match playlist {
            Some(id) if history_raw(id).is_some() => query(
                &conn,
                &format!(
                    "{base} join history_content hc on hc.content_id = c.content_id where hc.history_id = ?1 order by hc.sequenceNo"
                ),
                params![history_raw(id)],
                row,
            ),
            None => query(&conn, &format!("{base} order by c.title"), [], row),
            Some(id) => query(
                &conn,
                &format!(
                    "{base} join playlist_content pc on pc.content_id = c.content_id where pc.playlist_id = ?1 order by pc.sequenceNo"
                ),
                params![id],
                row,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema as observed in a real export (only the columns the queries read for `content`);
    /// the history rows are synthetic because the sample export has none.
    #[test]
    fn histories_nest_order_and_stay_apart_from_playlists() {
        let dir = std::env::temp_dir().join(format!("openfader-onelib-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("PIONEER/rekordbox")).unwrap();
        let db = Connection::open(dir.join(ONE)).unwrap();
        db.pragma_update(None, "key", "k").unwrap();
        db.execute_batch(
            "create table playlist(playlist_id integer primary key, sequenceNo integer, name varchar, image_id integer, attribute integer, playlist_id_parent integer);
             create table playlist_content(playlist_id integer, content_id integer, sequenceNo integer);
             create table history(history_id integer primary key, sequenceNo integer, name varchar, attribute integer, history_id_parent integer);
             create table history_content(history_id integer, content_id integer, sequenceNo integer);
             create table content(content_id integer primary key, title varchar, artist_id_artist integer, album_id integer, key_id integer, genre_id integer, bpmx100 integer, length integer, path varchar, analysisDataFilePath varchar, image_id integer, rating integer, djComment varchar, dateAdded varchar, bitrate integer, samplingRate integer);
             create table artist(artist_id integer primary key, name varchar);
             create table album(album_id integer primary key, name varchar);
             create table \"key\"(key_id integer primary key, name varchar);
             create table genre(genre_id integer primary key, name varchar);
             create table image(image_id integer primary key, path varchar);
             insert into content(content_id, title) values (1, 'A'), (2, 'B');
             insert into playlist values (1, 0, 'Mix', NULL, 0, NULL);
             insert into history values (1, 0, '2026', 1, NULL), (2, 0, 'Gig', 0, 1), (3, 1, 'Solo', 0, 0);
             insert into history_content values (2, 1, 2), (2, 2, 1), (3, 1, 1);",
        )
        .unwrap();
        drop(db);
        assert!(OneLibrary::open(&dir, "wrong").is_err());
        let lib = OneLibrary::open(&dir, "k").unwrap();
        let pls = lib.playlists().unwrap();
        let shape: Vec<_> = pls
            .iter()
            .map(|p| (p.name.as_str(), p.depth, p.is_folder))
            .collect();
        assert_eq!(
            shape,
            [
                ("Mix", 0, false),
                ("Histories", 0, true),
                ("2026", 1, true),
                ("Gig", 2, false),
                ("Solo", 1, false)
            ]
        );
        let titles = |id: &str| {
            lib.tracks(Some(id))
                .unwrap()
                .into_iter()
                .map(|t| t.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(titles("history:2"), ["B", "A"]); // sequenceNo order
        assert_eq!(titles("history:3"), ["A"]);
        assert!(titles("1").is_empty()); // playlist 1 is not history 1
        let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
    }

    /// Two calls within the window share one derived connection (no repeat SQLCipher key derivation); a file rewritten
    /// since (a different length) is never served from the cache.
    #[test]
    fn cache_shares_a_connection_until_the_file_changes() {
        let dir =
            std::env::temp_dir().join(format!("openfader-onelib-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lib.db");
        let write = |extra: usize| {
            let _ = std::fs::remove_file(&path);
            let db = Connection::open(&path).unwrap();
            db.pragma_update(None, "key", "k").unwrap();
            for i in 0..extra {
                db.execute_batch(&format!("create table t{i}(x)")).unwrap();
            }
        };
        write(1);
        let a = conn_cached(&path, "k").unwrap();
        let b = conn_cached(&path, "k").unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "a second call within the window reuses the first's connection"
        );
        write(6); // a later export rewrites the file: a longer file is a different signature
        let c = conn_cached(&path, "k").unwrap();
        assert!(
            !Arc::ptr_eq(&a, &c),
            "a changed file is never served from the cache"
        );
        let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
    }
}
