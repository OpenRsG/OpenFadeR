//! Connections: the libraries of other apps (and plain folders) synced into our Collection ([`crate::library::connect`]).
//! One `connection` row per source, `source_map` (the source's track id → our `collection.id` with the hash of what the
//! source last said) and `source_node` (the source's playlist id → our playlist node). Deleting a connection drops its maps
//! (foreign keys cascade); the tracks stay in the Collection.

use super::{column, exec, exec_count, query_all, scalar, Store, R};
use rusqlite::params;
use std::collections::HashMap;
use std::path::PathBuf;

/// Whether a connection only reads its source or also writes our edits back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Import = 0,
    TwoWay = 1,
}

/// Which side wins when a track changed in the source and was edited here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wins {
    Source = 0,
    Ours = 1,
    Newest = 2,
}

impl Mode {
    fn of(n: i64) -> Self {
        if n == 1 {
            Mode::TwoWay
        } else {
            Mode::Import
        }
    }
}

impl Wins {
    fn of(n: i64) -> Self {
        match n {
            1 => Wins::Ours,
            2 => Wins::Newest,
            _ => Wins::Source,
        }
    }
}

/// One `connection` row. `plugin` is "" for a plain folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub id: i64,
    pub plugin: String,
    pub path: PathBuf,
    pub label: String,
    pub mode: Mode,
    pub wins: Wins,
    /// Unix seconds of the last finished sync, 0 = never.
    pub last_sync: i64,
    /// The source's state at the last sync ([`crate::library::connect::stamp`]).
    pub stamp: String,
    /// The journal position written back last.
    pub last_seq: i64,
}

const COLS: &str = "id, plugin, path, label, mode, wins, last_sync, stamp, last_seq";

impl Store {
    pub fn connections(&self) -> R<Vec<Conn>> {
        query_all(
            &self.db(),
            &format!("SELECT {COLS} FROM connection ORDER BY id"),
            [],
            |r| {
                Ok(Conn {
                    id: r.get(0)?,
                    plugin: r.get(1)?,
                    path: PathBuf::from(r.get::<_, String>(2)?),
                    label: r.get(3)?,
                    mode: Mode::of(r.get(4)?),
                    wins: Wins::of(r.get(5)?),
                    last_sync: r.get(6)?,
                    stamp: r.get(7)?,
                    last_seq: r.get(8)?,
                })
            },
        )
    }

    /// The connection of `plugin` at `path`, made when there is none; `last_seq` starts at `seq` (edits before the
    /// connection never count as pending write-back).
    pub fn connection_add(
        &self,
        plugin: &str,
        path: &std::path::Path,
        label: &str,
        seq: i64,
    ) -> R<Conn> {
        let p = path.to_string_lossy();
        self.run(
            "INSERT OR IGNORE INTO connection(plugin, path, label, last_seq) VALUES (?1, ?2, ?3, ?4)",
            params![plugin, p.as_ref(), label, seq],
        )?;
        self.connections()?
            .into_iter()
            .find(|c| c.plugin == plugin && c.path == path)
            .ok_or_else(|| "connection vanished".into())
    }

    pub fn connection_set(&self, id: i64, mode: Mode, wins: Wins) -> R<()> {
        self.run(
            "UPDATE connection SET mode = ?2, wins = ?3 WHERE id = ?1",
            params![id, mode as i64, wins as i64],
        )
    }

    /// A sync finished at `at` (unix s) with the source in state `stamp`; `seq` (when given) is the journal position
    /// the source now holds.
    pub fn connection_synced(&self, id: i64, stamp: &str, at: i64, seq: Option<i64>) -> R<()> {
        self.run(
            "UPDATE connection SET stamp = ?2, last_sync = ?3, last_seq = COALESCE(?4, last_seq) WHERE id = ?1",
            params![id, stamp, at, seq],
        )
    }

    /// Drops the connection with its track and playlist maps; the tracks and playlists stay. Not an undo step (the maps
    /// it takes along could not come back without it).
    pub fn connection_delete(&self, id: i64) -> R<()> {
        self.system(|| self.run("DELETE FROM connection WHERE id = ?1", [id]))
    }

    /// How many tracks of the Collection the connection maps.
    pub fn connection_tracks(&self, id: i64) -> R<usize> {
        Ok(scalar::<i64>(
            &self.db(),
            "SELECT COUNT(*) FROM source_map WHERE conn = ?1",
            [id],
        )?
        .unwrap_or(0) as usize)
    }

    /// The source's track id → (our track id, sig).
    pub fn source_map(&self, conn: i64) -> R<HashMap<String, (i64, i64)>> {
        query_all(
            &self.db(),
            "SELECT foreign_id, track, sig FROM source_map WHERE conn = ?1",
            [conn],
            |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))),
        )
    }

    pub fn map_put(&self, conn: i64, foreign: &str, track: i64, sig: i64) -> R<()> {
        self.run(
            "INSERT INTO source_map(conn, foreign_id, track, sig) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(conn, foreign_id) DO UPDATE SET track = ?3, sig = ?4",
            params![conn, foreign, track, sig],
        )
    }

    /// The map rows of `foreign` ids that left the source.
    pub fn map_drop(&self, conn: i64, foreign: &[&String]) -> R<()> {
        self.write(|tx| {
            for f in foreign {
                exec(
                    tx,
                    "DELETE FROM source_map WHERE conn = ?1 AND foreign_id = ?2",
                    params![conn, f],
                )?;
            }
            Ok(())
        })
    }

    /// Our track ids the connection maps.
    pub fn mapped_ids(&self, conn: i64) -> R<Vec<i64>> {
        column(
            &self.db(),
            "SELECT track FROM source_map WHERE conn = ?1",
            [conn],
        )
    }

    /// The source's playlist id → our node.
    pub fn source_nodes(&self, conn: i64) -> R<HashMap<String, i64>> {
        query_all(
            &self.db(),
            "SELECT foreign_id, node FROM source_node WHERE conn = ?1",
            [conn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn node_map_put(&self, conn: i64, foreign: &str, node: i64) -> R<()> {
        self.run(
            "INSERT OR REPLACE INTO source_node(conn, foreign_id, node) VALUES (?1, ?2, ?3)",
            params![conn, foreign, node],
        )
    }

    /// Per track key (`t<id>`, as [`Store::changed_since`] names them): when (unix s) the user last changed it after
    /// journal position `seq`. Undo / redo markers carry no time and are not listed.
    pub fn last_edits_since(&self, seq: i64) -> R<HashMap<String, i64>> {
        let rows: Vec<(String, Option<String>, Option<i64>, i64)> = query_all(
            &self.db(),
            "SELECT j.tbl,
                    COALESCE(json_extract(j.new, '$.key'), json_extract(j.old, '$.key')),
                    COALESCE(json_extract(j.new, '$.id'), json_extract(j.old, '$.id')),
                    MAX(a.at)
             FROM journal j JOIN journal_action a ON a.id = j.action
             WHERE j.seq > ?1 GROUP BY 1, 2, 3",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let mut out: HashMap<String, i64> = HashMap::new();
        for (tbl, key, id, at) in rows {
            let key = match tbl.as_str() {
                "collection" => id.map(|i| format!("t{i}")),
                t if super::journal::KEYED.contains(&t) => key,
                _ => None,
            };
            if let Some(k) = key {
                let e = out.entry(k).or_insert(at);
                *e = (*e).max(at);
            }
        }
        Ok(out)
    }

    /// Deletes the nodes of `conn` whose foreign id is not in `keep` (their entries go with them) and their map rows;
    /// returns how many.
    pub fn node_map_prune(&self, conn: i64, keep: &[&str]) -> R<usize> {
        let gone: Vec<(String, i64)> = query_all(
            &self.db(),
            "SELECT foreign_id, node FROM source_node WHERE conn = ?1",
            [conn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut n = 0;
        for (_, node) in gone.iter().filter(|(f, _)| !keep.contains(&f.as_str())) {
            // a deleted folder takes its children along: count only what was still there
            n += exec_count(&self.db(), "DELETE FROM node WHERE id = ?1", [node])?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_roundtrip_and_delete_keeps_tracks() {
        let s = Store::in_memory().unwrap();
        let c = s
            .connection_add("p", std::path::Path::new("/lib.xml"), "P", 7)
            .unwrap();
        assert_eq!(
            (c.mode, c.wins, c.last_seq),
            (Mode::Import, Wins::Source, 7)
        );
        assert_eq!(
            s.connection_add("p", std::path::Path::new("/lib.xml"), "Q", 9)
                .unwrap()
                .id,
            c.id
        );
        s.connection_set(c.id, Mode::TwoWay, Wins::Newest).unwrap();
        s.connection_synced(c.id, "st", 5, None).unwrap();
        let c = &s.connections().unwrap()[0];
        assert_eq!(
            (c.mode, c.wins, c.stamp.as_str(), c.last_seq),
            (Mode::TwoWay, Wins::Newest, "st", 7)
        );
        s.connection_delete(c.id).unwrap();
        assert!(s.connections().unwrap().is_empty());
    }
}

#[cfg(test)]
mod edit_time_tests {
    use super::*;

    #[test]
    fn last_edits_are_kept_per_track() {
        let s = Store::in_memory().unwrap();
        let seq = s.journal_seq().unwrap();
        s.set_rating("t1", Some(2)).unwrap();
        s.set_rating("t2", Some(3)).unwrap();
        let m = s.last_edits_since(seq).unwrap();
        assert!(m.contains_key("t1") && m.contains_key("t2"));
        assert_eq!(
            s.last_edits_since(s.journal_seq().unwrap()).unwrap().len(),
            0
        );
    }
}
