//! Our own persistent store (base): the edits to the collection (cues, loops, beat grids, ratings, comments,
//! track colours, tags, user playlists and the play history), reproduced in a plain SQLite file `store.db` under
//! [`paths::data_dir`]. The user's library sources (the library database, USB, XML) are never written; the store is overlaid on what
//! they load, and the store wins.
//!
//! Track identity ([`key`]): `t<id>`, `id` the track's row in `collection` ([`Track::id`]): our own Collection is the
//! library and every track in it has one, so the identity survives a move, a rename or a new file. A track without an id
//! (a device, another source's list) is keyed `t<id>` of the one Collection row with its file name and size when there is
//! exactly one, else `<file name>|<file size in bytes>` (edits made that way move to `t<id>` when the row is added, see
//! [`Store::collection_put`]). A track without a file is keyed `meta|<title>|<artist>`.
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
//! [`phrase`] (manual phrase edits), [`sequence`] (saved sampler sequences), [`matching`] (MATCHING pairs), [`health`] (library health:
//! ignored checks, files that failed to load), [`journal`] (change
//! journal: undo / redo, [`Store::system`]), [`inbox`] (stages INBOX → REVIEW → COLLECTION, move batches); [`overlay`]
//! applies the track data to a [`crate::library::Library`].

mod artwork;
pub mod backup;
mod bank;
mod collection;
pub mod connect;
mod device;
pub mod device_settings;
mod gain;
mod health;
mod history;
mod inbox;
mod journal;
mod matching;
mod node;
mod overlay;
mod phrase;
mod print;
mod rekey;
mod saved_search;
mod sequence;
mod tag;
mod track;
mod undo;

pub use artwork::{MISS_TTL, album_key};
pub use backup::{Backup, MUSIC_DIR, Restored};
pub use bank::{BankSlot, HotCueBank, collect_banks};
pub use device::{DeviceBackup, NewBackup};
pub use gain::{TrackGain, effective_db};
pub use inbox::{Batch, Move, Role, Stage};
pub use node::{Node, NodeKind};
pub use overlay::{Overlay, apply, overlay, with_edits};
pub use saved_search::SavedSearch;
pub use sequence::{MAX_SEQUENCES, Sequence};
pub use tag::{Tag, TagGroup};
pub use track::{Edits, AUTO_CUES, TRACK_COLORS};
pub use undo::UndoEntry;

use crate::{analysis::Cue, library::Track, paths};
use parking_lot::{Mutex, MutexGuard};
use rusqlite::{Connection, OptionalExtension, Params, Row, types::FromSql};
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
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
    // v13: our Collection is the library (src/library/own.rs): every track is a `collection` row and `t<id>` its [`key`].
    // `size` / `mtime` (s) of the file when last seen, `fingerprint` of its decoded audio (found again after a move or a
    // retag), `key_id` the musical key 0..=23 (`1A`..`12B` as 0..=23: (camelot number − 1) × 2 + (B = 1)), `added_at` unix s.
    // `connection`: a library of another app (or a folder) synced into the Collection by a plugin (src/library/connect/);
    // `mode` 0 import only, 1 two-way; `wins` on conflict 0 source, 1 ours, 2 newest; `stamp` the source file's state at the
    // last sync; `last_seq` the journal seq written back last. `source_map` / `source_node`: a source's track / playlist id → ours,
    // `sig` a hash of what the source last said about the track.
    "ALTER TABLE collection ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
     ALTER TABLE collection ADD COLUMN mtime INTEGER NOT NULL DEFAULT 0;
     ALTER TABLE collection ADD COLUMN fingerprint TEXT;
     ALTER TABLE collection ADD COLUMN key_id INTEGER;
     ALTER TABLE collection ADD COLUMN added_at INTEGER NOT NULL DEFAULT 0;
     CREATE INDEX collection_fingerprint ON collection(fingerprint);
     CREATE INDEX collection_bpm ON collection(bpm_x100);
     CREATE INDEX collection_key ON collection(key_id);
     CREATE TABLE connection (
        id INTEGER PRIMARY KEY, plugin TEXT NOT NULL, path TEXT NOT NULL, label TEXT NOT NULL,
        mode INTEGER NOT NULL DEFAULT 0, wins INTEGER NOT NULL DEFAULT 0, last_sync INTEGER NOT NULL DEFAULT 0,
        stamp TEXT NOT NULL DEFAULT '', last_seq INTEGER NOT NULL DEFAULT 0, UNIQUE (plugin, path));
     CREATE TABLE source_map (
        conn INTEGER NOT NULL REFERENCES connection(id) ON DELETE CASCADE, foreign_id TEXT NOT NULL,
        track INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE, sig INTEGER NOT NULL,
        PRIMARY KEY (conn, foreign_id)) WITHOUT ROWID;
     CREATE INDEX source_map_track ON source_map(track);
     CREATE TABLE source_node (
        conn INTEGER NOT NULL REFERENCES connection(id) ON DELETE CASCADE, foreign_id TEXT NOT NULL,
        node INTEGER NOT NULL REFERENCES node(id) ON DELETE CASCADE, PRIMARY KEY (conn, foreign_id)) WITHOUT ROWID;",
    // v14: the change journal (src/store/journal.rs): one `journal_action` per user action, one `journal` row per row it
    // changed (`op` 0 insert, 1 update, 2 delete; `old` / `new` the row as JSON), for undo / redo and write-back.
    "CREATE TABLE journal_action (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, label TEXT NOT NULL, undone INTEGER NOT NULL DEFAULT 0);
     CREATE TABLE journal (
        seq INTEGER PRIMARY KEY, action INTEGER NOT NULL REFERENCES journal_action(id) ON DELETE CASCADE,
        tbl TEXT NOT NULL, op INTEGER NOT NULL, old TEXT, new TEXT);
     CREATE INDEX journal_by_action ON journal(action);",
    // v15: collection ids are never reused (a deleted track's `t<id>` key, analysis file and journal rows stay its own):
    // `collection_hwm.n` is the highest id ever deleted; the next id is above it and above every live id.
    "CREATE TABLE collection_hwm (n INTEGER NOT NULL);
     INSERT INTO collection_hwm SELECT COALESCE(MAX(id), 0) FROM collection;
     CREATE TRIGGER collection_hwm_d AFTER DELETE ON collection BEGIN UPDATE collection_hwm SET n = MAX(n, OLD.id); END;",
    // v16: descriptive text overrides; NULL keeps the source's value, an empty string clears it.
    "ALTER TABLE track ADD COLUMN title TEXT;
     ALTER TABLE track ADD COLUMN artist TEXT;
     ALTER TABLE track ADD COLUMN album TEXT;
     ALTER TABLE track ADD COLUMN genre TEXT;
     ALTER TABLE track ADD COLUMN label TEXT;",
    // v17: AUTO GAIN is capped by the track's peak now (src/audio/loudness.rs); the stored analysed values are re-measured on the next load.
    "UPDATE gain SET analysed_db = NULL;",
    // v18: library health (src/store/health.rs): the tracks a check no longer lists, and the tracks whose audio failed to load.
    "CREATE TABLE IF NOT EXISTS health_ignore (
        check_id TEXT NOT NULL, track INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
        PRIMARY KEY (check_id, track)) WITHOUT ROWID;
     CREATE TABLE IF NOT EXISTS health_failure (
        track INTEGER PRIMARY KEY REFERENCES collection(id) ON DELETE CASCADE, reason TEXT NOT NULL,
        size INTEGER NOT NULL, mtime INTEGER NOT NULL, at INTEGER NOT NULL);",
    // v19: INBOX → REVIEW → COLLECTION (src/store/inbox.rs). `stage` lists the stages in `pos` order (`role` 0 the one new files
    // arrive in, 1 a stage between, 2 the Collection, last); `collection.stage` NULL = in the Collection, else the stage the
    // track waits in (a staged row is a Collection row that the Collection's own list leaves out). `inbox_move` is one row per
    // track a move batch changed (the batch is its journal action), `from_path` ≠ `to_path` when the file moved too.
    // `inbox_seen` = watched-folder files already offered (`path|size|mtime`), `track` the Collection row a duplicate links to.
    "CREATE TABLE IF NOT EXISTS stage (id INTEGER PRIMARY KEY, name TEXT NOT NULL, pos INTEGER NOT NULL, role INTEGER NOT NULL);
     INSERT OR IGNORE INTO stage(id, name, pos, role) VALUES (1, 'INBOX', 0, 0), (2, 'REVIEW', 1, 1), (3, 'COLLECTION', 2, 2);
     CREATE TABLE IF NOT EXISTS inbox_move (
        id INTEGER PRIMARY KEY, track INTEGER NOT NULL, from_stage INTEGER NOT NULL, to_stage INTEGER NOT NULL,
        from_path TEXT NOT NULL, to_path TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS inbox_seen (sig TEXT PRIMARY KEY, track INTEGER) WITHOUT ROWID;",
    // v20: cover art by album or track (src/store/artwork.rs); an empty `path` is a lookup that found nothing at `at` (unix s).
    "CREATE TABLE IF NOT EXISTS artwork (key TEXT PRIMARY KEY, path TEXT NOT NULL, at INTEGER NOT NULL) WITHOUT ROWID;",
    // v21: backups the app took itself before it rewrote a device library (`device_backup.auto`), pruned to the last few per
    // device. SQL cannot guard an ALTER, so [`migrate`] adds the column when missing (as for v19).
    "SELECT 1;",
    // v22: saved searches of the media browser (src/store/saved_search.rs); `spec` is the browser's own text form of the search box and filters.
    "CREATE TABLE IF NOT EXISTS saved_search (
        id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE, spec TEXT NOT NULL, created INTEGER NOT NULL);",
    // v23: `file_print` = the content print of a Collection track's file (size + a
    // hash of three fixed slices, src/library/print.rs: finds a file again after a move or a rename, and a device copy under
    // another name); `import_undo` = what an import changed, to take it back (src/store/undo.rs, src/device/undo.rs).
    "CREATE TABLE IF NOT EXISTS file_print (
        track INTEGER PRIMARY KEY REFERENCES collection(id) ON DELETE CASCADE, print TEXT NOT NULL,
        size INTEGER NOT NULL, mtime INTEGER NOT NULL);
     CREATE INDEX IF NOT EXISTS file_print_by_print ON file_print(print);
     CREATE TABLE IF NOT EXISTS import_undo (
        id INTEGER PRIMARY KEY, at INTEGER NOT NULL, label TEXT NOT NULL, device TEXT NOT NULL, data TEXT NOT NULL);",
];

/// The database behind a mutex: every method locks it for one call, so a `Store` is shared as
/// `Arc<Store>` between the UI thread and workers. Lock order: `cache` before `db`.
pub struct Store {
    db: Mutex<Connection>,
    /// Counts the row changes of the `collection` table whoever makes them (SQLite's update hook): what [`Store::collection_generation`]
    /// reports and the `cache` compares.
    collection_changes: Arc<AtomicU64>,
    /// The track list and the file-name index of the Collection, each as of a [`Store::collection_generation`].
    cache: Mutex<collection::Cache>,
    /// The Collection with the text and rating edits on ([`Store::collection_edited`]). Lock order: before `db`.
    edited: Mutex<Option<overlay::EditedList>>,
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
        let collection_changes = Arc::new(AtomicU64::new(0));
        let count = collection_changes.clone();
        conn.update_hook(Some(move |_, _: &str, table: &str, _| {
            if table == "collection" {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }))
        .s()?;
        migrate(&mut conn)?;
        rekey::backfill(&mut conn)?;
        journal::install(&conn)?;
        Ok(Self {
            db: Mutex::new(conn),
            collection_changes,
            cache: Mutex::default(),
            edited: Mutex::default(),
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

    /// Changes with every row this store has written (inserted, updated or deleted) since it was opened: what a cache of
    /// a query result compares to know it is still current.
    pub fn generation(&self) -> u64 {
        self.db().total_changes()
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
    // Columns SQL cannot guard: added here when missing, so a re-run of their migration keeps them (v19 `collection.stage`, v21
    // `device_backup.auto`).
    let has = |table: &str, column: &str| {
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            [table, column],
            |r| r.get::<_, i64>(0),
        )
        .s()
        .map(|n| n > 0)
    };
    if !has("collection", "stage")? {
        conn.execute_batch(
            "ALTER TABLE collection ADD COLUMN stage INTEGER REFERENCES stage(id);
             CREATE INDEX IF NOT EXISTS collection_stage ON collection(stage);",
        )
        .s()?;
    }
    if !has("device_backup", "auto")? {
        conn.execute_batch("ALTER TABLE device_backup ADD COLUMN auto INTEGER NOT NULL DEFAULT 0;")
            .s()?;
    }
    // The rows [`rekey::backfill`] looks for at every start, found without reading the whole table (3 ms at 50k tracks, the
    // whole file from disk when it is not cached). Not a schema version: an older build opens the store with it as well; a
    // store that cannot take it (read-only) only reads slower.
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS collection_unstamped ON collection(id) WHERE added_at = 0 OR size = 0;",
    );
    Ok(())
}

/// The process-wide store ([`Store::open`]). If it cannot be opened (no `HOME`, read-only disk, newer
/// schema) the reason goes to stderr and edits live in memory for this run only.
pub fn global() -> Arc<Store> {
    GLOBAL
        .get_or_init(|| {
            Arc::new(Store::open().unwrap_or_else(|e| {
                eprintln!("store: {e}; edits are kept in memory for this run only");
                Store::in_memory().expect("in-memory sqlite")
            }))
        })
        .clone()
}

/// [`global`]'s store once something asked for it; [`key`] must not open the store just to find it unused.
static GLOBAL: OnceLock<Arc<Store>> = OnceLock::new();

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

/// The file-name part of a track's [`key`].
fn file_name(p: &Path) -> Cow<'_, str> {
    p.file_name()
        .map_or(Cow::Borrowed(""), |n| n.to_string_lossy())
}

/// What a track with a file is keyed by when it has no row id and no single Collection row to take the id from.
fn legacy_key(name: &str, size: u64) -> String {
    format!("{name}|{size}")
}

/// The key of Collection row `id`.
pub fn id_key(id: i64) -> String {
    format!("t{id}")
}

/// `(size in bytes, mtime in unix s)` of the file at `p` right now; `(0, 0)` when it cannot be stat'ed.
fn file_stamp(p: &Path) -> (i64, i64) {
    let Ok(m) = std::fs::metadata(p) else {
        return (0, 0);
    };
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    (m.len() as i64, mtime)
}

/// Unix s of a `date_added` that starts `YYYY-MM-DD` (UTC midnight); `None` for any other text.
fn date_secs(date: &str) -> Option<i64> {
    let d = chrono::NaiveDate::parse_from_str(date.get(..10)?, "%Y-%m-%d").ok()?;
    Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
}

/// Stable identity of `t` in the process-wide store (see the module docs), the legacy identity while nothing has asked
/// for that store yet. A file that cannot be stat'ed counts as size 0.
pub fn key(t: &Track) -> String {
    key_with(GLOBAL.get().map(|s| &**s), t)
}

fn key_with(store: Option<&Store>, t: &Track) -> String {
    if let Some(id) = t.id {
        return id_key(id);
    }
    let Some(f) = &t.file else {
        return format!("meta|{}|{}", t.title, t.artist);
    };
    let (name, size) = (file_name(f), file_size(f));
    let id = store.and_then(|s| {
        // The row of this very file, else the only row with this name and size.
        s.id_of_path(f)
            .ok()
            .flatten()
            .or_else(|| s.id_of_file(&name, size))
    });
    match id {
        Some(id) => id_key(id),
        None => legacy_key(&name, size),
    }
}

impl Store {
    /// [`key`] looked up in this store.
    pub fn key_of(&self, t: &Track) -> String {
        key_with(Some(self), t)
    }
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
        let s = Store::in_memory().unwrap();
        assert_eq!(s.key_of(&t("a")), "x.mp3|5"); // not in the Collection
        s.collection_put(&[t("a")], false).unwrap();
        let id = s.id_of_path(&dir.join("a").join("x.mp3")).unwrap().unwrap();
        assert_eq!(s.key_of(&t("a")), id_key(id)); // its own row
        assert_eq!(s.key_of(&t("b")), id_key(id)); // the only row with this name and size
        s.collection_put(&[t("b")], false).unwrap();
        assert_eq!(s.key_of(&t("b")), id_key(id + 1));
        std::fs::write(dir.join("x.mp3"), b"12345").unwrap();
        let other = Track {
            file: Some(dir.join("x.mp3")),
            ..Default::default()
        };
        assert_eq!(s.key_of(&other), "x.mp3|5"); // two rows share the name and size: ambiguous, so the legacy key
        assert_eq!(
            s.key_of(&Track {
                id: Some(9),
                ..other
            }),
            "t9"
        );
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
