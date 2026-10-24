//! Saved searches of the media browser: a name for the search box plus the filter bar, kept as the compact text
//! `spec` the browser writes (src/app/browser/saved.rs). The store never reads `spec`, so a filter added later does
//! not touch this table.

use super::{query_all, scalar, Store, R};
use rusqlite::params;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub spec: String,
}

impl Store {
    /// Every saved search by name (case-insensitive), the older first when two names only differ in case.
    pub fn saved_searches(&self) -> R<Vec<SavedSearch>> {
        query_all(
            &self.db(),
            "SELECT id, name, spec FROM saved_search ORDER BY name COLLATE NOCASE, created, id",
            [],
            |r| {
                Ok(SavedSearch {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    spec: r.get(2)?,
                })
            },
        )
    }

    /// Saves `spec` under `name`; a saved search of that name (case-insensitive) keeps its place and gets the new `spec`.
    /// Returns whether one was replaced.
    pub fn save_search(&self, name: &str, spec: &str) -> R<bool> {
        self.write(|db| {
            let old: Option<i64> = scalar(
                db,
                "SELECT id FROM saved_search WHERE name = ?1 COLLATE NOCASE",
                [name],
            )?;
            match old {
                Some(id) => super::exec(
                    db,
                    "UPDATE saved_search SET spec = ?2 WHERE id = ?1",
                    params![id, spec],
                )?,
                None => super::exec(
                    db,
                    "INSERT INTO saved_search(name, spec, created) VALUES (?1, ?2, CAST(strftime('%s', 'now') AS INTEGER))",
                    params![name, spec],
                )?,
            }
            Ok(old.is_some())
        })
    }

    /// Gives saved search `id` a new name; `false` (nothing changed) when another one already has it.
    pub fn rename_search(&self, id: i64, name: &str) -> R<bool> {
        self.write(|db| {
            let clash: Option<i64> = scalar(
                db,
                "SELECT id FROM saved_search WHERE name = ?1 COLLATE NOCASE AND id <> ?2",
                params![name, id],
            )?;
            if clash.is_some() {
                return Ok(false);
            }
            super::exec(
                db,
                "UPDATE saved_search SET name = ?2 WHERE id = ?1",
                params![id, name],
            )?;
            Ok(true)
        })
    }

    pub fn delete_search(&self, id: i64) -> R<()> {
        self.run("DELETE FROM saved_search WHERE id = ?1", [id])
    }
}
