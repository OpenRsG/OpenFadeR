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

use super::{MIGRATIONS, R, Sql, Store, in_tx};
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

    /// Replaces our data with the backup in `zip`; music is copied back from `MUSIC_DIR` beside the zip where the original
    /// file is gone. The settings file is returned, not written.
    pub fn restore(&self, zip: &Path) -> R<Restored> {
        let mut z = Zip::open(zip).map_err(zip_error)?;
        let mut get = |n: &str| z.has(n).then(|| z.read(n).map_err(zip_error)).transpose();
        let db = get(STORE)?
            .ok_or("this zip holds no store.db (it is not a backup made by this program)")?;
        let (settings, index) = (get(SETTINGS)?, get(MUSIC_INDEX)?);
        let temp =
            std::env::temp_dir().join(format!("openfader-restore-{}.db", std::process::id()));
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
        let done = replace_tables(&mut db);
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
    let mine = tables(db, "main")?;
    let shared: Vec<String> = tables(db, "bak")?
        .into_iter()
        .filter(|t| mine.contains(t))
        .collect();
    in_tx(db, true, |tx| {
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
}
