//! INBOX → REVIEW → COLLECTION: the steps new music takes into the Collection. Our own design.
//!
//! A [`Stage`] is a named step with a [`Role`]: the one that new files arrive in, any number between, and the Collection
//! itself, last. A track waiting in a stage before the Collection is a `collection` row with `stage` set (so it has an id,
//! edits, analysis and a fingerprint like any track); the Collection's own list leaves such rows out
//! ([`Store::collection_tracks`]). `stage` NULL means the Collection.
//!
//! A move ([`Store::stage_moves`]) is one journal action: it updates `stage` and `path` of its rows and writes one `inbox_move`
//! row per track (where it came from and went to), so Edit > Undo restores both and, because the journal replay moves the files
//! the batch moved ([`Store::step_file_moves`]), the files too. [`Store::recent_batches`] lists the batches still undoable.
//!
//! `inbox_seen` remembers the watched-folder files already offered (`path|size|mtime`), so a file the user moved on or removed
//! is not offered again; `track` is the Collection row a duplicate is linked to instead of being added.

use super::{
    collection::{from_row, COLS},
    exec, exec_count, file_stamp, query_all, scalar, Store, R,
};
use crate::library::Track;
use rusqlite::params;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// What a stage is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Where new files arrive; there is one, first.
    Inbox,
    /// Any stage between (REVIEW).
    Middle,
    /// The Collection; there is one, last.
    Collection,
}

impl Role {
    fn of(n: i64) -> Role {
        match n {
            0 => Role::Inbox,
            2 => Role::Collection,
            _ => Role::Middle,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub id: i64,
    pub name: String,
    pub role: Role,
}

/// One track of a move batch: its stage and file before and after (`to_path` = `from_path` when the file stays).
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    pub track: i64,
    pub from_stage: i64,
    pub to_stage: i64,
    pub from_path: PathBuf,
    pub to_path: PathBuf,
}

/// A move batch the journal still holds and has not undone.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    /// The journal action ([`Store::batch_moves`]).
    pub action: i64,
    pub label: String,
    /// Unix s.
    pub at: i64,
    pub tracks: usize,
    /// It is the step Edit > Undo would undo now.
    pub next: bool,
}

impl Store {
    /// Every stage, first to last.
    pub fn stages(&self) -> R<Vec<Stage>> {
        query_all(
            &self.db(),
            "SELECT id, name, role FROM stage ORDER BY pos, id",
            [],
            |r| {
                Ok(Stage {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    role: Role::of(r.get(2)?),
                })
            },
        )
    }

    /// A new stage just before the Collection.
    pub fn add_stage(&self, name: &str) -> R<i64> {
        self.write(|tx| {
            exec(
                tx,
                "INSERT INTO stage(name, pos, role) VALUES (?1, (SELECT pos FROM stage WHERE role = 2), 1)",
                [name],
            )?;
            let id = tx.last_insert_rowid();
            exec(tx, "UPDATE stage SET pos = pos + 1 WHERE role = 2", [])?;
            Ok(id)
        })
    }

    pub fn rename_stage(&self, id: i64, name: &str) -> R<()> {
        self.run(
            "UPDATE stage SET name = ?2 WHERE id = ?1",
            params![id, name],
        )
    }

    /// Deletes a stage between INBOX and the Collection; its tracks wait in the stage before it (no file moves).
    pub fn delete_stage(&self, id: i64) -> R<()> {
        self.write(|tx| {
            let (pos, role): (i64, i64) = super::query_opt(
                tx,
                "SELECT pos, role FROM stage WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .ok_or("no such stage")?;
            if role != 1 {
                return Err("INBOX and COLLECTION stay".into());
            }
            exec(
                tx,
                "UPDATE collection SET stage = (SELECT id FROM stage WHERE pos < ?2 ORDER BY pos DESC LIMIT 1) WHERE stage = ?1",
                params![id, pos],
            )?;
            exec(tx, "DELETE FROM stage WHERE id = ?1", [id])
        })
    }

    /// Tracks in each stage (the Collection's too), by stage id.
    pub fn stage_counts(&self) -> R<HashMap<i64, usize>> {
        let rows: Vec<(i64, i64)> = query_all(
            &self.db(),
            "SELECT COALESCE(stage, (SELECT id FROM stage WHERE role = 2)), COUNT(*) FROM collection GROUP BY 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(rows.into_iter().map(|(s, n)| (s, n as usize)).collect())
    }

    /// The tracks waiting in a stage before the Collection, in arrival order (empty for the Collection's), with the user's text
    /// and rating edits on as the Collection shows them.
    pub fn stage_tracks(&self, stage: i64) -> R<Vec<Track>> {
        let mut tracks: Vec<Track> = query_all(
            &self.db(),
            &format!("SELECT id, {COLS} FROM collection WHERE stage = ?1 ORDER BY id"),
            [stage],
            from_row,
        )?;
        super::overlay::edited(self, &mut tracks);
        Ok(tracks)
    }

    /// The stage and file of Collection row `id` now (the Collection's stage id for a row in it).
    pub fn stage_of(&self, id: i64) -> R<Option<(i64, PathBuf)>> {
        super::query_opt(
            &self.db(),
            "SELECT COALESCE(stage, (SELECT id FROM stage WHERE role = 2)), path FROM collection WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?))),
        )
    }

    /// Adds `tracks` (with a file; the ones the Collection lists already are only refreshed) waiting in `stage`: not a journal
    /// step, a file arriving is no edit. The rows' ids in order.
    pub fn collection_put_staged(&self, tracks: &[Track], stage: i64) -> R<Vec<i64>> {
        let items: Vec<(Option<i64>, &Track)> = tracks.iter().map(|t| (None, t)).collect();
        self.system(|| self.put_rows(&items, false, Some(stage)))
            .map(|(_, ids)| ids)
    }

    /// Applies `moves` as one undo step called `label`: each row takes its new stage and file (already moved on disk), and
    /// each is recorded in `inbox_move`.
    pub fn stage_moves(&self, label: &str, moves: &[Move]) -> R<()> {
        self.action(label, || {
            self.write(|tx| {
                for m in moves {
                    let (size, mtime) = file_stamp(&m.to_path);
                    let changed = exec_count(
                        tx,
                        "UPDATE collection SET path = ?2, size = ?4, mtime = ?5,
                            stage = CASE WHEN (SELECT role FROM stage WHERE id = ?3) = 2 THEN NULL ELSE ?3 END
                         WHERE id = ?1",
                        params![m.track, m.to_path.to_string_lossy(), m.to_stage, size, mtime],
                    )?;
                    if changed == 0 {
                        return Err(format!("no collection row {}", m.track));
                    }
                    exec(
                        tx,
                        "INSERT INTO inbox_move(track, from_stage, to_stage, from_path, to_path) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            m.track,
                            m.from_stage,
                            m.to_stage,
                            m.from_path.to_string_lossy(),
                            m.to_path.to_string_lossy()
                        ],
                    )?;
                }
                Ok(())
            })
        })
    }

    /// The newest move batches the journal holds that are not undone, newest first.
    pub fn recent_batches(&self, limit: usize) -> R<Vec<Batch>> {
        query_all(
            &self.db(),
            "SELECT a.id, a.label, a.at, COUNT(j.seq),
                    a.id = (SELECT MAX(id) FROM journal_action WHERE id > 0 AND undone = 0)
             FROM journal_action a JOIN journal j ON j.action = a.id AND j.tbl = 'inbox_move' AND j.op = 0
             WHERE a.undone = 0 GROUP BY a.id ORDER BY a.id DESC LIMIT ?1",
            [limit as i64],
            |r| {
                Ok(Batch {
                    action: r.get(0)?,
                    label: r.get(1)?,
                    at: r.get(2)?,
                    tracks: r.get::<_, i64>(3)? as usize,
                    next: r.get(4)?,
                })
            },
        )
    }

    /// What batch `action` moved, in order.
    pub fn batch_moves(&self, action: i64) -> R<Vec<Move>> {
        query_all(
            &self.db(),
            "SELECT json_extract(new, '$.track'), json_extract(new, '$.from_stage'), json_extract(new, '$.to_stage'),
                    json_extract(new, '$.from_path'), json_extract(new, '$.to_path')
             FROM journal WHERE action = ?1 AND tbl = 'inbox_move' AND op = 0 ORDER BY seq",
            [action],
            |r| {
                Ok(Move {
                    track: r.get(0)?,
                    from_stage: r.get(1)?,
                    to_stage: r.get(2)?,
                    from_path: PathBuf::from(r.get::<_, String>(3)?),
                    to_path: PathBuf::from(r.get::<_, String>(4)?),
                })
            },
        )
    }

    /// Whether the watched-folder file `sig` was offered before (`Some(track)` = linked to that row, `Some(None)` = added or
    /// skipped).
    pub fn inbox_seen(&self, sig: &str) -> R<Option<Option<i64>>> {
        scalar::<Option<i64>>(
            &self.db(),
            "SELECT track FROM inbox_seen WHERE sig = ?1",
            [sig],
        )
    }

    /// Remembers `sig` (see [`Store::inbox_seen`]); not a journal step.
    pub fn mark_inbox_seen(&self, sig: &str, link: Option<i64>) -> R<()> {
        self.system(|| {
            self.run(
                "INSERT OR REPLACE INTO inbox_seen(sig, track) VALUES (?1, ?2)",
                params![sig, link],
            )
        })
    }

    /// The row listing the file at `path` that waits in a stage (not the Collection).
    pub fn staged_at(&self, path: &Path) -> R<Option<i64>> {
        scalar(
            &self.db(),
            "SELECT id FROM collection WHERE path = ?1 AND stage IS NOT NULL",
            [path.to_string_lossy()],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(dir: &Path, name: &str) -> Track {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), name).unwrap();
        Track {
            title: name.into(),
            file: Some(dir.join(name)),
            ..Track::default()
        }
    }

    #[test]
    fn staged_rows_stay_out_of_the_collection_and_stages_can_be_added_renamed_and_deleted() {
        let dir = std::env::temp_dir().join(format!("openfader-stages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Store::in_memory().unwrap();
        let st = s.stages().unwrap();
        assert_eq!(
            st.iter()
                .map(|x| (x.name.as_str(), x.role))
                .collect::<Vec<_>>(),
            [
                ("INBOX", Role::Inbox),
                ("REVIEW", Role::Middle),
                ("COLLECTION", Role::Collection)
            ]
        );
        let ids = s
            .collection_put_staged(&[track(&dir, "a.mp3")], st[0].id)
            .unwrap();
        s.system(|| s.collection_put(&[track(&dir, "b.mp3")], false))
            .unwrap();
        assert_eq!(
            s.collection_tracks().unwrap().len(),
            1,
            "only b.mp3 is in the Collection"
        );
        assert_eq!(s.stage_tracks(st[0].id).unwrap()[0].id, Some(ids[0]));
        assert_eq!(s.stage_counts().unwrap()[&st[0].id], 1);
        assert_eq!(s.stage_counts().unwrap()[&st[2].id], 1);
        assert_eq!(s.undo().unwrap(), None, "a file arriving is no undo step");
        // A stage added later sits before the Collection; deleting it hands its tracks to the one before.
        let extra = s.add_stage("CLUB").unwrap();
        s.rename_stage(extra, "CLUB SET").unwrap();
        let names: Vec<_> = s.stages().unwrap().into_iter().map(|x| x.name).collect();
        assert_eq!(names, ["INBOX", "REVIEW", "CLUB SET", "COLLECTION"]);
        s.stage_moves(
            "Move",
            &[Move {
                track: ids[0],
                from_stage: st[0].id,
                to_stage: extra,
                from_path: dir.join("a.mp3"),
                to_path: dir.join("a.mp3"),
            }],
        )
        .unwrap();
        s.delete_stage(extra).unwrap();
        assert_eq!(s.stage_of(ids[0]).unwrap().unwrap().0, st[1].id);
        assert!(s.delete_stage(st[0].id).is_err() && s.delete_stage(st[2].id).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
