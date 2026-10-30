//! The database file: its schema versions (3.0.0 and 2.18.0 for a new library, DDL of libdjinterop `schema_3_0_0.cpp::create` and
//! [`crate::schema_2`]; ten versions of the 2.x and 3.x lines extended in place), reading the library on the device without
//! writing to it, and building the staged file the commit switches in.

use crate::schema_2;
use openfader_plugin::library::commit::Commit;
use rusqlite::{params, Connection, OpenFlags};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A schema version: `Information.schemaVersionMajor/Minor/Patch`.
pub type Version = (i64, i64, i64);

/// The schema of a new library unless the user picks another: Engine DJ 4.1.0 to 4.2.1 (libdjinterop `to_application_version_string`).
pub const CURRENT: Version = (3, 0, 0);

/// The oldest `Database2` schema, Engine DJ 2.0.x to 2.1.x: the one a new library for older Engine versions gets.
pub const OLDEST: Version = (2, 18, 0);

/// The schemas a new library can be made with.
pub const NEW: [Version; 2] = [CURRENT, OLDEST];

/// The schemas an existing library is extended with, all of libdjinterop's `supported_v2_schemas` and `supported_v3_schemas`:
/// 2.18.0, 2.20.1 to 2.20.3, 2.21.0 to 2.21.2 (Engine 2.0 to 4.0.1) and 3.0.0 to 3.0.2 (Engine 4.1 to 5.0). The rows written
/// are valid for each: 3.0.1 only changes triggers and 3.0.2 adds the nullable `Track.albumArtSourceHash` column (libdjinterop
/// `schema_3_0_1.cpp`, `schema_3_0_2.cpp`), which stays NULL; the 2.x line is covered by [`has_edit_time`] and the `PerformanceData`
/// view of [`crate::schema_2`].
pub const EXTENDED: [Version; 10] = [
    (2, 18, 0),
    (2, 20, 1),
    (2, 20, 2),
    (2, 20, 3),
    (2, 21, 0),
    (2, 21, 1),
    (2, 21, 2),
    (3, 0, 0),
    (3, 0, 1),
    (3, 0, 2),
];

/// `1.2.3` for `(1, 2, 3)`: how a version is stored in the device option and shown in messages.
pub fn text(v: Version) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// The version a device option value names; only one of [`NEW`].
pub fn parse_new(s: &str) -> Option<Version> {
    NEW.into_iter().find(|&v| text(v) == s)
}

/// `Track.lastEditTime` exists from 2.20.3 on (libdjinterop `track_table.cpp`: `schema_2_20_3`).
pub fn has_edit_time(v: Version) -> bool {
    v >= (2, 20, 3)
}

/// Everything a schema 3.0.0 `Engine Library/Database2/m.db` is made of, with the row of the stock album art (id 1: "no artwork")
/// the tracks point at. `Information` is filled in by [`stage`].
const DDL: &str = "
CREATE TABLE Information (id INTEGER PRIMARY KEY AUTOINCREMENT, uuid TEXT, schemaVersionMajor INTEGER,
  schemaVersionMinor INTEGER, schemaVersionPatch INTEGER, currentPlayedIndiciator INTEGER,
  lastRekordBoxLibraryImportReadCounter INTEGER);
CREATE TABLE AlbumArt (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT, albumArt BLOB);
CREATE TABLE Pack (id INTEGER PRIMARY KEY AUTOINCREMENT, packId TEXT, changeLogDatabaseUuid TEXT, changeLogId INTEGER,
  lastPackTime DATETIME);
CREATE TABLE Playlist (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT, parentListId INTEGER, isPersisted BOOLEAN,
  nextListId INTEGER, lastEditTime DATETIME, isExplicitlyExported BOOLEAN,
  CONSTRAINT C_NAME_UNIQUE_FOR_PARENT UNIQUE (title, parentListId),
  CONSTRAINT C_NEXT_LIST_ID_UNIQUE_FOR_PARENT UNIQUE (parentListId, nextListId));
CREATE TABLE PlaylistEntity (id INTEGER PRIMARY KEY AUTOINCREMENT, listId INTEGER, trackId INTEGER, databaseUuid TEXT,
  nextEntityId INTEGER, membershipReference INTEGER,
  CONSTRAINT C_NAME_UNIQUE_FOR_LIST UNIQUE (listId, databaseUuid, trackId),
  FOREIGN KEY (listId) REFERENCES Playlist (id) ON DELETE CASCADE);
CREATE TABLE Smartlist (listUuid TEXT NOT NULL PRIMARY KEY, title TEXT, parentPlaylistPath TEXT, nextPlaylistPath TEXT,
  nextListUuid TEXT, rules TEXT, lastEditTime DATETIME,
  CONSTRAINT C_NAME_UNIQUE_FOR_PARENT UNIQUE (title, parentPlaylistPath),
  CONSTRAINT C_NEXT_LIST_UNIQUE_FOR_PARENT UNIQUE (parentPlaylistPath, nextPlaylistPath, nextListUuid));
CREATE TABLE Track (id INTEGER PRIMARY KEY AUTOINCREMENT, playOrder INTEGER, length INTEGER, bpm INTEGER, year INTEGER,
  path TEXT, filename TEXT, bitrate INTEGER, bpmAnalyzed REAL, albumArtId INTEGER, fileBytes INTEGER, title TEXT,
  artist TEXT, album TEXT, genre TEXT, comment TEXT, label TEXT, composer TEXT, remixer TEXT, key INTEGER,
  rating INTEGER, albumArt TEXT, timeLastPlayed DATETIME, isPlayed BOOLEAN, fileType TEXT, isAnalyzed BOOLEAN,
  dateCreated DATETIME, dateAdded DATETIME, isAvailable BOOLEAN, isMetadataOfPackedTrackChanged BOOLEAN,
  isPerfomanceDataOfPackedTrackChanged BOOLEAN, playedIndicator INTEGER, isMetadataImported BOOLEAN,
  pdbImportKey INTEGER, streamingSource TEXT, uri TEXT, isBeatGridLocked BOOLEAN, originDatabaseUuid TEXT,
  originTrackId INTEGER, streamingFlags INTEGER, explicitLyrics BOOLEAN, lastEditTime DATETIME,
  CONSTRAINT C_originDatabaseUuid_originTrackId UNIQUE (originDatabaseUuid, originTrackId),
  CONSTRAINT C_path UNIQUE (path),
  FOREIGN KEY (albumArtId) REFERENCES AlbumArt (id) ON DELETE RESTRICT);
CREATE TABLE PerformanceData (trackId INTEGER PRIMARY KEY, trackData BLOB, overviewWaveFormData BLOB, beatData BLOB,
  quickCues BLOB, loops BLOB, thirdPartySourceId INTEGER, activeOnLoadLoops INTEGER,
  FOREIGN KEY(trackId) REFERENCES Track(id) ON DELETE CASCADE ON UPDATE CASCADE);
CREATE TABLE PreparelistEntity (id INTEGER PRIMARY KEY AUTOINCREMENT, trackId INTEGER, trackNumber INTEGER,
  FOREIGN KEY (trackId) REFERENCES Track (id) ON DELETE CASCADE);
DELETE FROM sqlite_sequence;
CREATE INDEX index_AlbumArt_hash ON AlbumArt (hash);
CREATE INDEX index_PlaylistEntity_nextEntityId_listId ON PlaylistEntity(nextEntityId, listId);
CREATE TRIGGER trigger_after_insert_Pack_timestamp AFTER INSERT ON Pack FOR EACH ROW WHEN NEW.lastPackTime IS NULL
  BEGIN UPDATE Pack SET lastPackTime = strftime('%s') WHERE ROWID = NEW.ROWID; END;
CREATE TRIGGER trigger_after_insert_Pack_changeLogId AFTER INSERT ON Pack FOR EACH ROW WHEN NEW.changeLogId = 0
  BEGIN UPDATE Pack SET changeLogId = 1 WHERE ROWID = NEW.ROWID; END;
CREATE VIEW ChangeLog (id, trackId) AS SELECT 0, 0 WHERE FALSE;
CREATE TRIGGER trigger_before_insert_List BEFORE INSERT ON Playlist FOR EACH ROW
  BEGIN UPDATE Playlist SET nextListId = -(1 + nextListId)
    WHERE nextListId = NEW.nextListId AND parentListId = NEW.parentListId; END;
CREATE TRIGGER trigger_after_insert_List AFTER INSERT ON Playlist FOR EACH ROW
  BEGIN UPDATE Playlist SET nextListId = NEW.id
    WHERE nextListId = -(1 + NEW.nextListId) AND parentListId = NEW.parentListId; END;
CREATE TRIGGER trigger_after_delete_List AFTER DELETE ON Playlist FOR EACH ROW
  BEGIN UPDATE Playlist SET nextListId = OLD.nextListId WHERE nextListId = OLD.id;
    DELETE FROM Playlist WHERE parentListId = OLD.id; END;
CREATE TRIGGER trigger_after_update_isPersistParent AFTER UPDATE ON Playlist
  WHEN (old.isPersisted = 0 AND new.isPersisted = 1) OR (old.parentListId != new.parentListId AND new.isPersisted = 1)
  BEGIN UPDATE Playlist SET isPersisted = 1 WHERE id IN (SELECT parentListId FROM PlaylistAllParent WHERE id=new.id); END;
CREATE TRIGGER trigger_after_update_isPersistChild AFTER UPDATE ON Playlist WHEN old.isPersisted = 1 AND new.isPersisted = 0
  BEGIN UPDATE Playlist SET isPersisted = 0 WHERE id IN (SELECT childListId FROM PlaylistAllChildren WHERE id=new.id); END;
CREATE TRIGGER trigger_after_insert_isPersist AFTER INSERT ON Playlist WHEN new.isPersisted = 1
  BEGIN UPDATE Playlist SET isPersisted = 1 WHERE id IN (SELECT parentListId FROM PlaylistAllParent WHERE id=new.id); END;
CREATE VIEW PlaylistAllParent AS WITH FindAllParent AS (
    SELECT id, parentListId FROM Playlist
    UNION ALL
    SELECT recursiveCTE.id, Plist.parentListId FROM Playlist Plist
      INNER JOIN FindAllParent recursiveCTE ON recursiveCTE.parentListId = Plist.id)
  SELECT * FROM FindAllParent;
CREATE VIEW PlaylistAllChildren AS WITH FindAllChild AS (
    SELECT id, id as childListId FROM Playlist
    UNION ALL
    SELECT recursiveCTE.id, Plist.id FROM Playlist Plist
      INNER JOIN FindAllChild recursiveCTE ON recursiveCTE.childListId = Plist.parentListId)
  SELECT * FROM FindAllChild WHERE id <> childListId;
CREATE VIEW PlaylistPath AS WITH RECURSIVE Heirarchy AS (
    SELECT id AS child, parentListId AS parent, title AS name, 1 AS depth FROM Playlist
    UNION ALL
    SELECT child, parentListId AS parent, title AS name, h.depth + 1 AS depth FROM Playlist c
      JOIN Heirarchy h ON h.parent = c.id ORDER BY depth DESC),
  OrderedList AS (
    SELECT id, nextListId, 1 AS position FROM Playlist WHERE nextListId = 0
    UNION ALL
    SELECT c.id, c.nextListId, l.position + 1 FROM Playlist c INNER JOIN OrderedList l ON c.nextListId = l.id),
  NameConcat AS (
    SELECT child AS id, GROUP_CONCAT(name, ';') || ';' AS path
      FROM (SELECT child, name FROM Heirarchy ORDER BY depth DESC) GROUP BY child)
  SELECT id, path, ROW_NUMBER() OVER (
      ORDER BY (SELECT COUNT(*) FROM (SELECT * FROM Heirarchy WHERE child = id)) DESC,
               (SELECT position FROM OrderedList ol WHERE ol.id = c.id) ASC) AS position
    FROM Playlist c LEFT JOIN NameConcat g USING (id);
CREATE TRIGGER trigger_before_delete_PlaylistEntity BEFORE DELETE ON PlaylistEntity WHEN OLD.trackId > 0
  BEGIN UPDATE PlaylistEntity SET nextEntityId = OLD.nextEntityId WHERE nextEntityId = OLD.id AND listId = OLD.listId; END;
CREATE INDEX index_Track_filename ON Track (filename);
CREATE INDEX index_Track_albumArtId ON Track (albumArtId);
CREATE INDEX index_Track_uri ON Track (uri);
CREATE INDEX index_Track_title ON Track(title);
CREATE INDEX index_Track_length ON Track(length);
CREATE INDEX index_Track_rating ON Track(rating);
CREATE INDEX index_Track_year ON Track(year);
CREATE INDEX index_Track_dateAdded ON Track(dateAdded);
CREATE INDEX index_Track_genre ON Track(genre);
CREATE INDEX index_Track_artist ON Track(artist);
CREATE INDEX index_Track_album ON Track(album);
CREATE INDEX index_Track_key ON Track(key);
CREATE INDEX index_Track_bpmAnalyzed ON Track(CAST(bpmAnalyzed + 0.5 AS int));
CREATE TRIGGER trigger_after_insert_Track_check_id AFTER INSERT ON Track
  WHEN NEW.id <= (SELECT seq FROM sqlite_sequence WHERE name = 'Track')
  BEGIN SELECT RAISE(ABORT, 'Recycling deleted track id''s are not allowed'); END;
CREATE TRIGGER trigger_after_update_Track_check_Id BEFORE UPDATE ON Track WHEN NEW.id <> OLD.id
  BEGIN SELECT RAISE(ABORT, 'Changing track id''s are not allowed'); END;
CREATE TRIGGER trigger_after_insert_Track_fix_origin AFTER INSERT ON Track
  WHEN IFNULL(NEW.originTrackId, 0) = 0 OR IFNULL(NEW.originDatabaseUuid, '') = ''
  BEGIN UPDATE Track SET originTrackId = NEW.id, originDatabaseUuid = (SELECT uuid FROM Information) WHERE track.id = NEW.id; END;
CREATE TRIGGER trigger_after_update_Track_fix_origin AFTER UPDATE ON Track
  WHEN IFNULL(NEW.originTrackId, 0) = 0 OR IFNULL(NEW.originDatabaseUuid, '') = ''
  BEGIN UPDATE Track SET originTrackId = NEW.id, originDatabaseUuid = (SELECT uuid FROM Information) WHERE track.id = NEW.id; END;
CREATE TRIGGER trigger_after_update_Track_timestamp AFTER UPDATE OF length, bpm, year, filename, bitrate, bpmAnalyzed,
    albumArtId, title, artist, album, genre, comment, label, composer, remixer, key, rating, albumArt, fileType, isAnalyzed,
    isBeatgridLocked, trackData, overviewWaveformData, beatData, quickCues, loops, explicitLyrics, activeOnLoadLoops
  ON Track FOR EACH ROW BEGIN UPDATE Track SET lastEditTime = strftime('%s') WHERE ROWID=NEW.ROWID; END;
CREATE TRIGGER trigger_after_insert_Track_insert_performance_data AFTER INSERT ON Track
  BEGIN INSERT INTO PerformanceData(trackId) VALUES(NEW.id); END;
CREATE INDEX index_PreparelistEntity_trackId ON PreparelistEntity (trackId);
INSERT INTO AlbumArt VALUES (1, '', NULL);
";

/// The current time in unix seconds, for `lastEditTime` columns.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 64 random bits: the std hasher is seeded from the OS per instance.
fn random() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.finish()
}

/// A random version 4 UUID, lower case.
fn uuid() -> String {
    let (a, b) = (random(), random());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        (b >> 48) & 0x3fff | 0x8000,
        b & 0xffff_ffff_ffff
    )
}

/// A library that is on the device, read without writing anything there: opened `immutable` (SQLite then creates no `-wal`,
/// `-shm` or journal and changes no time stamp). A `-wal` left by a player holds changes `immutable` would ignore, so then the
/// pair is copied to a scratch folder and opened there (the copy is removed when this is dropped).
pub struct Old {
    pub conn: Connection,
    /// `Information.uuid`: the id of this library, the `databaseUuid` of its playlist entries.
    pub uuid: String,
    pub version: Version,
    _scratch: Option<Scratch>,
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `file:` URI of `p` that opens it `immutable`.
fn immutable_uri(p: &Path) -> String {
    let s = p
        .to_string_lossy()
        .replace('\\', "/")
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    let lead = if s.starts_with('/') { "" } else { "/" };
    format!("file://{lead}{s}?immutable=1")
}

/// Opens `db` read-only and immutable: nothing is written next to it (see [`Old`]).
pub fn open_immutable(db: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        immutable_uri(db),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

/// The `-wal` or `-shm` file of the database `db`.
pub fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The library at `db`, `None` when there is no file; refused (the text for the user) when it is not an Engine library or its
/// schema version is not in [`EXTENDED`].
pub fn open_old(db: &Path) -> Result<Option<Old>, String> {
    if !db.is_file() {
        return Ok(None);
    }
    let err = |e: rusqlite::Error| format!("{}: {e}", db.display());
    let wal = sidecar(db, "-wal");
    let (conn, scratch) = if std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
        let dir = std::env::temp_dir().join(format!(
            "openfader-engine-{}-{:x}",
            std::process::id(),
            random()
        ));
        let scratch = Scratch(dir.clone());
        let io = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::create_dir_all(&dir).map_err(io)?;
        std::fs::copy(db, dir.join("m.db")).map_err(io)?;
        std::fs::copy(&wal, dir.join("m.db-wal")).map_err(io)?;
        let conn = Connection::open_with_flags(dir.join("m.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(err)?;
        (conn, Some(scratch))
    } else {
        (open_immutable(db).map_err(err)?, None)
    };
    let (uuid, major, minor, patch): (String, i64, i64, i64) = conn
        .query_row(
            "SELECT uuid, schemaVersionMajor, schemaVersionMinor, schemaVersionPatch FROM Information",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| {
            format!(
                "The file {} is not an Engine library ({e}). Nothing was written. Check the stick in Engine DJ or choose another device.",
                db.display()
            )
        })?;
    let version = (major, minor, patch);
    if !EXTENDED.contains(&version) {
        return Err(format!(
            "The Engine library {} has schema version {}, which this plugin does not know (it handles 2.18.0, 2.20.1 to 2.20.3, 2.21.0 to 2.21.2 and 3.0.0 to 3.0.2). It might not read what would be added, so nothing was written. Let Engine DJ rewrite the library or update this plugin.",
            db.display(),
            text(version)
        ));
    }
    Ok(Some(Old {
        conn,
        uuid,
        version,
        _scratch: scratch,
    }))
}

/// The library file being built: a copy of `m.db` (or a new one) at `Commit::staged_path(m.db)`, self-contained (journal mode
/// DELETE, no `-wal`/`-shm`), handed to the commit by [`Staged::finish`] and the caller. Whoever drops it before the commit
/// removes the file (`crate::Writer`).
pub struct Staged {
    pub conn: Connection,
    /// `Information.uuid`: the id of this library, the `databaseUuid` of its playlist entries.
    pub uuid: String,
    /// The schema version of the file: the existing library's, else the one it was made with.
    pub version: Version,
    pub path: PathBuf,
}

/// Builds the staged file for the library at `db`: a consistent copy of `old` (`VACUUM INTO` folds in a journal a player left
/// behind), else a new library of the schema `new` (one of [`NEW`]). The folder of `db` must exist.
pub fn stage(db: &Path, new: Version, old: Option<Old>) -> Result<Staged, String> {
    let err = |e: rusqlite::Error| format!("{}: {e}", db.display());
    let path = Commit::staged_path(db);
    let _ = std::fs::remove_file(&path);
    let (uuid, version) = if let Some(old) = old {
        old.conn
            .execute("VACUUM INTO ?1", [path.to_string_lossy()])
            .map_err(err)?;
        (old.uuid, old.version)
    } else {
        debug_assert!(NEW.contains(&new));
        let uuid = uuid();
        let conn = Connection::open(&path).map_err(err)?;
        conn.execute_batch(if new == OLDEST { schema_2::DDL } else { DDL })
            .map_err(err)?;
        conn.execute(
            "INSERT INTO Information (uuid, schemaVersionMajor, schemaVersionMinor, schemaVersionPatch, \
             currentPlayedIndiciator, lastRekordBoxLibraryImportReadCounter) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![uuid, new.0, new.1, new.2, (random() >> 1) as i64],
        )
        .map_err(err)?;
        (uuid, new)
    };
    let conn = Connection::open(&path).map_err(err)?;
    // A copy of a WAL-mode file would keep that mode (and write `-wal` files): the staged file must stand alone.
    let mode: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .map_err(err)?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(format!("{}: journal mode {mode}", path.display()));
    }
    Ok(Staged {
        conn,
        uuid,
        version,
        path,
    })
}

impl Staged {
    /// Runs `PRAGMA integrity_check` and closes the file; returns its path.
    pub fn finish(self) -> Result<PathBuf, String> {
        let ok: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if ok != "ok" {
            return Err(format!(
                "The new library failed its integrity check ({ok}). Nothing was changed on the stick and the old library is intact. Try the export again; if it repeats, the stick may be failing."
            ));
        }
        self.conn.close().map_err(|(_, e)| e.to_string())?;
        Ok(self.path)
    }
}
