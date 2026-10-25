//! The way back from an import (`import_undo`): UPDATE COLLECTION, IMPORT HISTORIES and a cue sync that overwrote a copy each leave
//! one entry holding what they changed, as the JSON text [`crate::device::undo`] writes. The last [`KEEP`] entries stay across
//! runs; an entry is deleted once it was undone. The store only keeps them; what is in one is the device module's business.

use super::{exec, query_all, Store, R};
use rusqlite::params;

/// Entries kept; a newer one pushes the oldest out.
pub const KEEP: i64 = 5;

/// One kept import.
#[derive(Clone, Debug, PartialEq)]
pub struct UndoEntry {
    pub id: i64,
    /// Unix seconds.
    pub at: i64,
    /// "Update Collection of KEY", for the lists.
    pub label: String,
    /// [`crate::device::Device::id`] it came from ("" none).
    pub device: String,
    pub data: String,
}

impl Store {
    /// Keeps a new entry (the oldest beyond [`KEEP`] is dropped); its id.
    pub fn undo_push(&self, label: &str, device: &str, data: &str, now: i64) -> R<i64> {
        self.system(|| {
            self.write(|tx| {
                exec(
                    tx,
                    "INSERT INTO import_undo(at, label, device, data) VALUES (?1, ?2, ?3, ?4)",
                    params![now, label, device, data],
                )?;
                let id = tx.last_insert_rowid();
                exec(
                    tx,
                    "DELETE FROM import_undo WHERE id NOT IN (SELECT id FROM import_undo ORDER BY id DESC LIMIT ?1)",
                    [KEEP],
                )?;
                Ok(id)
            })
        })
    }

    /// The kept entries, newest first.
    pub fn undo_entries(&self) -> R<Vec<UndoEntry>> {
        query_all(
            &self.db(),
            "SELECT id, at, label, device, data FROM import_undo ORDER BY id DESC",
            [],
            |r| {
                Ok(UndoEntry {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    label: r.get(2)?,
                    device: r.get(3)?,
                    data: r.get(4)?,
                })
            },
        )
    }

    /// Forgets entry `id` (it was undone).
    pub fn undo_forget(&self, id: i64) -> R<()> {
        self.system(|| self.run("DELETE FROM import_undo WHERE id = ?1", [id]))
    }

    /// Takes plays back out of the history: one row per `(day, key, played_at)`.
    pub fn history_remove(&self, plays: &[(String, String, i64)]) -> R<usize> {
        self.system(|| {
            self.write(|tx| {
                let mut n = 0;
                for (day, key, at) in plays {
                    n += super::exec_count(
                        tx,
                        "DELETE FROM history WHERE id = (SELECT id FROM history WHERE day = ?1 AND key = ?2 AND played_at = ?3 LIMIT 1)",
                        params![day, key, at],
                    )?;
                }
                Ok(n)
            })
        })
    }

    /// Forgets that the history `history` of `device` was imported (it can be imported again).
    pub fn history_unmark_imported(&self, device: &str, history: &str) -> R<()> {
        self.run(
            "DELETE FROM history_import WHERE device = ?1 AND history = ?2",
            params![device, history],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_five_stay_newest_first() {
        let s = Store::in_memory().unwrap();
        for i in 0..7 {
            s.undo_push(&format!("l{i}"), "d", "{}", i).unwrap();
        }
        let e = s.undo_entries().unwrap();
        assert_eq!(
            e.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
            ["l6", "l5", "l4", "l3", "l2"]
        );
        s.undo_forget(e[0].id).unwrap();
        assert_eq!(s.undo_entries().unwrap().len(), 4);
    }
}
