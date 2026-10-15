//! Our own persistent store (base): the edits to the collection (cues, loops, beat grids, ratings, comments,
//! track colours, tags, user playlists and the play history), reproduced in a plain SQLite file `store.db` under
//! [`paths::data_dir`]. The user's library sources (the library database, USB, XML) are never written; the store is overlaid on what
//! they load, and the store wins.
//!
//! Track identity ([`key`]): `<file name>|<file size in bytes>`. It survives a different mount point or
//! library source (the library database, the device library, the device library and XML name the same file), and changes when
//! the audio file is replaced; two same-named, same-sized files in different folders share their edits.
//! A track without a file is keyed `meta|<title>|<artist>`.
//!
//! Schema versions: `PRAGMA user_version` counts the applied [`MIGRATIONS`]; opening a store written by
//! a newer build fails instead of guessing.
//!
//! Base: [`Store`], [`key`], [`global`], migrations, and the statement helpers every child uses ([`exec`], [`column`],
//! [`query_all`], [`query_opt`], [`scalar`], [`exists`], [`Store::run`], [`Store::insert`], [`Store::write`]) plus the cue
//! column codec shared by `cue` and `hotcue_bank_cue`. Children, one per data family: [`track`] (rating, comment,
//! colour, cues, grid), [`node`] (playlist tree + entries), [`tag`], [`history`], [`collection`] (imported tracks,
//! removed / relocated source tracks, analysis locks, Tag List, re-keying), [`backup`] (zip backup / restore of the whole
//! store), [`device`] (known devices, Sync Manager playlists, device backups, export baselines), [`device_settings`]
//! (DJ System settings), [`bank`] (Cue Banks), [`gain`] (AUTO GAIN: analysed dB + the user's adjustment),
//! [`phrase`] (manual phrase edits), [`sequence`] (saved sampler sequences), [`matching`] (MATCHING pairs); [`overlay`]
//! applies the track data to a [`crate::library::Library`].

mod backup;
mod bank;
mod collection;
mod device;
pub mod device_settings;
mod gain;
mod history;
mod matching;
mod node;
mod overlay;
mod phrase;
mod sequence;
mod tag;
mod track;

pub use backup::{Backup, MUSIC_DIR, Restored};
pub use bank::{BankSlot, HotCueBank, collect_banks};
pub use device::{DeviceBackup, NewBackup};
pub use gain::{TrackGain, effective_db};
pub use node::{Node, NodeKind};
pub use overlay::{Overlay, apply, overlay};
pub use sequence::{MAX_SEQUENCES, Sequence};
pub use tag::{Tag, TagGroup};
pub use track::{Edits, TRACK_COLORS};

use crate::{analysis::Cue, library::Track, paths};
use parking_lot::{Mutex, MutexGuard};
use rusqlite::{Connection, OptionalExtension, Params, Row, types::FromSql};
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

pub type R<T> = Result<T, String>;

/// Schema, one entry per version (never edit an applied one; append).
const MIGRATIONS: &[&str] = &[
    // v1. NULL column = "no edit, the library's value stands". `cues_set` marks a stored cue list
    // (which may be empty: every library cue deleted). Grid blob: 8 bytes per beat, little-endian
    // `time_ms u32, bpm_x100 u16, bar_pos u16`; `grid_orig` = the grid before the first edit (GRID RESET).
    "CREATE TABLE track (
        key TEXT PRIMARY KEY, rating INTEGER, comment TEXT, color INTEGER,
        cues_set INTEGER NOT NULL DEFAULT 0, grid BLOB, grid_orig BLOB) WITHOUT ROWID;
     CREATE TABLE cue (
        key TEXT NOT NULL REFERENCES track(key) ON DELETE CASCADE, pos INTEGER NOT NULL,
        hot INTEGER NOT NULL, time_ms INTEGER NOT NULL, loop_end_ms INTEGER, color INTEGER, comment TEXT,
        PRIMARY KEY (key, pos)) WITHOUT ROWID;
     CREATE TABLE node (
        id INTEGER PRIMARY KEY, parent INTEGER REFERENCES node(id) ON DELETE CASCADE,
        name TEXT NOT NULL, kind INTEGER NOT NULL, pos INTEGER NOT NULL, smart TEXT);
     CREATE TABLE entry (
        node INTEGER NOT NULL REFERENCES node(id) ON DELETE CASCADE, pos INTEGER NOT NULL,
        key TEXT NOT NULL, PRIMARY KEY (node, pos)) WITHOUT ROWID;
     CREATE TABLE mytag_category (id INTEGER PRIMARY KEY, name TEXT NOT NULL, pos INTEGER NOT NULL);
     CREATE TABLE mytag (
        id INTEGER PRIMARY KEY, category INTEGER NOT NULL REFERENCES mytag_category(id) ON DELETE CASCADE,
        name TEXT NOT NULL, pos INTEGER NOT NULL);
     CREATE TABLE track_mytag (
        key TEXT NOT NULL, tag INTEGER NOT NULL REFERENCES mytag(id) ON DELETE CASCADE,
        PRIMARY KEY (key, tag)) WITHOUT ROWID;
     CREATE TABLE history (
        id INTEGER PRIMARY KEY, day TEXT NOT NULL, key TEXT NOT NULL, played_at INTEGER NOT NULL);
     CREATE INDEX history_day ON history(day);",
    // v2: our Collection ([`collection`]): tracks imported from files / playlists / other libraries, source tracks the user removed
    // (by [`key`]), relocated files (original path → new path), analysis locks, and the persisted Tag List.
    concat!(
        "CREATE TABLE collection (
        id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, title TEXT NOT NULL, artist TEXT NOT NULL, album TEXT NOT NULL,
        tkey TEXT NOT NULL, genre TEXT NOT NULL, bpm_x100 INTEGER NOT NULL, length_s INTEGER NOT NULL, an",
        "lz TEXT, artwork TEXT,
        comment TEXT NOT NULL, date_added TEXT NOT NULL, bitrate INTEGER NOT NULL, sample_rate INTEGER NOT NULL);
     CREATE TABLE removed (id TEXT PRIMARY KEY) WITHOUT ROWID;
     CREATE TABLE relocated (orig TEXT PRIMARY KEY, path TEXT NOT NULL) WITHOUT ROWID;
     CREATE TABLE analysis_lock (key TEXT PRIMARY KEY) WITHOUT ROWID;
     CREATE TABLE taglist (pos INTEGER PRIMARY KEY, key TEXT NOT NULL);"
    ),
    // v3: devices (src/store/device.rs).
    "CREATE TABLE device (id TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', sync_on INTEGER NOT NULL DEFAULT 0, last_seen INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
     CREATE TABLE sync_playlist (device TEXT NOT NULL, playlist TEXT NOT NULL, PRIMARY KEY (device, playlist)) WITHOUT ROWID;
     CREATE TABLE device_backup (id INTEGER PRIMARY KEY, device TEXT NOT NULL, taken_at INTEGER NOT NULL, computer TEXT NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL, fs TEXT NOT NULL, bg INTEGER NOT NULL DEFAULT 0);
     CREATE TABLE history_import (device TEXT NOT NULL, history TEXT NOT NULL, PRIMARY KEY (device, history)) WITHOUT ROWID;",
    // v4: device settings (src/store/device_settings.rs).
    "CREATE TABLE device_setting(scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(scope, key)) WITHOUT ROWID;",
    // v5: baseline of what a device held after our export (src/store/device.rs, Update Collection).
    "CREATE TABLE device_sig (device TEXT NOT NULL, key TEXT NOT NULL, sig INTEGER NOT NULL, PRIMARY KEY (device, key)) WITHOUT ROWID;",
    // v6: Cue Banks (src/store/bank.rs): a list is up to 8 cues of any tracks, `pos` 0.. = letter A..
    "CREATE TABLE hotcue_bank (id INTEGER PRIMARY KEY, name TEXT NOT NULL, pos INTEGER NOT NULL);
     CREATE TABLE hotcue_bank_cue (
        bank INTEGER NOT NULL REFERENCES hotcue_bank(id) ON DELETE CASCADE, pos INTEGER NOT NULL, key TEXT NOT NULL, file TEXT NOT NULL,
        hot INTEGER NOT NULL, time_ms INTEGER NOT NULL, loop_end_ms INTEGER, color INTEGER, comment TEXT,
        PRIMARY KEY (bank, pos)) WITHOUT ROWID;",
    // v7: record label of imported tracks (DJ-library XML `Label`, iTunes Grouping).
    "ALTER TABLE collection ADD COLUMN label TEXT NOT NULL DEFAULT '';",
    // v8: saved sampler sequences (src/store/sequence.rs).
    "CREATE TABLE sequence (slot INTEGER PRIMARY KEY, bars INTEGER NOT NULL, bank INTEGER NOT NULL, events TEXT NOT NULL);",
    // v9: MATCHING pairs (src/store/matching.rs), `a` <= `b`.
    "CREATE TABLE matching (a TEXT NOT NULL, b TEXT NOT NULL, PRIMARY KEY (a, b)) WITHOUT ROWID;",
    // v10: AUTO GAIN per track (src/store/gain.rs): `analysed_db` NULL until analysed, `adjust_db` the user's offset.
    "CREATE TABLE gain (key TEXT PRIMARY KEY, analysed_db REAL, adjust_db REAL NOT NULL DEFAULT 0) WITHOUT ROWID;",
    // v11: manual phrase edits per track (src/store/phrase.rs): `mood` 1 high / 2 mid / 3 low, `list` the whole phrase list (empty = all deleted).
    "CREATE TABLE phrase_edit (key TEXT PRIMARY KEY, mood INTEGER NOT NULL, list BLOB NOT NULL) WITHOUT ROWID;",
    // v12: the collection's analysis path column is an opaque `analysis_ref` now (the first schema calls it by its old name).
    concat!("ALTER TABLE collection RENAME COLUMN an", "lz TO analysis_ref;"),
];

/// The database behind a mutex: every method locks it for one call, so a `Store` is shared as
/// `Arc<Store>` between the UI thread and workers.
pub struct Store {
    db: Mutex<Connection>,
}

/// SQLite errors as the `String`s the rest of the core uses.
trait Sql<T> {
    fn s(self) -> R<T>;
}

impl<T> Sql<T> for rusqlite::Result<T> {
    fn s(self) -> R<T> {
        self.map_err(|e| e.to_string())
    }
}

/// Statements below go through the connection's statement cache: `Connection::execute` and `query_row` re-parse their
/// SQL on every call, `prepare_cached` parses each text once.
fn exec(db: &Connection, sql: &str, p: impl Params) -> R<()> {
    exec_count(db, sql, p).map(drop)
}

/// [`exec`] returning the number of rows changed.
fn exec_count(db: &Connection, sql: &str, p: impl Params) -> R<usize> {
    db.prepare_cached(sql).s()?.execute(p).s()
}

/// Every row of `sql` mapped by `f`, collected into any collection.
fn query_all<T, B: FromIterator<T>>(
    db: &Connection,
    sql: &str,
    p: impl Params,
    f: impl FnMut(&Row) -> rusqlite::Result<T>,
) -> R<B> {
    db.prepare_cached(sql)
        .s()?
        .query_map(p, f)
        .s()?
        .collect::<rusqlite::Result<B>>()
        .s()
}

/// [`query_all`] of the first column.
fn column<T: FromSql, B: FromIterator<T>>(db: &Connection, sql: &str, p: impl Params) -> R<B> {
    query_all(db, sql, p, |r| r.get(0))
}

/// The first row of `sql` mapped by `f`; `None` when there is none.
fn query_opt<T>(
    db: &Connection,
    sql: &str,
    p: impl Params,
    f: impl FnOnce(&Row) -> rusqlite::Result<T>,
) -> R<Option<T>> {
    db.prepare_cached(sql).s()?.query_row(p, f).optional().s()
}

/// [`query_opt`] of the first column.
fn scalar<T: FromSql>(db: &Connection, sql: &str, p: impl Params) -> R<Option<T>> {
    query_opt(db, sql, p, |r| r.get(0))
}

/// Whether `sql` returns any row.
fn exists(db: &Connection, sql: &str, p: impl Params) -> R<bool> {
    db.prepare_cached(sql).s()?.exists(p).s()
}

/// Runs `f` in one transaction on `db` and commits when it returns `Ok`. `defer_fk` postpones foreign-key checks to the
/// commit, for rewrites that move parent and child rows in any order.
fn in_tx<T>(db: &mut Connection, defer_fk: bool, f: impl FnOnce(&Connection) -> R<T>) -> R<T> {
    let tx = db.transaction().s()?;
    if defer_fk {
        tx.execute_batch("PRAGMA defer_foreign_keys = ON").s()?;
    }
    let out = f(&tx)?;
    tx.commit().s()?;
    Ok(out)
}

/// `0xRRGGBB`, how `cue.color` and `hotcue_bank_cue.color` keep a colour.
fn rgb(c: [u8; 3]) -> i64 {
    i64::from(c[0]) << 16 | i64::from(c[1]) << 8 | i64::from(c[2])
}

/// Inverse of [`rgb`]; each `as u8` keeps one channel's byte.
fn unrgb(v: i64) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// The cue in columns `at..at + 5` (`hot, time_ms, loop_end_ms, color, comment`: the tail of `cue` and `hotcue_bank_cue`).
fn cue_at(r: &Row, at: usize) -> rusqlite::Result<Cue> {
    Ok(Cue {
        hot_cue: r.get(at)?,
        time_ms: r.get(at + 1)?,
        loop_end_ms: r.get(at + 2)?,
        color: r.get::<_, Option<i64>>(at + 3)?.map(unrgb),
        comment: r.get(at + 4)?,
    })
}

impl Store {
    /// `<data_dir>/store.db`, created with its folder when missing.
    pub fn open() -> R<Self> {
        Self::open_at(
            &paths::data_dir()
                .ok_or("no data directory (set XDG_DATA_HOME or HOME)")?
                .join("store.db"),
        )
    }

    pub fn open_at(path: &Path) -> R<Self> {
        if let Some(dir) = path.parent() {
            paths::at(std::fs::create_dir_all(dir), dir)?;
        }
        let conn = paths::at(Connection::open(path), path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).s()?;
        conn.pragma_update(None, "journal_mode", "WAL").s()?;
        Self::init(conn)
    }

    pub fn in_memory() -> R<Self> {
        Self::init(Connection::open_in_memory().s()?)
    }

    fn init(mut conn: Connection) -> R<Self> {
        conn.pragma_update(None, "foreign_keys", true).s()?;
        // The children run roughly a hundred distinct statements; the default cache of 16 would evict most of them.
        conn.set_prepared_statement_cache_capacity(128);
        migrate(&mut conn)?;
        Ok(Self {
            db: Mutex::new(conn),
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock()
    }

    /// [`exec`] on the locked connection.
    fn run(&self, sql: &str, p: impl Params) -> R<()> {
        exec(&self.db(), sql, p)
    }

    /// Inserts one row and returns its new id.
    fn insert(&self, sql: &str, p: impl Params) -> R<i64> {
        let db = self.db();
        exec(&db, sql, p)?;
        Ok(db.last_insert_rowid())
    }

    /// [`in_tx`] on the locked connection.
    fn write<T>(&self, f: impl FnOnce(&Connection) -> R<T>) -> R<T> {
        in_tx(&mut self.db(), false, f)
    }
}

/// Applies every migration the file has not seen; refuses a store newer than this build.
fn migrate(conn: &mut Connection) -> R<()> {
    let have: usize = conn
        .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
        .s()? as usize;
    if have > MIGRATIONS.len() {
        return Err(format!(
            "store.db is schema v{have}, newer than this build (v{})",
            MIGRATIONS.len()
        ));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(have) {
        let tx = conn.transaction().s()?;
        tx.execute_batch(sql).s()?;
        tx.pragma_update(None, "user_version", i as i64 + 1).s()?;
        tx.commit().s()?;
    }
    Ok(())
}

/// The process-wide store ([`Store::open`]). If it cannot be opened (no `HOME`, read-only disk, newer
/// schema) the reason goes to stderr and edits live in memory for this run only.
pub fn global() -> Arc<Store> {
    static STORE: LazyLock<Arc<Store>> = LazyLock::new(|| {
        Arc::new(Store::open().unwrap_or_else(|e| {
            eprintln!("store: {e}; edits are kept in memory for this run only");
            Store::in_memory().expect("in-memory sqlite")
        }))
    });
    STORE.clone()
}

/// Sizes already looked up (a stat per track per call would crawl on a USB stick).
static SIZES: LazyLock<Mutex<HashMap<PathBuf, u64>>> = LazyLock::new(Mutex::default);

fn file_size(p: &Path) -> u64 {
    if let Some(&n) = SIZES.lock().get(p) {
        return n;
    }
    match std::fs::metadata(p) {
        Ok(m) => {
            SIZES.lock().insert(p.to_owned(), m.len());
            m.len()
        }
        Err(_) => 0, // not cached: the file may appear (stick mounted later)
    }
}

/// The file-name part of a track's [`key`] (what the overlay indexes the store by before it stats anything).
fn file_name(p: &Path) -> Cow<'_, str> {
    p.file_name()
        .map_or(Cow::Borrowed(""), |n| n.to_string_lossy())
}

/// Stable identity of `t` in the store (see the module docs). A file that cannot be stat'ed counts as size 0.
pub fn key(t: &Track) -> String {
    match &t.file {
        Some(f) => format!("{}|{}", file_name(f), file_size(f)),
        None => format!("meta|{}|{}", t.title, t.artist),
    }
}

/// The name part of a key made by [`key`].
fn name_of(key: &str) -> &str {
    key.rsplit_once('|').map_or(key, |(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once_and_a_newer_store_is_refused() {
        let dir = std::env::temp_dir().join(format!("openfader-store-mig-{}", std::process::id()));
        let path = dir.join("s.db");
        Store::open_at(&path)
            .unwrap()
            .set_rating("a|1", Some(3))
            .unwrap();
        let again = Store::open_at(&path).unwrap(); // second open: no re-run, data intact
        assert_eq!(again.edits("a|1").unwrap().rating, Some(3));
        drop(again);
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", MIGRATIONS.len() as i64 + 1)
            .unwrap();
        drop(c);
        assert!(Store::open_at(&path).err().unwrap().contains("newer"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn key_follows_the_file_not_the_folder() {
        let dir = std::env::temp_dir().join(format!("openfader-store-key-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        for d in ["a", "b"] {
            std::fs::write(dir.join(d).join("x.mp3"), b"12345").unwrap();
        }
        let t = |d: &str| Track {
            file: Some(dir.join(d).join("x.mp3")),
            ..Default::default()
        };
        assert_eq!(key(&t("a")), "x.mp3|5");
        assert_eq!(key(&t("a")), key(&t("b")));
        assert_eq!(name_of(&key(&t("a"))), "x.mp3");
        assert_eq!(
            key(&Track {
                title: "T".into(),
                artist: "A".into(),
                ..Default::default()
            }),
            "meta|T|A"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
