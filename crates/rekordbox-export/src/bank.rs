//! Hot Cue Bank Lists on a device (rekordbox: Media Browser > Hot Cue Bank Lists, dragged onto a device; manual p3478-3582): a
//! list holds up to 8 banks A-H, each one cue or loop of some track. They exist in the OneLibrary only: `hotCueBankList(id,
//! sequenceNo, name, image_id, attribute, parent)`, `hotCueBankList_cue(list, cue_id, sequenceNo)` and `cue` rows. `export.pdb`
//! has no table for them (the sample export's tables were all empty or constant; inferred).
//!
//! Column meaning (spec: pyrekordbox docs of `exportLibrary.db`, no real export with cue rows was available to check): `kind` 0 =
//! memory cue, otherwise the hot cue number (we write the slot 1..=8); `colorTableIndex` the cue colour's index in the 65-entry
//! palette ([`rekordbox_common::anlz::cues::picker`], nearest match); `inUsec` / `outUsec` microseconds, `in150FramePerSec` /
//! `out150FramePerSec` the same in 1/150 s; `isActiveLoop` 1 for a loop. The beat-loop length and the MPEG / block positions are
//! written as 0 (inferred: players take cue positions from the time columns; unobserved).
//!
//! A list with the name of one the device has replaces it; the other lists stay (see `carry`).

use super::open_keyed;
use openfader_plugin::library::{Cue, MAX_BANKS};
use rekordbox_common::at;
use rusqlite::{Transaction, params};
use std::path::Path;

/// A list resolved to device track ids.
pub type Resolved = (String, Vec<(u32, Cue)>);

/// Index of the palette entry closest to `rgb` (0 when the cue has no colour).
fn palette_index(rgb: Option<[u8; 3]>) -> usize {
    let Some(c) = rgb else { return 0 };
    let d = |p: &[u8; 3]| {
        p.iter()
            .zip(c)
            .map(|(a, b)| (i32::from(*a) - i32::from(b)).pow(2))
            .sum::<i32>()
    };
    // `picker` is the palette without its two black entries (0 and 63): map its index back.
    let best = rekordbox_common::anlz::cues::picker()
        .enumerate()
        .min_by_key(|(_, p)| d(p))
        .map_or(0, |(i, _)| i);
    if best < 62 { best + 1 } else { best + 2 }
}

/// Writes `lists` into the OneLibrary at `path` (encrypted with `key`): replaces the lists of the same names, appends the rest.
pub fn apply(path: &Path, key: &str, lists: &[Resolved]) -> Result<(), String> {
    if lists.is_empty() {
        return Ok(());
    }
    let mut conn = open_keyed(path, key)?;
    let tx = at(conn.transaction(), path)?;
    let max = |sql: &str| at(tx.query_row(sql, [], |r| r.get::<_, i64>(0)), path);
    for (name, cues) in lists {
        at(tx.execute("delete from cue where cue_id in (select cue_id from hotCueBankList_cue where hotCueBankList_id in (select hotCueBankList_id from hotCueBankList where name = ?1))", [name]), path)?;
        at(tx.execute("delete from hotCueBankList_cue where hotCueBankList_id in (select hotCueBankList_id from hotCueBankList where name = ?1)", [name]), path)?;
        at(
            tx.execute("delete from hotCueBankList where name = ?1", [name]),
            path,
        )?;
        let list = max("select coalesce(max(hotCueBankList_id), 0) from hotCueBankList")? + 1;
        let seq = max(
            "select coalesce(max(sequenceNo), -1) from hotCueBankList where hotCueBankList_id_parent = 0",
        )? + 1;
        at(
            tx.execute(
                "insert into hotCueBankList values (?, ?, ?, NULL, 0, 0)",
                params![list, seq, name],
            ),
            path,
        )?;
        let mut cue_id = max("select coalesce(max(cue_id), 0) from cue")?;
        for (slot, (content, c)) in cues.iter().take(MAX_BANKS).enumerate() {
            cue_id += 1;
            at(insert_cue(&tx, list, cue_id, slot, *content, c), path)?;
        }
    }
    at(tx.commit(), path)
}

/// The `cue` row of bank `slot` (A = 0) and its `hotCueBankList_cue` link.
fn insert_cue(
    tx: &Transaction,
    list: i64,
    id: i64,
    slot: usize,
    content: u32,
    c: &Cue,
) -> rusqlite::Result<()> {
    let (inn, out) = (
        i64::from(c.time_ms) * 1000,
        c.loop_end_ms.map_or(0, |o| i64::from(o) * 1000),
    );
    tx.prepare_cached(
        "insert into cue values (?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?, ?, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)",
    )?
    .execute(params![
        id,
        content,
        slot as i64 + 1,
        palette_index(c.color) as i64,
        c.comment,
        i64::from(c.loop_end_ms.is_some()),
        inn,
        out,
        inn * 150 / 1_000_000,
        out * 150 / 1_000_000
    ])?;
    tx.prepare_cached("insert into hotCueBankList_cue values (?, ?, ?)")?
        .execute(params![list, id, slot as i64])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_written_replaced_by_name_and_keep_other_lists() {
        let dir = std::env::temp_dir().join(format!("rk-bank-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.db");
        let conn = open_keyed(&path, "k").unwrap();
        conn.execute_batch(crate::onelibrary::SCHEMA).unwrap();
        drop(conn);
        let cue = |ms, hot| Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: None,
            color: Some([0, 0, 255]),
            comment: Some("drop".into()),
        };
        apply(
            &path,
            "k",
            &[
                (
                    "Opening".into(),
                    vec![
                        (1, cue(1500, 1)),
                        (
                            2,
                            Cue {
                                loop_end_ms: Some(5500),
                                ..cue(4000, 2)
                            },
                        ),
                    ],
                ),
                ("Other".into(), vec![(1, cue(100, 1))]),
            ],
        )
        .unwrap();
        apply(&path, "k", &[("Opening".into(), vec![(3, cue(250, 1))])]).unwrap();

        let c = open_keyed(&path, "k").unwrap();
        let q = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(
            (
                q("select count(*) from hotCueBankList"),
                q("select count(*) from cue"),
                q("select count(*) from hotCueBankList_cue")
            ),
            (2, 2, 2)
        );
        let (content, usec, f150, kind, color): (i64, i64, i64, i64, i64) =
            c.query_row("select content_id, inUsec, in150FramePerSec, kind, colorTableIndex from cue where content_id = 3", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!((content, usec, f150, kind, color), (3, 250_000, 37, 1, 1));
        assert_eq!(
            q("select count(*) from hotCueBankList where name = 'Opening'"),
            1
        );
        let _ = std::fs::remove_dir_all(dir); // cleanup: Windows may still hold a handle
    }
}
