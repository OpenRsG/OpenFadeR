//! Change journal (child of [`super`]): every user edit of the Collection's data is recorded row by row, for undo / redo
//! and for write-back to a connected library.
//!
//! Recording is done by SQL: at open, one TEMP trigger per table and operation is generated from `PRAGMA table_info`
//! ([`TABLES`]; history, devices and the connection tables are not user data). A trigger writes the old / new row as JSON
//! into `journal` (an UPDATE records only the changed columns plus the primary key; BLOB columns are hex) under the
//! action of the calling thread. The action is created by its first change: an autocommit statement or a transaction
//! is one action, [`Store::action`] groups several calls under one label. [`Store::system`] turns recording off for
//! the calling thread (sync, analysis results, undo / redo themselves). Rows pruned beyond [`KEEP`] actions; a new
//! action after an undo drops the undone tail.
//!
//! Undo / redo replay the rows inside one transaction with foreign keys deferred and append marker rows (action 0) so
//! [`Store::changed_since`] reports what they touched.

mod record;
mod replay;
#[cfg(test)]
mod tests;

pub(in crate::store) use record::install;

use super::{R, Sql, Store, exec, in_tx, query_all, query_opt, scalar};
use rusqlite::{Connection, functions::FunctionFlags, params_from_iter, types::Value};
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc,
    },
};

/// Actions kept (older ones are deleted with their rows when a new one starts).
const KEEP: i64 = 1000;
/// Actions kept at most while a two-way connection has not written their rows back yet (see the pruning trigger).
// ponytail: a connection that never writes back keeps 50k actions of rows at most; then the oldest go anyway.
const HARD: i64 = 50_000;
/// Undo / redo marker rows kept.
const MARKERS: i64 = 1000;

/// User data: the tables whose changes are journaled.
const TABLES: &[&str] = &[
    "artwork",
    "track",
    "cue",
    "node",
    "entry",
    "mytag_category",
    "mytag",
    "track_mytag",
    "hotcue_bank",
    "hotcue_bank_cue",
    "matching",
    "phrase_edit",
    "gain",
    "sequence",
    "collection",
    "removed",
    "taglist",
    "source_map",
    "source_node",
    "analysis_lock",
    "history",
    "stage",
    "inbox_move",
];

/// Tables whose primary key starts with the track key (see [`Store::changed_since`]).
pub(super) const KEYED: &[&str] = &["track", "cue", "track_mytag", "gain", "phrase_edit"];

struct Group {
    label: String,
    id: Option<i64>,
}

/// What the calling thread's writes are recorded as.
#[derive(Default)]
struct Scope {
    /// Nesting depth of [`Store::system`]; recording is off while above 0.
    system: u32,
    /// The open [`Store::action`].
    group: Option<Group>,
    /// The action of the statement / transaction being committed (cleared by the commit and rollback hooks).
    auto: Option<i64>,
}

thread_local! {
    static SCOPE: RefCell<Scope> = RefCell::default();
}

fn clear_auto() {
    SCOPE.with(|s| s.borrow_mut().auto = None);
}

/// Write transactions committed in this process (any store): a cheap "something changed" signal for the Edit menu.
static COMMITS: AtomicU64 = AtomicU64::new(0);

/// Restores the scope when a [`Store::system`] / [`Store::action`] closure ends (also by panic).
struct Restore(bool);

impl Drop for Restore {
    fn drop(&mut self) {
        SCOPE.with(|s| {
            let mut s = s.borrow_mut();
            if self.0 {
                s.system -= 1;
            } else {
                s.group = None;
            }
        });
    }
}

fn system_scope() -> Restore {
    SCOPE.with(|s| s.borrow_mut().system += 1);
    Restore(true)
}

/// A column of a journaled table.
struct Col {
    name: String,
    blob: bool,
    pk: i64,
}

fn columns(db: &Connection, table: &str) -> R<Vec<Col>> {
    query_all(db, &format!("PRAGMA table_info(\"{table}\")"), [], |r| {
        Ok(Col {
            name: r.get(1)?,
            blob: r.get::<_, String>(2)?.eq_ignore_ascii_case("BLOB"),
            pk: r.get(5)?,
        })
    })
}

/// What one journal row says about the user's change, in words ("rating", "cues", ...).
fn noun(table: &str, op: i64, old: Option<&str>, new: Option<&str>) -> &'static str {
    match table {
        "track" => {
            // An update lists the changed columns; an insert / delete the row: what it sets is what changed.
            let set: Vec<String> = new
                .or(old)
                .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
                .and_then(|v| v.as_object().cloned())
                .map(|o| {
                    o.into_iter()
                        .filter(|(_, v)| op == 1 || !(v.is_null() || v.as_i64() == Some(0)))
                        .map(|(k, _)| k)
                        .collect()
                })
                .unwrap_or_default();
            let has = |c: &str| set.iter().any(|x| x == c);
            if ["title", "artist", "album", "genre", "label"]
                .iter()
                .any(|c| has(c))
            {
                return "metadata";
            }
            if has("rating") {
                "rating"
            } else if has("comment") {
                "comment"
            } else if has("color") {
                "track colour"
            } else if has("grid") {
                "beat grid"
            } else {
                "cues"
            }
        }
        "cue" => "cues",
        "node" | "entry" => "playlist",
        "mytag_category" | "mytag" | "track_mytag" => "tags",
        "hotcue_bank" | "hotcue_bank_cue" => "cue banks",
        "matching" => "matching",
        "analysis_lock" => "analysis lock",
        "phrase_edit" => "phrases",
        "artwork" => "artwork",
        "gain" => "gain",
        "sequence" => "sampler sequence",
        "collection" | "removed" => "collection",
        "taglist" => "tag list",
        _ => "library",
    }
}

/// The action's own label, or "Edit <nouns>" from the rows it recorded.
fn label_of(db: &Connection, id: i64, label: String) -> R<String> {
    if !label.is_empty() {
        return Ok(label);
    }
    let rows: Vec<(String, i64, Option<String>, Option<String>)> = query_all(
        db,
        "SELECT tbl, op, old, new FROM journal WHERE action = ?1 ORDER BY seq LIMIT 50",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut nouns: Vec<&str> = Vec::new();
    for (t, op, old, new) in &rows {
        let n = noun(t, *op, old.as_deref(), new.as_deref());
        if !nouns.contains(&n) {
            nouns.push(n);
        }
    }
    nouns.truncate(3);
    Ok(if nouns.is_empty() {
        "Edit".into()
    } else {
        format!("Edit {}", nouns.join(", "))
    })
}

impl Store {
    /// Runs `f` with the journal off for this thread: its writes are not undo steps (sync, imports from a connected
    /// library, analysis results). Nests; threads `f` spawns are not covered.
    pub fn system<T>(&self, f: impl FnOnce() -> T) -> T {
        let _g = system_scope();
        f()
    }

    /// Runs `f` so every write it makes on this thread is one undo step called `label` (nested calls join the outer one).
    pub fn action<T>(&self, label: &str, f: impl FnOnce() -> T) -> T {
        let own = SCOPE.with(|s| {
            let mut s = s.borrow_mut();
            let own = s.group.is_none();
            if own {
                s.group = Some(Group {
                    label: label.to_owned(),
                    id: None,
                });
            }
            own
        });
        let _g = own.then_some(Restore(false));
        f()
    }

    /// Changes with every write transaction committed by any store in this process (what the Edit menu polls to refresh its labels).
    pub fn journal_tick() -> u64 {
        COMMITS.load(Ordering::Relaxed)
    }

    /// The last journal row's `seq` (0 when empty). Undo / redo append rows, so it only grows.
    pub fn journal_seq(&self) -> R<i64> {
        Ok(
            scalar::<i64>(&self.db(), "SELECT COALESCE(MAX(seq), 0) FROM journal", [])?
                .unwrap_or(0),
        )
    }

    /// The `(table, key)` pairs user edits (and undo / redo) touched after `seq`, each once. The key is `t<id>` for
    /// `collection`, the track key for `track`, `cue`, `track_mytag`, `gain` and `phrase_edit`, `None` for the others.
    pub fn changed_since(&self, seq: i64) -> R<Vec<(String, Option<String>)>> {
        let rows: Vec<(String, Option<String>, Option<i64>)> = query_all(
            &self.db(),
            "SELECT DISTINCT tbl,
                    COALESCE(json_extract(new, '$.key'), json_extract(old, '$.key')),
                    COALESCE(json_extract(new, '$.id'), json_extract(old, '$.id'))
             FROM journal WHERE seq > ?1 ORDER BY seq",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut out: Vec<(String, Option<String>)> = Vec::new();
        for (t, key, id) in rows {
            let key = match t.as_str() {
                "collection" => id.map(|i| format!("t{i}")),
                t if KEYED.contains(&t) => key,
                _ => None,
            };
            if !out.iter().any(|(ot, ok)| *ot == t && *ok == key) {
                out.push((t, key));
            }
        }
        Ok(out)
    }

    /// The Collection ids of the tracks touched after `seq` ([`Store::changed_since`] keys of the form `t<id>`).
    pub fn journal_touched_since(&self, seq: i64) -> R<Vec<i64>> {
        let mut ids: Vec<i64> = self
            .changed_since(seq)?
            .into_iter()
            .filter_map(|(_, k)| k?.strip_prefix('t')?.parse().ok())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// The label of the step [`Store::undo`] would undo (`None` when there is none).
    pub fn undo_label(&self) -> R<Option<String>> {
        self.step_label(false)
    }

    /// The label of the step [`Store::redo`] would redo.
    pub fn redo_label(&self) -> R<Option<String>> {
        self.step_label(true)
    }

    fn step_label(&self, redo: bool) -> R<Option<String>> {
        let db = self.db();
        Ok(match step(&db, redo)? {
            Some((id, label)) => Some(label_of(&db, id, label)?),
            None => None,
        })
    }

    /// The file moves `(from, to)` the step that undo (`redo` false) or redo would replay recorded (`inbox_move` rows whose
    /// file moved), already turned the way the replay goes: an undo walks them last to first, from `to` back to `from`.
    pub(super) fn step_file_moves(&self, redo: bool) -> R<Vec<(PathBuf, PathBuf)>> {
        let db = self.db();
        let Some((id, _)) = step(&db, redo)? else {
            return Ok(Vec::new());
        };
        let mut moves: Vec<(PathBuf, PathBuf)> = query_all(
            &db,
            "SELECT json_extract(new, '$.from_path'), json_extract(new, '$.to_path') FROM journal
             WHERE action = ?1 AND tbl = 'inbox_move' AND op = 0
               AND json_extract(new, '$.from_path') != json_extract(new, '$.to_path') ORDER BY seq",
            [id],
            |r| {
                Ok((
                    PathBuf::from(r.get::<_, String>(0)?),
                    PathBuf::from(r.get::<_, String>(1)?),
                ))
            },
        )?;
        if !redo {
            moves.reverse();
            moves
                .iter_mut()
                .for_each(|m| *m = (m.1.clone(), m.0.clone()));
        }
        Ok(moves)
    }

    /// Undoes the latest step; its label, or `None` when there is nothing to undo.
    pub fn undo(&self) -> R<Option<String>> {
        self.replay(false)
    }

    /// Redoes the step undone last; its label, or `None`.
    pub fn redo(&self) -> R<Option<String>> {
        self.replay(true)
    }
}

/// The step undo (`redo` false: the newest not undone) or redo (the oldest undone) would replay.
fn step(db: &Connection, redo: bool) -> R<Option<(i64, String)>> {
    query_opt(
        db,
        if redo {
            "SELECT id, label FROM journal_action WHERE undone = 1 ORDER BY id LIMIT 1"
        } else {
            "SELECT id, label FROM journal_action WHERE id > 0 AND undone = 0 ORDER BY id DESC LIMIT 1"
        },
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}
