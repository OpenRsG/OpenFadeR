//! Our Collection: the library. One `collection` row per track (`id` is the track's identity, [`super::id_key`]), in import
//! order, with the canonical columns the app sorts and finds by (`size`, `mtime`, `key_id`, `added_at`, `fingerprint`).
//!
//! * `removed` = source paths (a [`key`] for a track without a file) the user removed from the Collection: a sync must not
//!   bring them back (the file is never touched).
//! * `relocated` = original file path → where the user relocated it (Missing File Manager); a sync maps the source's
//!   original path to the relocated row.
//! * `analysis_lock` = keys whose analysis must not be redone; `taglist` = the persisted Tag List.
//!
//! [`Store::collection_tracks`] is cached until a `collection` row changes ([`Store::collection_generation`]); a track list
//! is a clone of one `Arc`. Re-keying of the edits made before a row existed is in `rekey`.
//! Paths are stored as (lossy) UTF-8 text.

use super::{
    Sql, Store, R, column, date_secs, exec, exec_count, file_name, file_stamp, id_key, in_tx, key,
    legacy_key, query_all, query_opt, rekey, scalar,
};
use crate::{analysis::key::key_id, library::Track};
use openfader_plugin::library::{owned_ref, split_ref};
use rusqlite::{params, params_from_iter, types::ToSql, Connection, Row};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{atomic::Ordering, Arc};

const COLS: &str = "path, title, artist, album, tkey, genre, bpm_x100, length_s, analysis_ref, artwork, comment, date_added, bitrate, sample_rate, label";

fn from_row(r: &Row) -> rusqlite::Result<Track> {
    let opt = |i| r.get::<_, Option<String>>(i).map(|s| s.map(PathBuf::from));
    Ok(Track {
        id: Some(r.get(0)?),
        file: Some(PathBuf::from(r.get::<_, String>(1)?)),
        title: r.get(2)?,
        artist: r.get(3)?,
        album: r.get(4)?,
        key: r.get(5)?,
        genre: r.get(6)?,
        label: r.get(15)?,
        bpm_x100: r.get(7)?,
        length_s: r.get(8)?,
        analysis_ref: r.get(9)?,
        artwork: opt(10)?,
        rating: 0,
        comment: r.get(11)?,
        date_added: r.get(12)?,
        bitrate: r.get(13)?,
        sample_rate: r.get(14)?,
        remote: None,
    })
}

/// File name and size → the one row that has them; `None` for a name and size several rows share.
type Names = HashMap<String, Option<i64>>;

/// What [`Store::collection_tracks`] and [`Store::id_of_file`] keep, each as of the [`Store::collection_generation`] it was
/// read at.
#[derive(Default)]
pub(super) struct Cache {
    tracks: Option<(u64, Arc<Vec<Track>>)>,
    names: Option<(u64, Arc<Names>)>,
}

impl Store {
    /// Changes with every row of `collection` that is inserted, updated or deleted, by any statement on this store
    /// (the update hook counts them): what a cache of the Collection compares. Less often than [`Store::generation`].
    pub fn collection_generation(&self) -> u64 {
        self.collection_changes.load(Ordering::Relaxed)
    }

    /// The Collection, in import order, every track with `id: Some(..)`. Read once per [`Store::collection_generation`].
    pub fn collection_tracks(&self) -> R<Arc<Vec<Track>>> {
        let mut cache = self.cache.lock();
        let g = self.collection_generation();
        if let Some((have, t)) = &cache.tracks
            && *have == g
        {
            return Ok(t.clone());
        }
        let rows: Vec<Track> = query_all(
            &self.db(),
            &format!("SELECT id, {COLS} FROM collection ORDER BY id"),
            [],
            from_row,
        )?;
        let t = Arc::new(rows);
        cache.tracks = Some((g, t.clone()));
        Ok(t)
    }

    /// Track `id`; `None` when there is no such row.
    pub fn collection_track(&self, id: i64) -> R<Option<Track>> {
        query_opt(
            &self.db(),
            &format!("SELECT id, {COLS} FROM collection WHERE id = ?1"),
            [id],
            from_row,
        )
    }

    /// The row listing the file at `path`.
    pub fn id_of_path(&self, path: &Path) -> R<Option<i64>> {
        scalar(
            &self.db(),
            "SELECT id FROM collection WHERE path = ?1",
            [path.to_string_lossy()],
        )
    }

    /// The only row whose file has this name and size (what [`key`] gives a track without an id); `None` for none or several.
    pub(super) fn id_of_file(&self, name: &str, size: u64) -> Option<i64> {
        let mut cache = self.cache.lock();
        let g = self.collection_generation();
        if cache.names.as_ref().is_none_or(|(have, _)| *have != g) {
            let rows: Vec<(i64, String, i64)> = query_all(&self.db(), "SELECT id, path, size FROM collection", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .ok()?;
            let mut names = Names::with_capacity(rows.len());
            for (id, path, size) in rows {
                let k = legacy_key(&file_name(Path::new(&path)), size as u64);
                names.entry(k).and_modify(|v| *v = None).or_insert(Some(id));
            }
            cache.names = Some((g, Arc::new(names)));
        }
        let names = cache.names.as_ref()?.1.clone();
        drop(cache);
        names.get(&legacy_key(name, size)).copied().flatten()
    }

    /// Adds (or, with `replace`, overwrites by file path) `tracks` that have a file; they stop being "removed". Fills `size`,
    /// `mtime` (from the file), `key_id` (from the track's key in any notation) and `added_at` (from `date_added`, else now), and
    /// gives the edits stored under the legacy key of a new row's file (`<name>|<size>`) to the row. Several new rows with one
    /// legacy key each get a copy of the per-track edits (rating, cues, tags, ...). A row already there only has its size and
    /// mtime refreshed (and everything else with `replace`). Returns how many rows were added or changed.
    pub fn collection_put(&self, tracks: &[Track], replace: bool) -> R<usize> {
        let items: Vec<(Option<i64>, &Track)> = tracks
            .iter()
            .filter(|t| t.file.is_some())
            .map(|t| (None, t))
            .collect();
        self.put_rows(&items, replace).map(|(n, _)| n)
    }

    /// [`Store::collection_put`] with `replace`, for one track, returning the row's id: `None` inserts (or overwrites the row
    /// of the same file), `Some(id)` overwrites that row in place, also moving it to the track's file.
    pub fn collection_upsert(&self, t: &Track, id: Option<i64>) -> R<i64> {
        Ok(self.collection_upsert_many(&[(id, t)])?[0])
    }

    /// [`Store::collection_upsert`] for many in one transaction (one legacy-key scan, not one per track); the ids in order.
    pub fn collection_upsert_many(&self, items: &[(Option<i64>, &Track)]) -> R<Vec<i64>> {
        self.put_rows(items, true).map(|(_, ids)| ids)
    }

    fn put_rows(&self, items: &[(Option<i64>, &Track)], replace: bool) -> R<(usize, Vec<i64>)> {
        let now = chrono::Utc::now().timestamp();
        let insert = format!(
            "INSERT INTO collection(id, {COLS}, size, mtime, key_id, added_at) VALUES ((SELECT MAX(COALESCE((SELECT MAX(id) FROM collection), 0), (SELECT n FROM collection_hwm)) + 1), ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,COALESCE(?19, ?20))"
        );
        let update = "UPDATE collection SET path=?1, title=?2, artist=?3, album=?4, tkey=?5, genre=?6, bpm_x100=?7, length_s=?8, analysis_ref=?9, artwork=?10, comment=?11, date_added=?12, bitrate=?13, sample_rate=?14, label=?15, size=?16, mtime=?17, key_id=?18, added_at=COALESCE(?19, added_at) WHERE id=?20";
        in_tx(&mut self.db(), true, |tx| {
            let mut ids = Vec::with_capacity(items.len());
            let (mut fresh, mut n) = (Vec::new(), 0);
            for &(id, t) in items {
                let file = t.file.as_deref().ok_or("this track has no file")?;
                let path = file.to_string_lossy();
                let (size, mtime) = file_stamp(file);
                let artwork = t.artwork.as_deref().map(Path::to_string_lossy);
                let (key_id, added) = (key_id(&t.key), date_secs(&t.date_added));
                let row: [&dyn ToSql; 19] = [
                    &path, &t.title, &t.artist, &t.album, &t.key, &t.genre, &t.bpm_x100, &t.length_s,
                    &t.analysis_ref, &artwork, &t.comment, &t.date_added, &t.bitrate, &t.sample_rate, &t.label,
                    &size, &mtime, &key_id, &added,
                ];
                let with = |last| bind(&row, last);
                let found = match id {
                    Some(id) => Some(id),
                    None => scalar(tx, "SELECT id FROM collection WHERE path = ?1", [&path])?,
                };
                let explicit = id.is_some();
                // A row whose file was out of reach (size 0) when it was imported has edits waiting under the legacy key.
                let unseen = |tx: &Connection, id: i64| -> R<bool> {
                    Ok(size > 0 && scalar::<i64>(tx, "SELECT size FROM collection WHERE id = ?1", [id])? == Some(0))
                };
                let id = match found {
                    Some(id) if explicit || replace => {
                        let late = unseen(tx, id)?;
                        if exec_count(tx, update, with(&id))? == 0 {
                            return Err(format!("no collection row {id}"));
                        }
                        if late {
                            fresh.push((id, legacy_key(&file_name(file), size as u64)));
                        }
                        n += 1;
                        id
                    }
                    Some(id) => {
                        // Already there: only the file's own facts move.
                        if unseen(tx, id)? {
                            fresh.push((id, legacy_key(&file_name(file), size as u64)));
                        }
                        exec(
                            tx,
                            "UPDATE collection SET size = ?2, mtime = ?3 WHERE id = ?1 AND (size != ?2 OR mtime != ?3)",
                            params![id, size, mtime],
                        )?;
                        id
                    }
                    None => {
                        exec(tx, &insert, with(&now))?;
                        let id = tx.last_insert_rowid();
                        fresh.push((id, legacy_key(&file_name(file), size as u64)));
                        n += 1;
                        id
                    }
                };
                exec(tx, "DELETE FROM removed WHERE id = ?1", [&path])?;
                ids.push(id);
            }
            rekey::adopt(tx, &fresh)?;
            Ok((n, ids))
        })
    }

    /// Gives every imported track whose `analysis_ref` is bare (no plugin name before it, [`split_ref`]) to library plugin
    /// `plugin`: refs written before they carried one. Idempotent; returns how many rows changed.
    pub fn prefix_analysis_refs(&self, plugin: &str) -> R<usize> {
        self.write(|tx| {
            let rows: Vec<(String, String)> = query_all(
                tx,
                "SELECT path, analysis_ref FROM collection WHERE analysis_ref IS NOT NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let mut n = 0;
            for (path, r) in rows.iter().filter(|(_, r)| split_ref(r).is_none()) {
                n += exec_count(
                    tx,
                    "UPDATE collection SET analysis_ref = ?2 WHERE path = ?1",
                    params![path, owned_ref(plugin, r)],
                )?;
            }
            Ok(n)
        })
    }

    /// Deletes row `id` with everything the store keeps under its key: edits, cues, tags, plays, Tag List, cue-bank slots,
    /// MATCHING pairs and its playlist entries (the lists close their gaps). `source_map` rows go with it (foreign key). The
    /// audio file stays where it is.
    pub fn collection_delete(&self, id: i64) -> R<()> {
        let key = id_key(id);
        self.forget_entries(&key)?;
        in_tx(&mut self.db(), true, |tx| {
            rekey::forget_rows(tx, &key)?;
            exec(tx, "DELETE FROM collection WHERE id = ?1", [id])
        })
    }

    /// Removes `t` from the Collection: its row goes ([`Store::collection_delete`]) and the path the source listed it under is
    /// remembered so a sync does not bring it back (a track without a file: its [`key`]).
    pub fn collection_remove(&self, t: &Track) -> R<()> {
        let id = match (t.id, &t.file) {
            (Some(id), _) => Some(id),
            (None, Some(f)) => self.id_of_path(f)?,
            (None, None) => None,
        };
        if let Some(id) = id {
            self.collection_delete(id)?;
        }
        let shown = match &t.file {
            Some(f) => {
                let shown = f.to_string_lossy();
                scalar::<String>(&self.db(), "SELECT orig FROM relocated WHERE path = ?1", [&shown])?
                    .unwrap_or_else(|| shown.into_owned())
            }
            None => key(t),
        };
        self.run("INSERT OR IGNORE INTO removed(id) VALUES (?1)", [shown])
    }

    /// Deletes every playlist entry naming `key` and closes the gaps.
    pub fn forget_entries(&self, key: &str) -> R<()> {
        let nodes: Vec<i64> = column(&self.db(), "SELECT DISTINCT node FROM entry WHERE key = ?1", [key])?;
        for n in nodes {
            let kept: Vec<String> = self.entries(n)?.into_iter().filter(|k| k != key).collect();
            self.set_entries(n, &kept)?;
        }
        Ok(())
    }

    /// Source tracks the user removed: the path the source lists them under (their key when they have no file).
    pub fn removed_ids(&self) -> R<HashSet<String>> {
        column(&self.db(), "SELECT id FROM removed", [])
    }

    /// Original path → relocated path.
    pub fn relocations(&self) -> R<HashMap<PathBuf, PathBuf>> {
        query_all(&self.db(), "SELECT orig, path FROM relocated", [], |r| {
            Ok((PathBuf::from(r.get::<_, String>(0)?), PathBuf::from(r.get::<_, String>(1)?)))
        })
    }

    /// The file `track` was listed at now lives at `new`: its row (the id, so its edits and playlist places) moves to `new`
    /// and the original path is remembered ([`Store::relocations`]). A track without a row follows by legacy key instead.
    pub fn relocate(&self, track: &Track, new: &Path) -> R<()> {
        let Some(old) = &track.file else {
            return Err("this track has no file".into());
        };
        let row = match track.id {
            Some(id) => Some(id),
            None => self.id_of_path(old)?,
        };
        // Another row already lists the new place: the moved one replaces it.
        if let Some(other) = self.id_of_path(new)?
            && Some(other) != row
        {
            self.collection_delete(other)?;
        }
        self.write(|tx| {
            let (o, n) = (old.to_string_lossy(), new.to_string_lossy());
            let (size, mtime) = file_stamp(new);
            match row {
                Some(id) => exec(
                    tx,
                    "UPDATE collection SET path = ?2, size = ?3, mtime = ?4 WHERE id = ?1",
                    params![id, n, size, mtime],
                )?,
                None => exec(tx, "UPDATE collection SET path = ?2, size = ?3, mtime = ?4 WHERE path = ?1", params![o, n, size, mtime])?,
            }
            let p = [&o, &n];
            if exec_count(tx, "UPDATE relocated SET path = ?2 WHERE path = ?1", p)? == 0 {
                exec(
                    tx,
                    "INSERT INTO relocated(orig, path) VALUES (?1, ?2) ON CONFLICT(orig) DO UPDATE SET path = ?2",
                    p,
                )?;
            }
            Ok(())
        })?;
        if track.id.is_none() && row.is_none() {
            // A track the Collection does not list is keyed by its file alone.
            let at_new = Track {
                file: Some(new.to_path_buf()),
                ..Track::default()
            };
            self.rekey(&key(track), &key(&at_new))?;
        }
        Ok(())
    }

    /// The audio fingerprint of row `id` ([`crate::analysis::fingerprint`]); `None` until one was set.
    pub fn fingerprint(&self, id: i64) -> R<Option<String>> {
        Ok(scalar::<Option<String>>(&self.db(), "SELECT fingerprint FROM collection WHERE id = ?1", [id])?
            .flatten()
            .filter(|f| !f.is_empty()))
    }

    /// Stores the audio fingerprint of row `id`. The cached track list and file index stay current: they do not hold it.
    pub fn set_fingerprint(&self, id: i64, fingerprint: &str) -> R<()> {
        let mut cache = self.cache.lock();
        let db = self.db();
        let before = self.collection_generation();
        self.system(|| {
            exec(&db, "UPDATE collection SET fingerprint = ?2 WHERE id = ?1", params![id, fingerprint])
        })?;
        let after = self.collection_generation();
        if let Some((g, _)) = cache.tracks.as_mut().filter(|(g, _)| *g == before) {
            *g = after;
        }
        if let Some((g, _)) = cache.names.as_mut().filter(|(g, _)| *g == before) {
            *g = after;
        }
        Ok(())
    }

    /// The rows whose audio has this fingerprint, oldest first: where a moved or renamed file went.
    pub fn find_by_fingerprint(&self, fingerprint: &str) -> R<Vec<i64>> {
        column(
            &self.db(),
            "SELECT id FROM collection WHERE fingerprint = ?1 ORDER BY id",
            [fingerprint],
        )
    }

    pub fn set_analysis_lock(&self, key: &str, locked: bool) -> R<()> {
        self.run(
            if locked {
                "INSERT OR IGNORE INTO analysis_lock(key) VALUES (?1)"
            } else {
                "DELETE FROM analysis_lock WHERE key = ?1"
            },
            [key],
        )
    }

    pub fn analysis_locked(&self, key: &str) -> R<bool> {
        super::exists(&self.db(), "SELECT 1 FROM analysis_lock WHERE key = ?1", [key])
    }

    /// The persisted Tag List: track keys in queue order.
    pub fn taglist(&self) -> R<Vec<String>> {
        column(&self.db(), "SELECT key FROM taglist ORDER BY pos", [])
    }

    pub fn set_taglist(&self, keys: &[String]) -> R<()> {
        self.write(|tx| {
            exec(tx, "DELETE FROM taglist", [])?;
            let mut ins = tx.prepare_cached("INSERT INTO taglist(pos, key) VALUES (?1, ?2)").s()?;
            for (i, k) in keys.iter().enumerate() {
                ins.execute(params![i as i64, k]).s()?;
            }
            Ok(())
        })
    }
}

/// `row` plus one more parameter, as the statement's parameter list.
fn bind<'a>(
    row: &[&'a dyn ToSql; 19],
    last: &'a dyn ToSql,
) -> rusqlite::ParamsFromIter<Vec<&'a dyn ToSql>> {
    params_from_iter(row.iter().copied().chain([last]).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("openfader-coll-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn t(dir: &Path, name: &str, body: &[u8]) -> Track {
        std::fs::write(dir.join(name), body).unwrap();
        Track {
            title: name.into(),
            file: Some(dir.join(name)),
            ..Track::default()
        }
    }

    fn cell(s: &Store, id: i64, col: &str) -> i64 {
        scalar(&s.db(), &format!("SELECT {col} FROM collection WHERE id = ?1"), [id]).unwrap().unwrap()
    }

    #[test]
    fn put_fills_the_canonical_columns_and_lists_rows_with_ids() {
        let d = tmp("put");
        let s = Store::in_memory().unwrap();
        let a = Track {
            key: "1m".into(), // Open Key: A minor, 8A
            date_added: "2020-01-02".into(),
            ..t(&d, "a.mp3", b"12345")
        };
        assert_eq!(s.collection_put(&[a.clone(), Track::default()], false).unwrap(), 1); // no file: skipped
        let rows = s.collection_tracks().unwrap();
        let id = rows[0].id.unwrap();
        assert_eq!((rows.len(), rows[0].title.as_str()), (1, "a.mp3"));
        assert_eq!(s.collection_track(id).unwrap().unwrap(), rows[0]);
        assert_eq!(
            (cell(&s, id, "size"), cell(&s, id, "key_id"), cell(&s, id, "added_at")),
            (5, 14, 1577923200)
        );
        assert!(cell(&s, id, "mtime") > 0);
        // A later put of the same file refreshes size and mtime, nothing else, and counts nothing.
        std::fs::write(a.file.as_ref().unwrap(), b"1234567").unwrap();
        let again = Track { title: "other".into(), ..a.clone() };
        assert_eq!(s.collection_put(&[again], false).unwrap(), 0);
        assert_eq!((cell(&s, id, "size"), s.collection_tracks().unwrap()[0].title.clone()), (7, "a.mp3".into()));
        // Undated tracks are dated now; a bad key has no id.
        let b = Track { key: "??".into(), ..t(&d, "b.mp3", b"b") };
        s.collection_put(&[b.clone()], false).unwrap();
        let ib = s.id_of_path(b.file.as_deref().unwrap()).unwrap().unwrap();
        assert!(cell(&s, ib, "added_at") > 1577923200);
        assert_eq!(scalar::<Option<i64>>(&s.db(), "SELECT key_id FROM collection WHERE id = ?1", [ib]).unwrap().unwrap(), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_cached_list_follows_writes_but_not_fingerprints() {
        let d = tmp("cache");
        let s = Store::in_memory().unwrap();
        s.collection_put(&[t(&d, "a.mp3", b"a")], false).unwrap();
        let first = s.collection_tracks().unwrap();
        assert!(Arc::ptr_eq(&first, &s.collection_tracks().unwrap())); // nothing changed: the same list
        s.set_rating("t1", Some(3)).unwrap(); // an edit of another table
        assert!(Arc::ptr_eq(&first, &s.collection_tracks().unwrap()));
        s.set_fingerprint(first[0].id.unwrap(), "00ff").unwrap();
        assert!(Arc::ptr_eq(&first, &s.collection_tracks().unwrap())); // not part of a listed track
        s.collection_put(&[t(&d, "b.mp3", b"bb")], false).unwrap();
        let second = s.collection_tracks().unwrap();
        assert_eq!(second.len(), 2);
        s.collection_delete(first[0].id.unwrap()).unwrap();
        assert_eq!(s.collection_tracks().unwrap().len(), 1);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn fingerprints_find_rows_and_upsert_overwrites_in_place() {
        let d = tmp("fp");
        let s = Store::in_memory().unwrap();
        let a = t(&d, "a.mp3", b"a");
        let id = s.collection_upsert(&a, None).unwrap();
        assert_eq!(s.fingerprint(id).unwrap(), None);
        s.set_fingerprint(id, "abc").unwrap();
        assert_eq!(s.find_by_fingerprint("abc").unwrap(), [id]);
        assert_eq!((s.fingerprint(id).unwrap().as_deref(), s.find_by_fingerprint("x").unwrap()), (Some("abc"), vec![]));
        // The same row, moved to another file and retagged.
        let moved = Track { title: "A2".into(), ..t(&d, "moved.mp3", b"m") };
        assert_eq!(s.collection_upsert(&moved, Some(id)).unwrap(), id);
        let rows = s.collection_tracks().unwrap();
        assert_eq!((rows.len(), rows[0].title.as_str(), rows[0].file.clone()), (1, "A2", moved.file.clone()));
        assert_eq!(s.collection_upsert(&moved, None).unwrap(), id); // found by its path
        assert!(s.collection_upsert(&moved, Some(99)).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn removing_forgets_the_row_remembers_the_path_and_relocating_keeps_the_id() {
        let d = tmp("rel");
        let s = Store::in_memory().unwrap();
        let (a, b) = (t(&d, "a.mp3", b"a"), t(&d, "b.mp3", b"b"));
        s.collection_put(&[a.clone(), b.clone()], false).unwrap();
        let ids: Vec<i64> = s.collection_tracks().unwrap().iter().filter_map(|t| t.id).collect();
        s.set_rating(&id_key(ids[0]), Some(5)).unwrap();
        // Relocated: same id, same edits, the new path; the old one is remembered.
        let new = d.join("moved.mp3");
        std::fs::rename(a.file.as_ref().unwrap(), &new).unwrap();
        s.relocate(&s.collection_track(ids[0]).unwrap().unwrap(), &new).unwrap();
        assert_eq!(s.id_of_path(&new).unwrap(), Some(ids[0]));
        assert_eq!(s.edits(&id_key(ids[0])).unwrap().rating, Some(5));
        assert_eq!(s.relocations().unwrap().get(a.file.as_ref().unwrap()), Some(&new));
        assert_eq!(cell(&s, ids[0], "size"), 1);
        // Removed: the row is gone and a put brings it back.
        s.collection_remove(&s.collection_track(ids[1]).unwrap().unwrap()).unwrap();
        assert_eq!(s.removed_ids().unwrap().into_iter().collect::<Vec<_>>(), [b.file.as_ref().unwrap().to_string_lossy().to_string()]);
        assert_eq!(s.collection_tracks().unwrap().len(), 1);
        s.collection_put(&[b], false).unwrap();
        assert!(s.removed_ids().unwrap().is_empty());
        s.set_taglist(&["p".into(), "q".into()]).unwrap();
        s.set_taglist(&["q".into()]).unwrap();
        assert_eq!(s.taglist().unwrap(), ["q"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn ids_of_deleted_rows_are_never_reused() {
        let d = tmp("hwm");
        let s = Store::in_memory().unwrap();
        let a = s.collection_upsert(&t(&d, "a.mp3", b"a"), None).unwrap();
        let b = s.collection_upsert(&t(&d, "b.mp3", b"b"), None).unwrap();
        s.collection_delete(b).unwrap();
        let c = s.collection_upsert(&t(&d, "c.mp3", b"c"), None).unwrap();
        assert!(a < b && b < c);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_row_whose_file_was_offline_adopts_its_legacy_edits_when_the_file_is_seen() {
        let d = tmp("offline");
        let s = Store::in_memory().unwrap();
        let a = t(&d, "a.mp3", b"12345");
        let ia = s.collection_upsert(&a, None).unwrap();
        s.set_rating("a.mp3|5", Some(4)).unwrap();
        // As after an upgrade with the drive unplugged: no size known.
        s.run("UPDATE collection SET size = 0, mtime = 0", []).unwrap();
        s.system(|| rekey::backfill(&mut s.db())).unwrap(); // the next start, drive back
        assert_eq!(s.edits(&id_key(ia)).unwrap().rating, Some(4));
        assert_eq!(cell(&s, ia, "size"), 5);
        // The same through a put of the file.
        let c = t(&d, "c.mp3", b"12");
        let ic = s.collection_upsert(&c, None).unwrap();
        s.set_rating("c.mp3|2", Some(2)).unwrap();
        s.run("UPDATE collection SET size = 0 WHERE id = ?1", [ic]).unwrap();
        s.collection_put(&[c], false).unwrap();
        assert_eq!(s.edits(&id_key(ic)).unwrap().rating, Some(2));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
