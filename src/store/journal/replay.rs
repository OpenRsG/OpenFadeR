use super::*;

/// Lowercase hex to bytes (the JSON form of a BLOB column).
fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks(2)
        .filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

/// `json` as column name / value pairs of `table` (names it no longer has are dropped, hex text of BLOB columns decoded).
fn fields(db: &Connection, json: &str, cols: &[Col]) -> R<Vec<(String, Value)>> {
    let all: Vec<(String, Value)> =
        query_all(db, "SELECT key, value FROM json_each(?1)", [json], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    Ok(all
        .into_iter()
        .filter_map(|(k, v)| {
            let c = cols.iter().find(|c| c.name == k)?;
            Some(match v {
                Value::Text(s) if c.blob => (k, Value::Blob(unhex(&s))),
                v => (k, v),
            })
        })
        .collect())
}

/// `WHERE` over the primary key columns of `cols` and the values to bind (from `row`).
fn by_key(cols: &[Col], row: &[(String, Value)], from: usize) -> (String, Vec<Value>) {
    let mut pk: Vec<&Col> = cols.iter().filter(|c| c.pk > 0).collect();
    pk.sort_by_key(|c| c.pk);
    let mut vals = Vec::new();
    let conds: Vec<_> = pk
        .iter()
        .enumerate()
        .map(|(i, c)| {
            vals.push(
                row.iter()
                    .find(|(k, _)| *k == c.name)
                    .map_or(Value::Null, |(_, v)| v.clone()),
            );
            format!("\"{}\" = ?{}", c.name, from + i + 1)
        })
        .collect();
    (conds.join(" AND "), vals)
}

fn insert_row(db: &Connection, table: &str, cols: &[Col], json: &str) -> R<()> {
    let row = fields(db, json, cols)?;
    let names: Vec<_> = row.iter().map(|(k, _)| format!("\"{k}\"")).collect();
    let marks: Vec<_> = (1..=row.len()).map(|i| format!("?{i}")).collect();
    exec(
        db,
        &format!(
            "INSERT OR REPLACE INTO \"{table}\"({}) VALUES ({})",
            names.join(", "),
            marks.join(", ")
        ),
        params_from_iter(row.into_iter().map(|(_, v)| v)),
    )
}

fn delete_row(db: &Connection, table: &str, cols: &[Col], json: &str) -> R<()> {
    let row = fields(db, json, cols)?;
    let (cond, vals) = by_key(cols, &row, 0);
    exec(
        db,
        &format!("DELETE FROM \"{table}\" WHERE {cond}"),
        params_from_iter(vals),
    )
}

/// Sets the columns of `set` on the row whose primary key `key` names.
fn update_row(db: &Connection, table: &str, cols: &[Col], set: &str, key: &str) -> R<()> {
    let set = fields(db, set, cols)?;
    if set.is_empty() {
        return Ok(());
    }
    let (cond, mut vals) = by_key(cols, &fields(db, key, cols)?, set.len());
    let assigns: Vec<_> = set
        .iter()
        .enumerate()
        .map(|(i, (k, _))| format!("\"{k}\" = ?{}", i + 1))
        .collect();
    let mut all: Vec<Value> = set.into_iter().map(|(_, v)| v).collect();
    all.append(&mut vals);
    exec(
        db,
        &format!("UPDATE \"{table}\" SET {} WHERE {cond}", assigns.join(", ")),
        params_from_iter(all),
    )
}

impl Store {
    /// One undo / redo step. The files the step moved ([`Store::step_file_moves`]) move first, all or none; the rows follow, and
    /// when they fail the files go back.
    pub(super) fn replay(&self, redo: bool) -> R<Option<String>> {
        let done = crate::paths::move_files(&self.step_file_moves(redo)?)?;
        let r = self.replay_rows(redo);
        if r.is_err() {
            crate::paths::move_back(&done);
        }
        r
    }

    fn replay_rows(&self, redo: bool) -> R<Option<String>> {
        let _g = system_scope();
        in_tx(&mut self.db(), true, |db| {
            let Some((id, label)) = step(db, redo)? else {
                return Ok(None);
            };
            let rows: Vec<(String, i64, Option<String>, Option<String>)> = query_all(
                db,
                if redo {
                    "SELECT tbl, op, old, new FROM journal WHERE action = ?1 ORDER BY seq"
                } else {
                    "SELECT tbl, op, old, new FROM journal WHERE action = ?1 ORDER BY seq DESC"
                },
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            for (t, op, old, new) in &rows {
                let cols = columns(db, t)?;
                let (old, new) = (
                    old.as_deref().unwrap_or("{}"),
                    new.as_deref().unwrap_or("{}"),
                );
                match (op, redo) {
                    (0, false) | (2, true) => {
                        delete_row(db, t, &cols, if *op == 0 { new } else { old })?
                    }
                    (0, true) | (2, false) => {
                        insert_row(db, t, &cols, if *op == 0 { new } else { old })?
                    }
                    (_, false) => update_row(db, t, &cols, old, new)?,
                    (_, true) => update_row(db, t, &cols, new, old)?,
                }
            }
            exec(
                db,
                "UPDATE journal_action SET undone = ?2 WHERE id = ?1",
                [id, i64::from(!redo)],
            )?;
            exec(
                db,
                "INSERT INTO journal(action, tbl, op, old, new) SELECT 0, tbl, 1, old, new FROM journal WHERE action = ?1",
                [id],
            )?;
            exec(
                db,
                "DELETE FROM journal WHERE action = 0 AND seq NOT IN
                 (SELECT seq FROM journal WHERE action = 0 ORDER BY seq DESC LIMIT ?1)",
                [MARKERS],
            )?;
            label_of(db, id, label).map(Some)
        })
    }
}
