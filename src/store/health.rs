//! Library health: what the user chose to ignore, and the tracks whose audio failed to load.
//!
//! * `health_ignore(check, track)`: the check named by `check` (an id the app owns, `app::library::health::Check::id`) no longer
//!   lists the Collection row `track`. Not user data worth an undo step: not journaled. Rows go with their track.
//! * `health_failure(track)`: the last load or analysis of the file failed (`reason`). `size` / `mtime` are the file's stamp at
//!   that moment, so a replaced file does not keep the mark: [`Store::health_failures`] drops what no longer matches.

use super::{exec, file_stamp, query_all, Store, R};
use crate::library::Track;
use rusqlite::params;
use std::collections::{HashMap, HashSet};

impl Store {
    /// Every ignored track, by check id.
    pub fn health_ignores(&self) -> R<HashMap<String, HashSet<i64>>> {
        let rows: Vec<(String, i64)> = query_all(
            &self.db(),
            "SELECT check_id, track FROM health_ignore",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut out: HashMap<String, HashSet<i64>> = HashMap::new();
        for (check, id) in rows {
            out.entry(check).or_default().insert(id);
        }
        Ok(out)
    }

    /// Ignores (`on`) or lists again (`!on`) the Collection rows `ids` for `check`.
    pub fn health_ignore(&self, check: &str, ids: &[i64], on: bool) -> R<()> {
        self.write(|tx| {
            for id in ids {
                let sql = if on {
                    "INSERT OR IGNORE INTO health_ignore(check_id, track) SELECT ?1, id FROM collection WHERE id = ?2"
                } else {
                    "DELETE FROM health_ignore WHERE check_id = ?1 AND track = ?2"
                };
                exec(tx, sql, params![check, id])?;
            }
            Ok(())
        })
    }

    /// Notes that loading or analysing `t`'s file failed. A file that cannot be stat'ed is missing, not corrupt: nothing is noted.
    pub fn health_note_failure(&self, t: &Track, reason: &str) -> R<()> {
        let (Some(file), Some(id)) = (&t.file, self.id_of_track(t)) else {
            return Ok(());
        };
        let (size, mtime) = file_stamp(file);
        if size == 0 && mtime == 0 {
            return Ok(());
        }
        self.run(
            "INSERT INTO health_failure(track, reason, size, mtime, at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(track) DO UPDATE SET reason = ?2, size = ?3, mtime = ?4, at = ?5",
            params![id, reason, size, mtime, chrono::Utc::now().timestamp()],
        )
    }

    /// `t` loaded or analysed fine: any earlier failure mark goes.
    pub fn health_clear_failure(&self, t: &Track) -> R<()> {
        match self.id_of_track(t) {
            Some(id) => self.run("DELETE FROM health_failure WHERE track = ?1", [id]),
            None => Ok(()),
        }
    }

    /// Collection row id → why its file failed, for the files still as they were when it failed. A mark of a file that changed
    /// since is deleted here.
    pub fn health_failures(&self) -> R<HashMap<i64, String>> {
        let rows: Vec<(i64, String, i64, i64, String)> = query_all(
            &self.db(),
            "SELECT f.track, f.reason, f.size, f.mtime, c.path FROM health_failure f JOIN collection c ON c.id = f.track",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        let (mut keep, mut stale) = (HashMap::new(), Vec::new());
        for (id, reason, size, mtime, path) in rows {
            if file_stamp(std::path::Path::new(&path)) == (size, mtime) {
                keep.insert(id, reason);
            } else {
                stale.push(id);
            }
        }
        for id in stale {
            self.run("DELETE FROM health_failure WHERE track = ?1", [id])?;
        }
        Ok(keep)
    }

    /// The Collection row of `t`: its id, else the row of its file.
    fn id_of_track(&self, t: &Track) -> Option<i64> {
        t.id.or_else(|| self.id_of_path(t.file.as_deref()?).ok().flatten())
    }

    /// Ids of the Collection rows with a failure mark (stale or not): what the tests count.
    #[cfg(test)]
    fn health_failure_rows(&self) -> Vec<i64> {
        super::column(
            &self.db(),
            "SELECT track FROM health_failure ORDER BY track",
            [],
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(dir: &std::path::Path, name: &str, body: &[u8]) -> (Store, Track) {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, body).unwrap();
        let s = Store::in_memory().unwrap();
        let t = Track {
            file: Some(file),
            ..Track::default()
        };
        s.collection_put(&[t], false).unwrap();
        let t = s.collection_tracks().unwrap()[0].clone();
        (s, t)
    }

    #[test]
    fn ignore_and_unignore_round_trip_per_check_and_track() {
        let (s, t) = one(
            &std::env::temp_dir().join(format!("openfader-health-ign-{}", std::process::id())),
            "a.wav",
            b"x",
        );
        let id = t.id.unwrap();
        s.health_ignore("rating", &[id], true).unwrap();
        s.health_ignore("rating", &[id], true).unwrap(); // twice: still one row
        s.health_ignore("artwork", &[id, 999], true).unwrap(); // a row that is not there is not ignored
        let got = s.health_ignores().unwrap();
        assert_eq!(got["rating"], HashSet::from([id]));
        assert_eq!(got["artwork"], HashSet::from([id]));
        s.health_ignore("rating", &[id], false).unwrap();
        let got = s.health_ignores().unwrap();
        assert!(!got.contains_key("rating") && got["artwork"].contains(&id)); // only that check
        s.collection_delete(id).unwrap();
        assert!(
            s.health_ignores().unwrap().is_empty(),
            "the ignores go with the track"
        );
        std::fs::remove_dir_all(t.file.unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_failure_mark_lives_until_a_good_load_or_the_file_changes() {
        let dir =
            std::env::temp_dir().join(format!("openfader-health-fail-{}", std::process::id()));
        let (s, t) = one(&dir, "b.wav", b"abc");
        let id = t.id.unwrap();
        s.health_note_failure(&t, "bad header").unwrap();
        assert_eq!(s.health_failures().unwrap()[&id], "bad header");
        s.health_note_failure(&t, "still bad").unwrap();
        assert_eq!(s.health_failures().unwrap()[&id], "still bad"); // one mark per track
        s.health_clear_failure(&t).unwrap();
        assert!(s.health_failures().unwrap().is_empty());
        s.health_note_failure(&t, "again").unwrap();
        std::fs::write(t.file.as_ref().unwrap(), b"a longer replacement").unwrap();
        assert!(
            s.health_failures().unwrap().is_empty(),
            "a changed file is a new file"
        );
        assert!(
            s.health_failure_rows().is_empty(),
            "and its mark is dropped"
        );
        std::fs::remove_file(t.file.as_ref().unwrap()).unwrap();
        s.health_note_failure(&t, "gone").unwrap();
        assert!(
            s.health_failure_rows().is_empty(),
            "a missing file is not corrupt"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
