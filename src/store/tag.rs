//! Tags: user categories, the tags in them and which tracks carry which tag. No tags are pre-seeded.

use super::{R, Sql, Store, column, query_all};
use rusqlite::params;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct TagGroup {
    pub id: i64,
    pub name: String,
    pub pos: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub id: i64,
    pub category: i64,
    pub name: String,
    pub pos: i64,
}

impl Store {
    pub fn mytag_categories(&self) -> R<Vec<TagGroup>> {
        query_all(
            &self.db(),
            "SELECT id, name, pos FROM mytag_category ORDER BY pos, id",
            [],
            |r| {
                Ok(TagGroup {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    pos: r.get(2)?,
                })
            },
        )
    }

    pub fn add_mytag_category(&self, name: &str) -> R<i64> {
        self.insert(
            "INSERT INTO mytag_category(name, pos) VALUES (?1, (SELECT COALESCE(MAX(pos) + 1, 0) FROM mytag_category))",
            [name],
        )
    }

    pub fn rename_mytag_category(&self, id: i64, name: &str) -> R<()> {
        self.run(
            "UPDATE mytag_category SET name = ?2 WHERE id = ?1",
            params![id, name],
        )
    }

    /// Deletes the category, its tags and their assignments.
    pub fn delete_mytag_category(&self, id: i64) -> R<()> {
        self.run("DELETE FROM mytag_category WHERE id = ?1", [id])
    }

    /// Every tag, by category then position.
    pub fn mytags(&self) -> R<Vec<Tag>> {
        query_all(
            &self.db(),
            "SELECT t.id, t.category, t.name, t.pos FROM mytag t JOIN mytag_category c ON c.id = t.category ORDER BY c.pos, c.id, t.pos, t.id",
            [],
            |r| {
                Ok(Tag {
                    id: r.get(0)?,
                    category: r.get(1)?,
                    name: r.get(2)?,
                    pos: r.get(3)?,
                })
            },
        )
    }

    pub fn add_mytag(&self, category: i64, name: &str) -> R<i64> {
        self.insert(
            "INSERT INTO mytag(category, name, pos) VALUES (?1, ?2, (SELECT COALESCE(MAX(pos) + 1, 0) FROM mytag WHERE category = ?1))",
            params![category, name],
        )
    }

    pub fn rename_mytag(&self, id: i64, name: &str) -> R<()> {
        self.run(
            "UPDATE mytag SET name = ?2 WHERE id = ?1",
            params![id, name],
        )
    }

    pub fn delete_mytag(&self, id: i64) -> R<()> {
        self.run("DELETE FROM mytag WHERE id = ?1", [id])
    }

    /// Tag ids of one track.
    pub fn track_mytags(&self, key: &str) -> R<Vec<i64>> {
        column(
            &self.db(),
            "SELECT tag FROM track_mytag WHERE key = ?1 ORDER BY tag",
            [key],
        )
    }

    pub fn set_track_mytag(&self, key: &str, tag: i64, on: bool) -> R<()> {
        self.run(
            if on {
                "INSERT OR IGNORE INTO track_mytag(key, tag) VALUES (?1, ?2)"
            } else {
                "DELETE FROM track_mytag WHERE key = ?1 AND tag = ?2"
            },
            params![key, tag],
        )
    }

    /// Tag ids of every tagged track, by key (for filtering a track list).
    pub fn all_track_mytags(&self) -> R<HashMap<String, Vec<i64>>> {
        let db = self.db();
        // The primary key's order: a track's rows come together, its tags in ascending order; one String per track, no sort.
        let mut st = db
            .prepare_cached("SELECT key, tag FROM track_mytag ORDER BY key, tag")
            .s()?;
        let mut out: HashMap<String, Vec<i64>> = HashMap::new();
        let (mut key, mut tags) = (String::new(), Vec::new());
        let mut rows = st.query([]).s()?;
        while let Some(r) = rows.next().s()? {
            let k = r.get_ref(0).s()?.as_str().map_err(|e| e.to_string())?;
            if k != key {
                if !tags.is_empty() {
                    out.insert(std::mem::take(&mut key), std::mem::take(&mut tags));
                }
                key = k.to_owned();
            }
            tags.push(r.get(1).s()?);
        }
        if !tags.is_empty() {
            out.insert(key, tags);
        }
        Ok(out)
    }
}
