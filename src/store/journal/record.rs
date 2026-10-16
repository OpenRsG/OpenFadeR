use super::*;

/// The value of `row.col` as JSON stores it (a BLOB as hex text).
fn val(row: &str, c: &Col) -> String {
    let v = format!("{row}.\"{}\"", c.name);
    if c.blob {
        format!("CASE WHEN {v} IS NULL THEN NULL ELSE hex({v}) END")
    } else {
        v
    }
}

/// `json_object` of every column of `row` (`OLD` / `NEW`).
fn object(row: &str, cols: &[Col]) -> String {
    let pairs: Vec<_> = cols
        .iter()
        .map(|c| format!("'{}', {}", c.name, val(row, c)))
        .collect();
    format!("json_object({})", pairs.join(", "))
}

/// JSON of the columns of `row` that differ between `OLD` and `NEW`, plus the primary key.
fn changed(row: &str, cols: &[Col]) -> String {
    let parts: Vec<_> = cols
        .iter()
        .map(|c| {
            let cond = if c.pk > 0 {
                String::new()
            } else {
                format!(" WHERE OLD.\"{0}\" IS NOT NEW.\"{0}\"", c.name)
            };
            format!("SELECT '{}' AS k, {} AS v{cond}", c.name, val(row, c))
        })
        .collect();
    format!(
        "(SELECT json_group_object(k, v) FROM ({}))",
        parts.join(" UNION ALL ")
    )
}

/// Registers the SQL functions and hooks and creates the TEMP triggers on `conn` (after the migrations).
pub(in crate::store) fn install(conn: &Connection) -> R<()> {
    let next = Arc::new(AtomicI64::new(
        scalar::<i64>(
            conn,
            "SELECT COALESCE(MAX(id), 0) + 1 FROM journal_action",
            [],
        )?
        .unwrap_or(1),
    ));
    let f = FunctionFlags::SQLITE_UTF8;
    conn.create_scalar_function(
        "jr_on",
        0,
        f,
        |_| Ok(SCOPE.with(|s| s.borrow().system == 0)),
    )
    .s()?;
    conn.create_scalar_function("jr_action", 0, f, move |_| {
        Ok(SCOPE.with(|s| {
            let mut s = s.borrow_mut();
            let s = &mut *s;
            let slot = match &mut s.group {
                Some(g) => &mut g.id,
                None => &mut s.auto,
            };
            *slot.get_or_insert_with(|| next.fetch_add(1, Ordering::Relaxed))
        }))
    })
    .s()?;
    conn.create_scalar_function("jr_label", 0, f, |_| {
        Ok(SCOPE.with(|s| {
            s.borrow()
                .group
                .as_ref()
                .map(|g| g.label.clone())
                .unwrap_or_default()
        }))
    })
    .s()?;
    conn.commit_hook(Some(|| {
        clear_auto();
        COMMITS.fetch_add(1, Ordering::Relaxed);
        false
    }))
    .s()?;
    conn.rollback_hook(Some(clear_auto)).s()?;
    // A REPLACE conflict deletes the old row: its trigger must fire too.
    conn.pragma_update(None, "recursive_triggers", true).s()?;
    // Action 0 owns the undo / redo marker rows (undone = 2: never undone, never dropped as an undone tail).
    exec(
        conn,
        "INSERT OR IGNORE INTO journal_action(id, at, label, undone) VALUES (0, 0, '', 2)",
        [],
    )?;
    let mut sql = format!(
        "CREATE TEMP TRIGGER jr_action_new AFTER INSERT ON main.journal_action BEGIN
           DELETE FROM main.journal_action WHERE id > 0 AND (undone = 1 OR (id <= NEW.id - {KEEP} AND
             (id <= NEW.id - {HARD} OR id < COALESCE(
               (SELECT action FROM main.journal WHERE action > 0 AND seq > (SELECT MIN(last_seq) FROM main.connection WHERE mode = 1)
                ORDER BY seq LIMIT 1), NEW.id))));
         END;"
    );
    for &t in TABLES {
        let cols = columns(conn, t)?;
        if cols.is_empty() {
            continue;
        }
        let any = cols
            .iter()
            .map(|c| format!("OLD.\"{0}\" IS NOT NEW.\"{0}\"", c.name))
            .collect::<Vec<_>>()
            .join(" OR ");
        let record = |op: i64, old: &str, new: &str| {
            format!(
                "INSERT INTO main.journal_action(id, at, label) SELECT jr_action(), unixepoch(), jr_label()
                   WHERE NOT EXISTS (SELECT 1 FROM main.journal_action WHERE id = jr_action());
                 INSERT INTO main.journal(action, tbl, op, old, new) VALUES (jr_action(), '{t}', {op}, {old}, {new});"
            )
        };
        let (ins, upd, del) = (
            record(0, "NULL", &object("NEW", &cols)),
            record(1, &changed("OLD", &cols), &changed("NEW", &cols)),
            record(2, &object("OLD", &cols), "NULL"),
        );
        sql += &format!(
            "CREATE TEMP TRIGGER jr_{t}_i AFTER INSERT ON main.\"{t}\" WHEN jr_on() BEGIN {ins} END;
             CREATE TEMP TRIGGER jr_{t}_u AFTER UPDATE ON main.\"{t}\" WHEN jr_on() AND ({any}) BEGIN {upd} END;
             CREATE TEMP TRIGGER jr_{t}_d AFTER DELETE ON main.\"{t}\" WHEN jr_on() BEGIN {del} END;"
        );
    }
    conn.execute_batch(&sql).s()
}
