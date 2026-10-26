//! Reading the SQLCipher databases (the OneLibrary of a device, `master.db`): a read-only open that never writes next to
//! the file, a row collector and the track columns both libraries select first.

use openfader_plugin::library::Track;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// A read-only connection; an uncheckpointed WAL is read from a private copy, never beside the source media.
pub struct ReadDb {
    conn: Connection,
    _snapshot: Option<tempfile::TempDir>, // dropped after the connection closes, including on Windows
}

impl std::ops::Deref for ReadDb {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.conn
    }
}

/// `path` as an SQLite `file:` URI with `?immutable=1`: SQLite then never creates `-shm`/`-wal`/
/// `-journal` next to the file (library sources live on the user's USB stick and stay untouched).
/// Only `%`, `?` and `#` need escaping in the path part (SQLite URI syntax).
pub fn immutable_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for c in path.to_string_lossy().chars() {
        match c {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3F"),
            '#' => uri.push_str("%23"),
            c => uri.push(c),
        }
    }
    uri.push_str("?immutable=1");
    uri
}

/// Opens SQLCipher without writing to the source. WAL databases are copied locally because `immutable=1` ignores the WAL.
pub fn open_encrypted(path: &Path, key: &str) -> Result<ReadDb, String> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    let snapshot = if wal.is_file() {
        let dir = crate::at(tempfile::tempdir(), path)?;
        crate::at(std::fs::copy(path, dir.path().join("library.db")), path)?;
        crate::at(std::fs::copy(&wal, dir.path().join("library.db-wal")), &wal)?;
        Some(dir)
    } else {
        None
    };
    let open = || -> rusqlite::Result<Connection> {
        let conn = match &snapshot {
            Some(dir) => Connection::open_with_flags(
                dir.path().join("library.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?,
            None => Connection::open_with_flags(
                immutable_uri(path),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )?,
        };
        conn.pragma_update(None, "key", key)?;
        conn.query_row("select count(*) from sqlite_master", [], |_| Ok(()))?;
        Ok(conn)
    };
    Ok(ReadDb {
        conn: crate::at(open(), path)?,
        _snapshot: snapshot,
    })
}

/// Runs `sql` and collects every row through `row`.
pub fn query<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    row: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, String> {
    conn.prepare(sql)
        .and_then(|mut s| s.query_map(params, row)?.collect())
        .map_err(|e| e.to_string())
}

/// A [`Track`] from the twelve non-path columns both SQL sources select first (title, artist,
/// key, genre, bpm x100, length, rating, comment, date added, bitrate, sample rate, album); the
/// caller fills the path fields from the columns after them.
pub fn track_from_row(r: &rusqlite::Row) -> rusqlite::Result<Track> {
    Ok(Track {
        title: r.get(0)?,
        artist: r.get(1)?,
        key: r.get(2)?,
        genre: r.get(3)?,
        bpm_x100: r.get(4)?,
        length_s: r.get(5)?,
        rating: r.get::<_, u8>(6)?.min(5),
        comment: r.get(7)?,
        date_added: r.get(8)?,
        bitrate: r.get(9)?,
        sample_rate: r.get(10)?,
        album: r.get(11)?,
        ..Track::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_escapes_and_wrong_key_fails() {
        assert_eq!(
            immutable_uri(Path::new("/a%b?c#d.db")),
            "file:/a%25b%3Fc%23d.db?immutable=1"
        );
        let p = std::env::temp_dir().join(format!("rk_sql_{}.db", std::process::id()));
        let db = Connection::open(&p).unwrap();
        db.pragma_update(None, "key", "k").unwrap();
        db.execute_batch("create table t(a); insert into t values (1), (2);")
            .unwrap();
        drop(db);
        let c = open_encrypted(&p, "k").unwrap();
        assert_eq!(
            query(&c, "select a from t order by a", [], |r| r.get::<_, i64>(0)).unwrap(),
            [1, 2]
        );
        assert!(open_encrypted(&p, "wrong").is_err());
        drop(c);
        std::fs::remove_file(&p).unwrap();
    }
}
