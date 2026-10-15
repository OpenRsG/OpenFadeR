//! User playlists: a tree of folders, playlists and smart playlists (`node`), and each playlist's
//! ordered track keys (`entry`). Smart playlist criteria are opaque text owned by the browser.

use super::{R, Sql, Store, column, exec, query_all, scalar};
use rusqlite::{Connection, params};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Folder,
    Playlist,
    Smart,
}

impl NodeKind {
    fn code(self) -> i64 {
        self as i64
    }

    fn from_code(c: i64) -> Self {
        match c {
            0 => Self::Folder,
            2 => Self::Smart,
            _ => Self::Playlist,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: i64,
    pub parent: Option<i64>,
    pub name: String,
    pub kind: NodeKind,
    /// Index among its siblings.
    pub pos: i64,
}

/// Appends `keys` after the last entry of playlist `id`.
fn push_entries(db: &Connection, id: i64, keys: &[String]) -> R<()> {
    let mut pos: i64 = scalar(
        db,
        "SELECT COALESCE(MAX(pos) + 1, 0) FROM entry WHERE node = ?1",
        [id],
    )?
    .unwrap_or(0);
    let mut ins = db
        .prepare_cached("INSERT INTO entry(node, pos, key) VALUES (?1, ?2, ?3)")
        .s()?;
    for k in keys {
        ins.execute(params![id, pos, k]).s()?;
        pos += 1;
    }
    Ok(())
}

impl Store {
    /// Every node, depth-first, siblings by `pos`.
    pub fn nodes(&self) -> R<Vec<Node>> {
        let all: Vec<Node> = query_all(
            &self.db(),
            "SELECT id, parent, name, kind, pos FROM node ORDER BY pos, id",
            [],
            |r| {
                Ok(Node {
                    id: r.get(0)?,
                    parent: r.get(1)?,
                    name: r.get(2)?,
                    kind: NodeKind::from_code(r.get(3)?),
                    pos: r.get(4)?,
                })
            },
        )?;
        let mut kids: HashMap<Option<i64>, Vec<Node>> = HashMap::new();
        for n in all {
            kids.entry(n.parent).or_default().push(n);
        }
        let mut out = Vec::new();
        let mut stack: Vec<Node> = kids
            .remove(&None)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .collect();
        while let Some(n) = stack.pop() {
            stack.extend(
                kids.remove(&Some(n.id))
                    .unwrap_or_default()
                    .into_iter()
                    .rev(),
            );
            out.push(n);
        }
        Ok(out)
    }

    /// New node after its last sibling.
    pub fn add_node(&self, parent: Option<i64>, name: &str, kind: NodeKind) -> R<i64> {
        self.insert(
            "INSERT INTO node(parent, name, kind, pos) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(pos) + 1, 0) FROM node WHERE parent IS ?1))",
            params![parent, name, kind.code()],
        )
    }

    pub fn rename_node(&self, id: i64, name: &str) -> R<()> {
        self.run("UPDATE node SET name = ?2 WHERE id = ?1", params![id, name])
    }

    /// Deletes the node with its children and entries.
    pub fn delete_node(&self, id: i64) -> R<()> {
        self.run("DELETE FROM node WHERE id = ?1", [id])
    }

    /// Moves `id` under `new_parent` (`None` = top level) at index `pos` among its new siblings (clamped).
    /// Moving a node into its own subtree is an error.
    pub fn move_node(&self, id: i64, new_parent: Option<i64>, pos: i64) -> R<()> {
        self.write(|tx| {
            let mut up = new_parent;
            while let Some(p) = up {
                if p == id {
                    return Err("cannot move a folder into itself".into());
                }
                up = scalar::<Option<i64>>(tx, "SELECT parent FROM node WHERE id = ?1", [p])?
                    .flatten();
            }
            let mut sibs: Vec<i64> = column(
                tx,
                "SELECT id FROM node WHERE parent IS ?1 AND id != ?2 ORDER BY pos, id",
                params![new_parent, id],
            )?;
            sibs.insert(pos.clamp(0, sibs.len() as i64) as usize, id);
            exec(
                tx,
                "UPDATE node SET parent = ?2 WHERE id = ?1",
                params![id, new_parent],
            )?;
            let mut set_pos = tx
                .prepare_cached("UPDATE node SET pos = ?2 WHERE id = ?1")
                .s()?;
            for (i, n) in sibs.iter().enumerate() {
                set_pos.execute(params![n, i as i64]).s()?;
            }
            Ok(())
        })
    }

    pub fn set_smart(&self, id: i64, criteria: &str) -> R<()> {
        self.run(
            "UPDATE node SET smart = ?2 WHERE id = ?1",
            params![id, criteria],
        )
    }

    pub fn smart(&self, id: i64) -> R<Option<String>> {
        Ok(
            scalar::<Option<String>>(&self.db(), "SELECT smart FROM node WHERE id = ?1", [id])?
                .flatten(),
        )
    }

    /// Track keys of a playlist in order.
    pub fn entries(&self, id: i64) -> R<Vec<String>> {
        column(
            &self.db(),
            "SELECT key FROM entry WHERE node = ?1 ORDER BY pos",
            [id],
        )
    }

    /// Replaces a playlist's entries.
    pub fn set_entries(&self, id: i64, keys: &[String]) -> R<()> {
        self.write(|tx| {
            exec(tx, "DELETE FROM entry WHERE node = ?1", [id])?;
            push_entries(tx, id, keys)
        })
    }

    /// Appends `keys` to a playlist; the entries already there are not rewritten.
    pub fn add_entries(&self, id: i64, keys: &[String]) -> R<()> {
        self.write(|tx| push_entries(tx, id, keys))
    }

    pub fn remove_entry(&self, id: i64, index: usize) -> R<()> {
        let mut all = self.entries(id)?;
        if index < all.len() {
            all.remove(index);
            self.set_entries(id, &all)?;
        }
        Ok(())
    }

    /// Entry `from` moves to index `to` (clamped), the ones between shift.
    pub fn move_entry(&self, id: i64, from: usize, to: usize) -> R<()> {
        let mut all = self.entries(id)?;
        if from < all.len() {
            let k = all.remove(from);
            all.insert(to.min(all.len()), k);
            self.set_entries(id, &all)?;
        }
        Ok(())
    }
}
