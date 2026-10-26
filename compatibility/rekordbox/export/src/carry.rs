//! What a rewritten `exportLibrary.db` keeps from the one it replaces. [`super::onelibrary::write`] builds the whole database from
//! the merged [`super::pdb::db::Db`], so everything it does not model would be lost: the play histories the players write, the
//! `cue` rows, the Hot Cue Bank Lists and `recommendedLike`. They are copied over table by table (same schema, same ids; rows
//! of tracks that are gone are dropped). Track ids survive an update, so the references stay valid.

use super::pdb::db::Db;
use super::{open_keyed, open_keyed_ro};
use compat_rekordbox_common::at;
use rusqlite::params;
use std::path::Path;

/// `property.myTagMasterDBID` of an existing library: the one number `content.masterDbId` shares too (`masterDbId` is not a `property` column); `None` when unreadable.
pub(super) fn library_id(old: &Path, key: &str) -> Option<u32> {
    open_keyed_ro(old, key)
        .ok()?
        .query_row("select myTagMasterDBID from property", [], |r| {
            r.get::<_, i64>(0)
        })
        .ok()
        .map(|v| v as u32)
}

/// Copies the carried tables of `old` into the freshly written `new` (both encrypted with `key`).
pub(super) fn carry(old: &Path, new: &Path, key: &str) -> Result<(), String> {
    let conn = open_keyed(new, key)?;
    at(
        conn.execute(
            "attach database ?1 as old key ?2",
            [old.to_string_lossy().as_ref(), key],
        ),
        old,
    )?;
    let alive = "select content_id from main.content";
    at(
        conn.execute_batch(&format!(
            "begin;
         insert into history select * from old.history;
         insert into history_content select * from old.history_content where content_id in ({alive});
         insert into cue select * from old.cue where content_id in ({alive});
         insert into hotCueBankList select * from old.hotCueBankList;
         insert into hotCueBankList_cue select * from old.hotCueBankList_cue where cue_id in (select cue_id from main.cue);
         insert into recommendedLike select * from old.recommendedLike where content_id_1 in ({alive}) and content_id_2 in ({alive});
         commit;"
        )),
        old,
    )?;
    at(conn.execute_batch("detach database old"), old)
}

/// Writes the Device Library's play histories ([`Db::histories`]) into the `history` tables of a OneLibrary made without an older one
/// to carry (`history(id, sequenceNo, name, attribute 0, parent 0)`, `history_content(id, content_id, sequenceNo from 1)`; row
/// semantics inferred from the `playlist` tables, the sample export had no history).
pub(super) fn histories(new: &Path, key: &str, db: &Db) -> Result<(), String> {
    let mut conn = open_keyed(new, key)?;
    let tx = at(conn.transaction(), new)?;
    {
        let mut history = at(
            tx.prepare("insert into history values (?, ?, ?, 0, 0)"),
            new,
        )?;
        let mut content = at(
            tx.prepare("insert into history_content values (?, ?, ?)"),
            new,
        )?;
        for (seq, h) in db.histories.iter().enumerate() {
            at(history.execute(params![h.id, seq as u32, h.name]), new)?;
            for (i, t) in h.tracks.iter().enumerate() {
                at(content.execute(params![h.id, t, i as u32 + 1]), new)?;
            }
        }
    }
    at(tx.commit(), new)
}
