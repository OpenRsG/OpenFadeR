//! Moving what the store knows about a track from one key to another ([`Store::rekey`]), and the one-time move of the edits
//! made under a file's legacy key (`<file name>|<size>`) to the key of its Collection row (`t<id>`, [`super::key`]).
//!
//! A key appears in every table below. Per-track data (`COPY`: rating, cues, tags, lock, gain, phrase edits, device
//! baselines) can be given to several rows; positional data (playlist entries, plays, Tag List, cue banks, MATCHING pairs) is
//! a place in a list, so it goes to one row only.

use super::{
    date_secs, exec, exec_count, exists, file_name, file_stamp, id_key, in_tx, legacy_key,
    query_all, Store, R,
};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Per-track tables: `(table, columns after the key)`; every row keyed by a track's key. `cue` follows `track` (foreign key).
const COPY: &[(&str, &str)] = &[
    ("track", "rating, comment, color, cues_set, grid, grid_orig"),
    ("cue", "pos, hot, time_ms, loop_end_ms, color, comment"),
    ("track_mytag", "tag"),
    ("analysis_lock", ""),
    ("gain", "analysed_db, adjust_db"),
    ("phrase_edit", "mood, list"),
    ("device_sig", "device, sig"),
];

/// Tables whose rows keep their place when the key changes.
const POSITIONAL: [&str; 4] = ["entry", "history", "taglist", "hotcue_bank_cue"];

/// Every key any table holds (the legacy ones among them are the ones [`adopt`] looks for).
fn all_keys(db: &Connection) -> R<HashSet<String>> {
    super::column(
        db,
        "SELECT key FROM track UNION SELECT key FROM entry UNION SELECT key FROM track_mytag UNION SELECT key FROM history
         UNION SELECT key FROM analysis_lock UNION SELECT key FROM taglist UNION SELECT key FROM device_sig
         UNION SELECT key FROM hotcue_bank_cue UNION SELECT key FROM gain UNION SELECT key FROM phrase_edit
         UNION SELECT a FROM matching UNION SELECT b FROM matching",
        [],
    )
}

/// `from`'s per-track rows copied to `to` (rows `to` has already stay).
fn copy_edits(tx: &Connection, from: &str, to: &str) -> R<()> {
    for (table, rest) in COPY {
        let (cols, sel) = if rest.is_empty() {
            ("key".to_owned(), "?2".to_owned())
        } else {
            (format!("key, {rest}"), format!("?2, {rest}"))
        };
        exec(
            tx,
            &format!(
                "INSERT OR IGNORE INTO {table}({cols}) SELECT {sel} FROM {table} WHERE key = ?1"
            ),
            [from, to],
        )?;
    }
    Ok(())
}

/// Everything under `from` moves to `to`: edits, cues, playlist entries, tags, plays, lock, gain, phrases, cue banks, pairs. Where
/// `to` already has its own per-track rows they win; nothing of `from` is kept. The caller defers foreign-key checks.
fn move_edits(tx: &Connection, from: &str, to: &str) -> R<()> {
    let p = [from, to];
    if exists(tx, "SELECT 1 FROM track WHERE key = ?1", [to])? {
        exec(tx, "DELETE FROM track WHERE key = ?1", [from])?; // cues follow by cascade
    } else {
        exec(tx, "UPDATE track SET key = ?2 WHERE key = ?1", p)?;
        exec(tx, "UPDATE cue SET key = ?2 WHERE key = ?1", p)?;
    }
    for table in POSITIONAL {
        exec(
            tx,
            &format!("UPDATE {table} SET key = ?2 WHERE key = ?1"),
            p,
        )?;
    }
    // Keyed rows: the target's own row wins, so the source's moves only where the target has none.
    for table in [
        "track_mytag",
        "analysis_lock",
        "phrase_edit",
        "gain",
        "device_sig",
    ] {
        exec(
            tx,
            &format!("UPDATE OR IGNORE {table} SET key = ?2 WHERE key = ?1"),
            p,
        )?;
        exec(tx, &format!("DELETE FROM {table} WHERE key = ?1"), [from])?;
    }
    // `a` <= `b`: a pair is stored once, whichever order it was made in.
    let pairs: Vec<(String, String)> = query_all(
        tx,
        "SELECT a, b FROM matching WHERE a = ?1 OR b = ?1",
        [from],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    for (a, b) in pairs {
        exec(tx, "DELETE FROM matching WHERE a = ?1 AND b = ?2", [&a, &b])?;
        let other = if a == from { b } else { a };
        if other != to {
            let (x, y) = if to <= other.as_str() {
                (to, &*other)
            } else {
                (&*other, to)
            };
            exec(
                tx,
                "INSERT OR IGNORE INTO matching(a, b) VALUES (?1, ?2)",
                [x, y],
            )?;
        }
    }
    Ok(())
}

/// `from`'s data to every key of `to`: the first takes everything, the others a copy of the per-track data.
fn spread(tx: &Connection, from: &str, to: &[String]) -> R<()> {
    for k in to.iter().skip(1) {
        copy_edits(tx, from, k)?;
    }
    match to.first() {
        Some(first) => move_edits(tx, from, first),
        None => Ok(()),
    }
}

/// Gives the edits stored under the legacy key of each new Collection row (`(id, legacy key)`) to its `t<id>`; rows that
/// share a legacy key each get a copy of the per-track data. The caller defers foreign-key checks.
pub(super) fn adopt(tx: &Connection, fresh: &[(i64, String)]) -> R<()> {
    if fresh.is_empty() {
        return Ok(());
    }
    let have = all_keys(tx)?;
    let mut groups: HashMap<&str, Vec<String>> = HashMap::new();
    for (id, legacy) in fresh.iter().filter(|(_, k)| have.contains(k)) {
        groups.entry(legacy).or_default().push(id_key(*id));
    }
    groups
        .into_iter()
        .try_for_each(|(from, to)| spread(tx, from, &to))
}

/// Rows imported before the canonical columns existed (`added_at` 0) get their size, mtime, key id and date from the file and
/// the tags, and their legacy edits move to `t<id>`. A row whose file cannot be read (a drive that is not mounted) keeps size
/// 0 and its legacy edits where they are: every start (and [`Store::collection_put`] when the file shows up) looks again, so
/// it adopts them once the size is known. Runs before anything else holds the connection.
// ponytail: stats every old row's file once at the first start after the upgrade (about a second per 100k tracks on a local
// disk); rows whose file stays missing are stat'ed again at every start.
pub(super) fn backfill(conn: &mut Connection) -> R<()> {
    if !exists(
        conn,
        "SELECT 1 FROM collection WHERE added_at = 0 OR size = 0",
        [],
    )? {
        return Ok(());
    }
    in_tx(conn, true, |tx| {
        let rows: Vec<(i64, String, String, String, i64)> = query_all(
            tx,
            "SELECT id, path, tkey, date_added, added_at FROM collection WHERE added_at = 0 OR size = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        let now = chrono::Utc::now().timestamp();
        let mut fresh = Vec::with_capacity(rows.len());
        for (id, path, tkey, date, added) in rows {
            let file = Path::new(&path);
            let (size, mtime) = file_stamp(file);
            if added == 0 {
                exec(
                    tx,
                    "UPDATE collection SET size = ?2, mtime = ?3, key_id = ?4, added_at = ?5 WHERE id = ?1",
                    params![
                        id,
                        size,
                        mtime,
                        crate::analysis::key::key_id(&tkey),
                        date_secs(&date).unwrap_or(now).max(1)
                    ],
                )?;
            } else if size > 0 {
                exec(
                    tx,
                    "UPDATE collection SET size = ?2, mtime = ?3 WHERE id = ?1",
                    params![id, size, mtime],
                )?;
            }
            if size > 0 {
                fresh.push((id, legacy_key(&file_name(file), size as u64)));
            }
        }
        adopt(tx, &fresh)
    })
}

/// Deletes everything keyed `key` (the rows [`Store::rekey`] would move), playlist entries aside: they need their gaps
/// closed, see [`Store::forget_entries`]. Cue-bank slots after a deleted one move up a letter.
pub(super) fn forget_rows(tx: &Connection, key: &str) -> R<()> {
    exec(tx, "DELETE FROM track WHERE key = ?1", [key])?; // cues follow by cascade
    for table in [
        "track_mytag",
        "analysis_lock",
        "phrase_edit",
        "gain",
        "device_sig",
        "history",
        "taglist",
    ] {
        exec(tx, &format!("DELETE FROM {table} WHERE key = ?1"), [key])?;
    }
    exec(tx, "DELETE FROM matching WHERE a = ?1 OR b = ?1", [key])?;
    let slots: Vec<(i64, i64)> = query_all(
        tx,
        "SELECT bank, pos FROM hotcue_bank_cue WHERE key = ?1 ORDER BY pos DESC",
        [key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    for (bank, pos) in slots {
        exec(
            tx,
            "DELETE FROM hotcue_bank_cue WHERE bank = ?1 AND pos = ?2",
            [bank, pos],
        )?;
        exec_count(
            tx,
            "UPDATE hotcue_bank_cue SET pos = pos - 1 WHERE bank = ?1 AND pos > ?2",
            [bank, pos],
        )?;
    }
    Ok(())
}

impl Store {
    /// Everything the store knows under key `from` (edits, cues, playlist entries, tags, plays, lock, gain, phrase edits,
    /// cue banks, MATCHING pairs) moves to `to`. Where `to` already has its own edits they win; nothing of `from` is kept twice.
    pub fn rekey(&self, from: &str, to: &str) -> R<()> {
        if from == to {
            return Ok(());
        }
        in_tx(&mut self.db(), true, |tx| move_edits(tx, from, to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Cue;
    use crate::store::NodeKind;

    fn cue(ms: u32) -> Cue {
        Cue {
            hot_cue: 1,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    #[test]
    fn rekey_moves_edits_entries_and_prefers_the_target() {
        let s = Store::in_memory().unwrap();
        s.set_rating("a|1", Some(3)).unwrap();
        s.set_cues("a|1", Some(&[cue(500)])).unwrap();
        let pl = s.add_node(None, "P", NodeKind::Playlist).unwrap();
        s.add_entries(pl, &["a|1".into(), "z|9".into()]).unwrap();
        s.set_analysis_lock("a|1", true).unwrap();
        s.set_gain_adjust("a|1", 2.0).unwrap();
        s.add_match("a|1", "m|5").unwrap();
        s.rekey("a|1", "b|2").unwrap();
        let e = s.edits("b|2").unwrap();
        assert_eq!((e.rating, e.cues), (Some(3), Some(vec![cue(500)])));
        assert_eq!(s.entries(pl).unwrap(), ["b|2", "z|9"]);
        assert!(s.analysis_locked("b|2").unwrap() && !s.analysis_locked("a|1").unwrap());
        assert_eq!(s.gain("b|2").unwrap().unwrap().adjust_db, 2.0);
        assert_eq!(s.matches_of("b|2").unwrap(), ["m|5"]);
        assert_eq!(s.matches_of("m|5").unwrap(), ["b|2"]); // `a` <= `b` holds after the move
        assert_eq!(s.edits("a|1").unwrap().rating, None);
        // Target already edited: its values stay, the source's are gone.
        s.set_rating("c|3", Some(5)).unwrap();
        s.rekey("b|2", "c|3").unwrap();
        assert_eq!(s.edits("c|3").unwrap().rating, Some(5));
        assert_eq!(s.edits("b|2").unwrap(), Default::default());
    }

    fn file(dir: &Path, name: &str, body: &[u8]) -> crate::library::Track {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
        crate::library::Track {
            file: Some(dir.join(name)),
            title: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn adding_a_row_moves_the_legacy_edits_and_copies_them_to_same_named_rows() {
        let dir = std::env::temp_dir().join(format!("openfader-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b, solo) = (
            file(&dir.join("a"), "x.mp3", b"12345"),
            file(&dir.join("b"), "x.mp3", b"abcde"),
            file(&dir.join("a"), "solo.mp3", b"1"),
        );
        let s = Store::in_memory().unwrap();
        for k in ["x.mp3|5", "solo.mp3|1"] {
            s.set_rating(k, Some(4)).unwrap();
            s.set_cues(k, Some(&[cue(900)])).unwrap();
        }
        let pl = s.add_node(None, "P", NodeKind::Playlist).unwrap();
        s.add_entries(pl, &["x.mp3|5".into(), "solo.mp3|1".into()])
            .unwrap();

        // Two rows with the legacy key of the edits: both keep the rating and cues, the playlist names the first.
        assert_eq!(
            s.collection_put(&[a.clone(), b.clone(), solo.clone()], false)
                .unwrap(),
            3
        );
        let id =
            |t: &crate::library::Track| s.id_of_path(t.file.as_deref().unwrap()).unwrap().unwrap();
        let (ka, kb, ks) = (id_key(id(&a)), id_key(id(&b)), id_key(id(&solo)));
        for k in [&ka, &kb, &ks] {
            let e = s.edits(k).unwrap();
            assert_eq!((e.rating, e.cues), (Some(4), Some(vec![cue(900)])), "{k}");
        }
        assert_eq!(s.entries(pl).unwrap(), [ka.clone(), ks.clone()]);
        assert_eq!(s.edits("x.mp3|5").unwrap(), Default::default());
        assert_eq!(s.edits("solo.mp3|1").unwrap(), Default::default());

        // Edits of a track are its own from now on: adding the same file name and size again finds no legacy edits.
        s.set_rating(&kb, Some(1)).unwrap();
        assert_eq!(s.edits(&ka).unwrap().rating, Some(4));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deleting_a_track_leaves_nothing_under_its_key() {
        let dir = std::env::temp_dir().join(format!("openfader-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (file(&dir, "a.mp3", b"a"), file(&dir, "b.mp3", b"bb"));
        let s = Store::in_memory().unwrap();
        s.collection_put(&[a.clone(), b.clone()], false).unwrap();
        let (ia, ib) = (
            s.id_of_path(a.file.as_deref().unwrap()).unwrap().unwrap(),
            s.id_of_path(b.file.as_deref().unwrap()).unwrap().unwrap(),
        );
        let (ka, kb) = (id_key(ia), id_key(ib));
        let pl = s.add_node(None, "P", NodeKind::Playlist).unwrap();
        s.add_entries(pl, &[ka.clone(), kb.clone()]).unwrap();
        s.set_rating(&ka, Some(2)).unwrap();
        s.add_match(&ka, &kb).unwrap();
        s.collection_delete(ia).unwrap();
        assert_eq!(s.entries(pl).unwrap(), [kb.clone()]);
        assert_eq!(s.edits(&ka).unwrap(), Default::default());
        assert!(s.matches_of(&kb).unwrap().is_empty());
        assert_eq!(s.id_of_path(a.file.as_deref().unwrap()).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
