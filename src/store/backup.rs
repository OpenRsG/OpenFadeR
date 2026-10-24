//! Backup and restore of our library data (File > Library > Backup Library / Restore Library).
//!
//! A backup is a zip next to which an optional `MUSIC_DIR` folder holds the music files ("back up music files as well?", zip
//! named with the date, music in `MUSIC_DIR`). Our zip holds `store.db` (a consistent copy made by
//! `VACUUM INTO`: collection, playlists, cues, grids, ratings, tags, history, devices …), `settings.conf` (the settings
//! file) and `music.tsv` (original path, tab, path relative to `MUSIC_DIR`). Restore copies every table of the backup's
//! `store.db` over ours in one transaction (the backup must not be from a newer build) and puts music back where it was
//! when nothing is there. Backups of other programs are not supported: that is a different database format.
//!
//! The zip is written and read by [`crate::zip`], entries stored uncompressed.
//! ponytail: no deflate on write (the entries are one SQLite file and two text files; `flate2` is already a dependency if backups
//! get large) and no zip64.

use super::{in_tx, Sql, Store, MIGRATIONS, R};
use crate::paths::at;
use crate::zip::{self, Zip};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Folder (beside the zip) holding the backed-up music.
pub const MUSIC_DIR: &str = "music-backup";

const SETTINGS: &str = "settings.conf";
const STORE: &str = "store.db";
const MUSIC_INDEX: &str = "music.tsv";

/// A zip error in the backup's wording.
fn zip_error(e: zip::Error) -> String {
    match e {
        zip::Error::NotZip => "not a OpenFadeR backup zip".into(),
        zip::Error::Crc(name) => format!("{name}: checksum mismatch, the backup is damaged"),
        zip::Error::Other(m) => m,
    }
}

/// Automatic backups (before an update) are named `OpenFadeR-backup-auto-<stamp>-v<version>-s<schema>.zip`: the stamp sorts by
/// time, the version and the store schema say which build wrote the library in it.
const AUTO: &str = "OpenFadeR-backup-auto-";

/// The store schema this build reads and writes (what `PRAGMA user_version` counts).
pub fn schema() -> usize {
    MIGRATIONS.len()
}

/// The schema of the store file `db`; `None` when it is missing or is no store.
pub fn file_schema(db: &Path) -> Option<usize> {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
        .ok()
        .map(|v| v as usize)
}

/// `<data folder>/backups`: where automatic backups go.
pub fn auto_dir() -> Option<PathBuf> {
    Some(crate::paths::data_dir()?.join("backups"))
}

/// The store `db` was migrated by a newer build than the one being rolled back to (`schema` is the highest it reads):
/// the automatic backup `backup` took its place and the newer store stays beside it as `kept`.
#[derive(Debug, PartialEq)]
pub struct RolledBack {
    pub backup: PathBuf,
    pub kept: PathBuf,
}

/// Puts the library of `backup` in the place of the store `db` when `db` is newer than `schema`; `Ok(None)` when it is not
/// (nothing to do) and an error, with everything as it was, when the backup cannot be used. The newer store is renamed
/// `store.v<its schema>.db` (with its `-wal` / `-shm`), never deleted. Works on the files, so the app may still have the
/// store open: it ends right after.
pub fn roll_back_store(db: &Path, backup: &Path, schema: usize) -> R<Option<RolledBack>> {
    let Some(have) = file_schema(db).filter(|&h| h > schema) else {
        return Ok(None);
    };
    let mut z = Zip::open(backup).map_err(zip_error)?;
    let bytes = z.read(STORE).map_err(zip_error)?;
    let staged = db.with_extension("restore.tmp");
    at(std::fs::write(&staged, bytes), &staged)?;
    if file_schema(&staged).is_none_or(|s| s > schema) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!(
            "{}: the backup's library is not readable by the version rolled back to",
            backup.display()
        ));
    }
    // Folds the write-ahead log into the file, so the file alone is the whole newer library.
    if let Ok(c) = Connection::open(db) {
        let _ = c.pragma_update(None, "wal_checkpoint", "TRUNCATE");
    }
    let kept = (1..)
        .map(|n| match n {
            1 => db.with_file_name(format!("store.v{have}.db")),
            n => db.with_file_name(format!("store.v{have}-{n}.db")),
        })
        .find(|p| !p.exists())
        .expect("an unused name");
    let side = |p: &Path, s: &str| {
        let mut n = p.as_os_str().to_owned();
        n.push(s);
        PathBuf::from(n)
    };
    for s in ["", "-wal", "-shm"] {
        let from = side(db, s);
        if from.exists() {
            at(std::fs::rename(&from, side(&kept, s)), &from)?;
        }
    }
    at(std::fs::rename(&staged, db), &staged)?;
    Ok(Some(RolledBack {
        backup: backup.to_owned(),
        kept,
    }))
}

/// What [`Store::backup`] made.
#[derive(Debug)]
pub struct Backup {
    pub zip: PathBuf,
    /// Music files copied into [`MUSIC_DIR`] / that could not be read.
    pub music: usize,
    pub music_failed: usize,
}

/// What [`Store::restore`] found in the zip.
#[derive(Debug)]
pub struct Restored {
    /// The backed-up settings file, for the caller to put in place.
    pub settings: Option<Vec<u8>>,
    /// Music files copied back to their original place / that were already there / that the backup does not hold.
    pub music: usize,
    pub music_present: usize,
    pub music_lost: usize,
}

impl Store {
    /// Writes `<dir>/OpenFadeR-backup-<stamp>.zip` (never over an existing file) with this store and `settings` (the settings
    /// file, when it exists); `music` files are copied to `<dir>/MUSIC_DIR/<n>/<name>` and listed in the zip.
    pub fn backup(
        &self,
        dir: &Path,
        settings: Option<&Path>,
        music: &[PathBuf],
        stamp: &str,
    ) -> R<Backup> {
        at(std::fs::create_dir_all(dir), dir)?;
        let mut zip = dir.join(format!("OpenFadeR-backup-{stamp}.zip"));
        for n in 2.. {
            if !zip.exists() {
                break;
            }
            zip = dir.join(format!("OpenFadeR-backup-{stamp}-{n}.zip"));
        }
        let snapshot = dir.join(format!(".store-{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&snapshot);
        self.run("VACUUM INTO ?1", [snapshot.to_string_lossy()])?;
        let db = at(std::fs::read(&snapshot), &snapshot);
        let _ = std::fs::remove_file(&snapshot);
        let mut entries = vec![(STORE, db?)];
        if let Some(s) = settings.filter(|s| s.is_file()) {
            entries.push((SETTINGS, at(std::fs::read(s), s)?));
        }
        let (mut index, mut copied, mut failed) = (String::new(), 0, 0);
        for (i, f) in music.iter().enumerate() {
            let rel = format!(
                "{i}/{}",
                f.file_name()
                    .map_or_else(|| "file".into(), |n| n.to_string_lossy())
            );
            let to = dir.join(MUSIC_DIR).join(&rel);
            let ok = to
                .parent()
                .is_some_and(|p| std::fs::create_dir_all(p).is_ok())
                && std::fs::copy(f, &to).is_ok();
            if ok {
                index += &format!("{}\t{rel}\n", f.display());
                copied += 1;
            } else {
                failed += 1;
            }
        }
        entries.push((MUSIC_INDEX, index.into_bytes()));
        at(std::fs::write(&zip, zip::write(&entries)), &zip)?;
        Ok(Backup {
            zip,
            music: copied,
            music_failed: failed,
        })
    }

    /// The backup UPDATE NOW makes first: [`Store::backup`] named by [`AUTO`] (no music), then only the newest `keep`
    /// automatic backups stay in `dir`. Returns the zip.
    pub fn backup_auto(
        &self,
        dir: &Path,
        settings: Option<&Path>,
        version: &str,
        stamp: &str,
        keep: usize,
    ) -> R<PathBuf> {
        let tag = format!("auto-{stamp}-v{version}-s{}", schema());
        let zip = self.backup(dir, settings, &[], &tag)?.zip;
        let mut old: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with(AUTO) && n.ends_with(".zip")
            })
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        old.sort();
        for (_, p) in old.iter().rev().skip(keep.max(1)) {
            let _ = std::fs::remove_file(p);
        }
        Ok(zip)
    }

    /// Replaces our data with the backup in `zip`; music is copied back from `MUSIC_DIR` beside the zip where the original
    /// file is gone. The settings file is returned, not written.
    pub fn restore(&self, zip: &Path) -> R<Restored> {
        let mut z = Zip::open(zip).map_err(zip_error)?;
        let mut get = |n: &str| z.has(n).then(|| z.read(n).map_err(zip_error)).transpose();
        let db = get(STORE)?
            .ok_or("this zip holds no store.db (it is not a backup made by this program)")?;
        let (settings, index) = (get(SETTINGS)?, get(MUSIC_INDEX)?);
        // One file per restore: two stores restoring at once (the tests do) never share it.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp =
            std::env::temp_dir().join(format!("openfader-restore-{}-{n}.db", std::process::id()));
        at(std::fs::write(&temp, db), &temp)?;
        let r = self.copy_tables_from(&temp);
        let _ = std::fs::remove_file(&temp);
        r?;
        let mut out = Restored {
            settings,
            music: 0,
            music_present: 0,
            music_lost: 0,
        };
        let base = zip.parent().unwrap_or(Path::new("")).join(MUSIC_DIR);
        let index = index
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        for line in index.lines() {
            let Some((orig, rel)) = line.split_once('\t') else {
                continue;
            };
            let (orig, from) = (Path::new(orig), base.join(rel));
            if orig.exists() {
                out.music_present += 1;
            } else if from.is_file()
                && orig
                    .parent()
                    .is_some_and(|p| std::fs::create_dir_all(p).is_ok())
                && std::fs::copy(&from, orig).is_ok()
            {
                out.music += 1;
            } else {
                out.music_lost += 1;
            }
        }
        // A backup from before the canonical columns holds rows still to be backfilled and edits under legacy keys; the
        // music is back by now, so their files can be read.
        self.system(|| super::rekey::backfill(&mut self.db()))?;
        Ok(out)
    }

    /// Every table of the database file `other` replaces the same-named table here (matching columns only).
    fn copy_tables_from(&self, other: &Path) -> R<()> {
        let theirs: i64 = Connection::open_with_flags(other, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .s()?
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|_| "the backup's store.db cannot be read".to_owned())?;
        if theirs as usize > MIGRATIONS.len() {
            return Err(format!(
                "the backup is from a newer build (store schema v{theirs})"
            ));
        }
        let mut db = self.db();
        db.execute("ATTACH DATABASE ?1 AS bak", [other.to_string_lossy()])
            .s()?;
        let done = self.system(|| replace_tables(&mut db));
        let _ = db.execute("DETACH DATABASE bak", []);
        done
    }
}

/// First column of every row of `sql`. Uncached: the statements below name the attached `bak` schema, which is gone
/// after the restore.
fn names(db: &Connection, sql: &str) -> R<Vec<String>> {
    db.prepare(sql)
        .s()?
        .query_map([], |r| r.get(0))
        .s()?
        .collect::<rusqlite::Result<_>>()
        .s()
}

fn tables(db: &Connection, schema: &str) -> R<Vec<String>> {
    names(
        db,
        &format!(
            "SELECT name FROM {schema}.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
        ),
    )
}

fn columns(db: &Connection, schema: &str, table: &str) -> R<Vec<String>> {
    names(
        db,
        &format!("SELECT name FROM pragma_table_info('{table}', '{schema}')"),
    )
}

/// Empties every table of `main` and refills the ones `bak` also has from it, in one transaction.
fn replace_tables(db: &mut Connection) -> R<()> {
    // The journal stays this store's own: it describes edits of the data being replaced, so it is emptied below.
    let mine: Vec<String> = tables(db, "main")?
        .into_iter()
        .filter(|t| !t.starts_with("journal") && t != "collection_hwm") // the high-water mark of ids only grows
        .collect();
    let shared: Vec<String> = tables(db, "bak")?
        .into_iter()
        .filter(|t| mine.contains(t))
        .collect();
    in_tx(db, true, |tx| {
        tx.execute("DELETE FROM main.journal", []).s()?;
        tx.execute("DELETE FROM main.journal_action WHERE id > 0", [])
            .s()?;
        for t in &mine {
            tx.execute(&format!("DELETE FROM main.\"{t}\""), []).s()?;
        }
        for t in &shared {
            let (a, b) = (columns(tx, "main", t)?, columns(tx, "bak", t)?);
            let cols = a
                .iter()
                .filter(|c| b.contains(c))
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            tx.execute(
                &format!("INSERT INTO main.\"{t}\"({cols}) SELECT {cols} FROM bak.\"{t}\""),
                [],
            )
            .s()?;
        }
        // The journal restarts at seq 1: a cursor into the old one would hide every new edit from write-back.
        tx.execute("UPDATE main.connection SET last_seq = 0", [])
            .s()?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NodeKind;

    #[test]
    fn backup_then_restore_brings_back_data_settings_and_music() {
        let d = std::env::temp_dir().join(format!("openfader-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("music")).unwrap();
        let (song, conf) = (d.join("music/song.mp3"), d.join("settings.conf"));
        std::fs::write(&song, b"audio").unwrap();
        std::fs::write(&conf, b"key = classic\n").unwrap();

        let s = Store::in_memory().unwrap();
        s.set_rating("a|1", Some(4)).unwrap();
        s.set_gain_adjust("a|1", 1.5).unwrap();
        let pl = s.add_node(None, "Mine", NodeKind::Playlist).unwrap();
        s.add_entries(pl, &["a|1".into()]).unwrap();
        s.set_taglist(&["a|1".into()]).unwrap();
        let b = s
            .backup(
                &d.join("out"),
                Some(&conf),
                std::slice::from_ref(&song),
                "t",
            )
            .unwrap();
        assert_eq!((b.music, b.music_failed), (1, 0));
        assert!(d.join("out").join(MUSIC_DIR).join("0/song.mp3").is_file());
        assert_eq!(
            s.backup(&d.join("out"), None, &[], "t")
                .unwrap()
                .zip
                .file_name()
                .unwrap(),
            "OpenFadeR-backup-t-2.zip"
        );

        // The library changes after the backup; the music file is lost.
        s.set_rating("a|1", Some(1)).unwrap();
        s.set_gain_adjust("a|1", -3.0).unwrap();
        s.delete_node(pl).unwrap();
        s.set_rating("late|2", Some(5)).unwrap();
        std::fs::remove_file(&song).unwrap();

        let r = s.restore(&b.zip).unwrap();
        assert_eq!(s.edits("a|1").unwrap().rating, Some(4));
        assert_eq!(s.edits("late|2").unwrap().rating, None);
        assert_eq!(s.gain("a|1").unwrap().unwrap().adjust_db, 1.5); // AUTO GAIN comes back too
        let nodes = s.nodes().unwrap();
        assert_eq!(
            (nodes[0].name.as_str(), s.entries(nodes[0].id).unwrap()),
            ("Mine", vec!["a|1".to_owned()])
        );
        assert_eq!(s.taglist().unwrap(), ["a|1"]);
        assert_eq!(r.settings.as_deref(), Some(&b"key = classic\n"[..]));
        assert_eq!((r.music, r.music_present, r.music_lost), (1, 0, 0));
        assert_eq!(std::fs::read(&song).unwrap(), b"audio");
        // Restoring twice: the music is there now.
        assert_eq!(s.restore(&b.zip).unwrap().music_present, 1);
        assert!(s.restore(&d.join("missing.zip")).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn restore_resets_connection_cursors_with_the_journal() {
        let d = std::env::temp_dir().join(format!("openfader-backup-seq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let s = Store::in_memory().unwrap();
        s.connection_add("p", Path::new("/x"), "L", 500).unwrap();
        let b = s.backup(&d, None, &[], "t").unwrap();
        s.restore(&b.zip).unwrap();
        assert_eq!(s.connections().unwrap()[0].last_seq, 0);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn automatic_backups_are_named_by_build_and_only_the_last_three_stay() {
        let d = std::env::temp_dir().join(format!("openfader-backup-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let s = Store::in_memory().unwrap();
        let mut made = Vec::new();
        for i in 0..5 {
            let zip = s
                .backup_auto(&d, None, "0.2.5", &format!("20260101-00000{i}"), 3)
                .unwrap();
            // Distinct, increasing times whatever the file system's clock grain.
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i);
            std::fs::File::options()
                .write(true)
                .open(&zip)
                .unwrap()
                .set_modified(at)
                .unwrap();
            made.push(zip);
        }
        let left: Vec<bool> = made.iter().map(|z| z.exists()).collect();
        assert_eq!(left, [false, false, true, true, true]);
        assert_eq!(
            made[4].file_name().unwrap().to_string_lossy(),
            format!(
                "OpenFadeR-backup-auto-20260101-000004-v0.2.5-s{}.zip",
                schema()
            )
        );
        // A manual backup in the same folder is never pruned.
        let manual = s.backup(&d, None, &[], "mine").unwrap().zip;
        s.backup_auto(&d, None, "0.2.5", "20260101-000009", 3)
            .unwrap();
        assert!(manual.exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A store file at `db` holding rating 5 for `k`, then marked as written by a build with `extra` more migrations.
    fn store_file(db: &Path, k: &str, extra: i64) {
        let s = Store::open_at(db).unwrap();
        s.set_rating(k, Some(5)).unwrap();
        drop(s);
        Connection::open(db)
            .unwrap()
            .pragma_update(None, "user_version", MIGRATIONS.len() as i64 + extra)
            .unwrap();
    }

    #[test]
    fn rolling_back_puts_the_backup_in_place_of_a_newer_store_and_keeps_the_newer_one() {
        let d = std::env::temp_dir().join(format!("openfader-backup-roll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let old = Store::in_memory().unwrap();
        old.set_rating("k", Some(2)).unwrap();
        let zip = old.backup_auto(&d, None, "0.2.5", "t", 3).unwrap();
        let db = d.join("store.db");
        let here = schema();

        // The store is not newer than the version rolled back to: nothing happens.
        store_file(&db, "k", 0);
        assert_eq!(roll_back_store(&db, &zip, here).unwrap(), None);
        assert_eq!(file_schema(&db), Some(here));

        // A newer store, a backup that cannot be read: the store stays as it is.
        store_file(&db, "k", 1);
        let bad = d.join("bad.zip");
        std::fs::write(&bad, b"not a zip").unwrap();
        assert!(roll_back_store(&db, &bad, here).is_err());
        assert!(roll_back_store(&db, &d.join("missing.zip"), here).is_err());
        assert_eq!(file_schema(&db), Some(here + 1));
        assert!(!d.join("store.restore.tmp").exists());

        let r = roll_back_store(&db, &zip, here).unwrap().unwrap();
        assert_eq!(r.kept, d.join(format!("store.v{}.db", here + 1)));
        assert_eq!(file_schema(&db), Some(here));
        assert_eq!(
            Store::open_at(&db).unwrap().edits("k").unwrap().rating,
            Some(2)
        );
        assert_eq!(file_schema(&r.kept), Some(here + 1));
        let newer = Connection::open(&r.kept).unwrap();
        let rating: i64 = newer
            .query_row("SELECT rating FROM track WHERE key = 'k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rating, 5, "the newer library is kept whole");

        // A second roll-back keeps the first one's file: names never collide.
        store_file(&db, "k", 1);
        let again = roll_back_store(&db, &zip, here).unwrap().unwrap();
        assert_eq!(again.kept, d.join(format!("store.v{}-2.db", here + 1)));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
