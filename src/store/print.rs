//! The content print of a Collection track's file (`file_print`, [`crate::library::print`]): size and a hash of three fixed slices
//! of the file's bytes. A file that moved or was renamed has the same print, so it is found again without decoding, and so is the
//! copy of a track on a device whose name differs. A print is filled lazily: when a track is analysed or loaded, and by a slow
//! background pass over the rows that have none ([`Store::unprinted`]).

use super::{exists, query_all, scalar, Store, R};
use rusqlite::params;
use std::{collections::HashMap, path::PathBuf};

/// The edits that make a row more than a fresh import: any of these set means the user worked on the track.
const WORKED_ON: &str = "rating IS NOT NULL OR comment IS NOT NULL OR color IS NOT NULL OR grid IS NOT NULL OR title IS NOT NULL
    OR artist IS NOT NULL OR album IS NOT NULL OR genre IS NOT NULL OR label IS NOT NULL
    OR (cues_set = 1 AND NOT EXISTS (SELECT 1 FROM device_setting WHERE scope = 'cue_analysis' AND key = ?1))";

impl Store {
    /// The print of row `id`'s file; `None` until one was made.
    pub fn file_print(&self, id: i64) -> R<Option<String>> {
        scalar(
            &self.db(),
            "SELECT print FROM file_print WHERE track = ?1",
            [id],
        )
    }

    /// Remembers the print of row `id`'s file (the file's `size` and `mtime` in unix s then).
    pub fn set_file_print(&self, id: i64, print: &str, size: i64, mtime: i64) -> R<()> {
        self.system(|| {
            self.run(
                "INSERT INTO file_print(track, print, size, mtime) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(track) DO UPDATE SET print = ?2, size = ?3, mtime = ?4",
                params![id, print, size, mtime],
            )
        })
    }

    /// Whether row `id` is a fresh import nobody worked on: no rating, comment, colour, text or grid edit, no cues but the ones
    /// CUE Analysis made, no place in a playlist, tag or Tag List. A duplicate like that may be replaced by the track it duplicates.
    pub fn is_bare(&self, id: i64) -> R<bool> {
        let key = super::id_key(id);
        let db = self.db();
        for sql in [
            // The folder playlists a connected folder makes for its tracks are not the user's work.
            "SELECT 1 FROM entry WHERE key = ?1 AND node NOT IN (SELECT node FROM source_node)",
            "SELECT 1 FROM track_mytag WHERE key = ?1",
            "SELECT 1 FROM taglist WHERE key = ?1",
            "SELECT 1 FROM phrase_edit WHERE key = ?1",
        ] {
            if exists(&db, sql, [&key])? {
                return Ok(false);
            }
        }
        Ok(!exists(
            &db,
            &format!("SELECT 1 FROM track WHERE key = ?1 AND ({WORKED_ON})"),
            [&key],
        )?)
    }

    /// Every print by the size of its file: `size -> [(row id, print)]`, for matching a file by content ([`crate::device::link::PrintIndex`]).
    pub fn prints_by_size(&self) -> R<HashMap<i64, Vec<(i64, String)>>> {
        let rows: Vec<(i64, i64, String)> = query_all(
            &self.db(),
            "SELECT size, track, print FROM file_print",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut out: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
        for (size, id, print) in rows {
            out.entry(size).or_default().push((id, print));
        }
        Ok(out)
    }

    /// The Collection rows whose file has this print, oldest first.
    pub fn tracks_by_print(&self, print: &str) -> R<Vec<i64>> {
        query_all(
            &self.db(),
            "SELECT track FROM file_print WHERE print = ?1 ORDER BY track",
            [print],
            |r| r.get(0),
        )
    }

    /// Up to `limit` rows that have a file but no print (or one made for another size of that file), as `(id, path)`.
    pub fn unprinted(&self, limit: usize) -> R<Vec<(i64, PathBuf)>> {
        query_all(
            &self.db(),
            "SELECT c.id, c.path FROM collection c LEFT JOIN file_print f ON f.track = c.id
             WHERE c.stage IS NULL AND (f.track IS NULL OR f.size <> c.size) ORDER BY c.id LIMIT ?1",
            [limit as i64],
            |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_are_kept_per_row_and_found_by_print() {
        let s = Store::in_memory().unwrap();
        s.set_file_print(1, "a-1", 10, 5).unwrap_err(); // no such row: the foreign key refuses
        let t = crate::library::Track {
            file: Some(std::env::temp_dir().join("openfader-print-store.mp3")),
            ..Default::default()
        };
        let id = s.collection_upsert(&t, None).unwrap();
        assert_eq!(s.file_print(id).unwrap(), None);
        assert_eq!(s.unprinted(10).unwrap().len(), 1);
        s.set_file_print(id, "a-1", 0, 0).unwrap();
        assert_eq!(s.file_print(id).unwrap().as_deref(), Some("a-1"));
        assert_eq!(s.tracks_by_print("a-1").unwrap(), [id]);
        assert!(s.unprinted(10).unwrap().is_empty());
        s.collection_delete(id).unwrap();
        assert!(s.tracks_by_print("a-1").unwrap().is_empty()); // the print goes with its row
    }
}
