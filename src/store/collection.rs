//! Our Collection: what the user did to the library that a read-only source cannot hold.
//!
//! * `collection` rows = tracks imported from files, playlists or other libraries (and tag re-reads of source tracks:
//!   a row shadows the source track with the same file). Order = import order.
//! * `removed` = [`key`]s of source tracks the user removed from the Collection (the file is never touched).
//! * `relocated` = original file path → where the user relocated it (Missing File Manager).
//! * `analysis_lock` = keys whose analysis must not be redone; `taglist` = the persisted Tag List.
//!
//! [`Store::collection_view`] lays these over a source's track list; `library::collection` is the `Library` around it.
//! Paths are stored as (lossy) UTF-8 text.

use super::{column, exec, exec_count, exists, in_tx, key, query_all, scalar, Sql, Store, R};
use crate::library::Track;
use openfader_plugin::library::{owned_ref, split_ref};
use rusqlite::{params, Row};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const COLS: &str = "path, title, artist, album, tkey, genre, bpm_x100, length_s, analysis_ref, artwork, comment, date_added, bitrate, sample_rate, label";

fn from_row(r: &Row) -> rusqlite::Result<Track> {
    let opt = |i| r.get::<_, Option<String>>(i).map(|s| s.map(PathBuf::from));
    Ok(Track {
        id: None,
        file: Some(PathBuf::from(r.get::<_, String>(0)?)),
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        key: r.get(4)?,
        genre: r.get(5)?,
        label: r.get(14)?,
        bpm_x100: r.get(6)?,
        length_s: r.get(7)?,
        analysis_ref: r.get(8)?,
        artwork: opt(9)?,
        rating: 0,
        comment: r.get(10)?,
        date_added: r.get(11)?,
        bitrate: r.get(12)?,
        sample_rate: r.get(13)?,
        remote: None,
    })
}

impl Store {
    /// The imported tracks, in import order.
    pub fn collection_tracks(&self) -> R<Vec<Track>> {
        query_all(
            &self.db(),
            &format!("SELECT {COLS} FROM collection ORDER BY id"),
            [],
            from_row,
        )
    }

    /// Adds (or, with `replace`, overwrites by file path) `tracks` that have a file; they stop being "removed". Returns how
    /// many rows were added or changed.
    pub fn collection_put(&self, tracks: &[Track], replace: bool) -> R<usize> {
        let conflict = if replace {
            "ON CONFLICT(path) DO UPDATE SET title=?2, artist=?3, album=?4, tkey=?5, genre=?6, bpm_x100=?7, length_s=?8, analysis_ref=?9, artwork=?10, comment=?11, date_added=?12, bitrate=?13, sample_rate=?14, label=?15"
        } else {
            "ON CONFLICT(path) DO NOTHING"
        };
        let sql = format!(
            "INSERT INTO collection({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) {conflict}"
        );
        self.write(|tx| {
            let mut put = tx.prepare_cached(&sql).s()?;
            let mut unremove = tx.prepare_cached("DELETE FROM removed WHERE id = ?1").s()?;
            let mut n = 0;
            for t in tracks {
                let Some(file) = &t.file else { continue };
                let file = file.to_string_lossy();
                n += put
                    .execute(params![
                        file,
                        t.title,
                        t.artist,
                        t.album,
                        t.key,
                        t.genre,
                        t.bpm_x100,
                        t.length_s,
                        t.analysis_ref,
                        t.artwork.as_deref().map(Path::to_string_lossy),
                        t.comment,
                        t.date_added,
                        t.bitrate,
                        t.sample_rate,
                        t.label
                    ])
                    .s()?;
                unremove.execute([&file]).s()?;
            }
            Ok(n)
        })
    }

    /// Whether `path` (as the track list shows it) is an imported track.
    pub fn in_collection(&self, path: &Path) -> R<bool> {
        exists(
            &self.db(),
            "SELECT 1 FROM collection WHERE path = ?1",
            [path.to_string_lossy()],
        )
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

    /// Removes `t` from the Collection: an imported track's row goes, a source track is hidden (by the path the source
    /// lists it under; a track without a file by its [`key`]). The audio file stays where it is. Playlist entries are
    /// untouched: call [`Store::forget_entries`] once no listed track carries the key any more.
    // ponytail: a source mounted elsewhere lists other paths and shows the track again; hide by key too if that matters.
    pub fn collection_remove(&self, t: &Track) -> R<()> {
        let db = self.db();
        let id = match &t.file {
            Some(f) => {
                let shown = f.to_string_lossy();
                exec(&db, "DELETE FROM collection WHERE path = ?1", [&shown])?;
                scalar::<String>(&db, "SELECT orig FROM relocated WHERE path = ?1", [&shown])?
                    .unwrap_or_else(|| shown.into_owned())
            }
            None => key(t),
        };
        exec(&db, "INSERT OR IGNORE INTO removed(id) VALUES (?1)", [id])
    }

    /// Deletes every playlist entry naming `key` and closes the gaps.
    pub fn forget_entries(&self, key: &str) -> R<()> {
        let nodes: Vec<i64> = column(
            &self.db(),
            "SELECT DISTINCT node FROM entry WHERE key = ?1",
            [key],
        )?;
        for n in nodes {
            let kept: Vec<String> = self.entries(n)?.into_iter().filter(|k| k != key).collect();
            self.set_entries(n, &kept)?;
        }
        Ok(())
    }

    /// Source tracks the user removed: the path the source lists them under (their key when they have no file).
    fn removed_ids(&self) -> R<HashSet<String>> {
        column(&self.db(), "SELECT id FROM removed", [])
    }

    /// Original path → relocated path.
    fn relocations(&self) -> R<HashMap<PathBuf, PathBuf>> {
        query_all(&self.db(), "SELECT orig, path FROM relocated", [], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                PathBuf::from(r.get::<_, String>(1)?),
            ))
        })
    }

    /// The file `old` now lives at `new` (the track list shows `new` from now on). The store's edits follow when the
    /// identity ([`key`]) changes with the file (another name or size); `track` is the track as it was listed.
    pub fn relocate(&self, track: &Track, new: &Path) -> R<()> {
        let Some(old) = &track.file else {
            return Err("this track has no file".into());
        };
        self.write(|tx| {
            let (o, n) = (old.to_string_lossy(), new.to_string_lossy());
            let p = [&o, &n];
            exec(
                tx,
                "DELETE FROM collection WHERE path = ?2 AND path != ?1",
                p,
            )?;
            exec(tx, "UPDATE collection SET path = ?2 WHERE path = ?1", p)?;
            if exec_count(tx, "UPDATE relocated SET path = ?2 WHERE path = ?1", p)? == 0 {
                exec(
                    tx,
                    "INSERT INTO relocated(orig, path) VALUES (?1, ?2) ON CONFLICT(orig) DO UPDATE SET path = ?2",
                    p,
                )?;
            }
            Ok(())
        })?;
        // A track with a file is keyed by the file alone.
        let at_new = Track {
            file: Some(new.to_path_buf()),
            ..Track::default()
        };
        self.rekey(&key(track), &key(&at_new))
    }

    /// Everything the store knows under key `from` (edits, cues, playlist entries, tags, plays, lock) moves to `to`. Where
    /// `to` already has its own edits they win; nothing of `from` is kept twice.
    pub fn rekey(&self, from: &str, to: &str) -> R<()> {
        if from == to {
            return Ok(());
        }
        let p = [from, to];
        in_tx(&mut self.db(), true, |tx| {
            if exists(tx, "SELECT 1 FROM track WHERE key = ?1", [to])? {
                exec(tx, "DELETE FROM track WHERE key = ?1", [from])?; // cues follow by cascade
            } else {
                exec(tx, "UPDATE track SET key = ?2 WHERE key = ?1", p)?;
                exec(tx, "UPDATE cue SET key = ?2 WHERE key = ?1", p)?;
            }
            for table in ["entry", "history", "taglist", "hotcue_bank_cue"] {
                exec(
                    tx,
                    &format!("UPDATE {table} SET key = ?2 WHERE key = ?1"),
                    p,
                )?;
            }
            // Keyed rows: the target's own row wins, so the source's moves only where the target has none.
            for table in ["track_mytag", "analysis_lock", "phrase_edit"] {
                exec(
                    tx,
                    &format!("UPDATE OR IGNORE {table} SET key = ?2 WHERE key = ?1"),
                    p,
                )?;
                exec(tx, &format!("DELETE FROM {table} WHERE key = ?1"), [from])?;
            }
            Ok(())
        })
    }

    /// `tracks` (a source's list) with the Collection laid over it: removed ones dropped, relocated files at their new
    /// path, an imported row replacing the source track of the same file, and (when `all`) the imported tracks that
    /// are not in the source appended.
    pub fn collection_view(&self, tracks: Vec<Track>, all: bool) -> R<Vec<Track>> {
        let rows = self.collection_tracks()?;
        let (removed, moved) = (self.removed_ids()?, self.relocations()?);
        if rows.is_empty() && removed.is_empty() && moved.is_empty() {
            return Ok(tracks);
        }
        let hidden = |t: &Track, orig: Option<&Path>| match orig {
            Some(p) => removed.contains(&*p.to_string_lossy()),
            None => removed.contains(&key(t)),
        };
        let at: HashMap<&Path, usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| Some((r.file.as_deref()?, i)))
            .collect();
        let mut used = vec![false; rows.len()];
        let mut out = Vec::with_capacity(tracks.len());
        for mut t in tracks {
            let orig = t.file.take();
            let hide = hidden(&t, orig.as_deref());
            t.file = orig.map(|f| moved.get(&f).cloned().unwrap_or(f));
            if let Some(&i) = t.file.as_deref().and_then(|f| at.get(f)) {
                used[i] = true;
                t = Track {
                    rating: t.rating,
                    ..rows[i].clone()
                };
            }
            if !hide {
                out.push(t);
            }
        }
        if all {
            out.extend(
                rows.into_iter()
                    .zip(used)
                    .filter(|(_, u)| !u)
                    .map(|(r, _)| r),
            );
        }
        Ok(out)
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
        exists(
            &self.db(),
            "SELECT 1 FROM analysis_lock WHERE key = ?1",
            [key],
        )
    }

    /// The persisted Tag List: track keys in queue order.
    pub fn taglist(&self) -> R<Vec<String>> {
        column(&self.db(), "SELECT key FROM taglist ORDER BY pos", [])
    }

    pub fn set_taglist(&self, keys: &[String]) -> R<()> {
        self.write(|tx| {
            exec(tx, "DELETE FROM taglist", [])?;
            let mut ins = tx
                .prepare_cached("INSERT INTO taglist(pos, key) VALUES (?1, ?2)")
                .s()?;
            for (i, k) in keys.iter().enumerate() {
                ins.execute(params![i as i64, k]).s()?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Cue;

    fn t(title: &str, file: &str) -> Track {
        Track {
            title: title.into(),
            file: Some(file.into()),
            ..Track::default()
        }
    }

    #[test]
    fn view_hides_removed_imports_and_relocates() {
        let s = Store::in_memory().unwrap();
        let (a, b) = (t("A", "/m/a.mp3"), t("B", "/m/b.mp3"));
        // Nothing stored: the source list passes through untouched.
        assert_eq!(
            s.collection_view(vec![a.clone()], true).unwrap(),
            vec![a.clone()]
        );

        assert_eq!(
            s.collection_put(&[b.clone(), t("C", "/n/c.mp3")], false)
                .unwrap(),
            2
        );
        assert_eq!(s.collection_put(&[b.clone()], false).unwrap(), 0); // already there
        let titles = |v: Vec<Track>| v.into_iter().map(|t| t.title).collect::<Vec<_>>();
        assert_eq!(
            titles(s.collection_view(vec![a.clone()], true).unwrap()),
            ["A", "B", "C"]
        );
        assert_eq!(
            titles(s.collection_view(vec![a.clone()], false).unwrap()),
            ["A"]
        ); // a playlist query adds nothing

        // A re-read tag of a source track shadows it, keeping the source's rating.
        let mut src = a.clone();
        src.rating = 4;
        s.collection_put(
            &[Track {
                title: "A2".into(),
                ..a.clone()
            }],
            false,
        )
        .unwrap();
        let v = s.collection_view(vec![src], true).unwrap();
        assert_eq!((v[0].title.as_str(), v[0].rating, v.len()), ("A2", 4, 3));

        // Removing hides a source track and drops an imported row; re-importing brings it back.
        s.collection_remove(&a).unwrap();
        s.collection_remove(&b).unwrap();
        assert_eq!(
            titles(s.collection_view(vec![a.clone()], true).unwrap()),
            ["C"]
        );
        s.collection_put(&[a.clone()], false).unwrap();
        assert_eq!(
            titles(s.collection_view(vec![a], true).unwrap()),
            ["A", "C"]
        );
    }

    #[test]
    fn relocate_moves_the_row_the_source_track_and_chains() {
        let s = Store::in_memory().unwrap();
        let src = t("S", "/old/s.mp3");
        let imp = t("I", "/old/i.mp3");
        s.collection_put(&[imp.clone()], false).unwrap();
        s.relocate(&src, Path::new("/new/s.mp3")).unwrap();
        s.relocate(&imp, Path::new("/new/i.mp3")).unwrap();
        let v = s.collection_view(vec![src.clone()], true).unwrap();
        let files: Vec<_> = v.iter().map(|t| t.file.clone().unwrap()).collect();
        assert_eq!(
            files,
            [PathBuf::from("/new/s.mp3"), PathBuf::from("/new/i.mp3")]
        );
        // Relocated again: still one entry, the original path now maps to the latest place.
        s.relocate(
            &Track {
                file: Some("/new/s.mp3".into()),
                ..src.clone()
            },
            Path::new("/newer/s.mp3"),
        )
        .unwrap();
        assert_eq!(
            s.collection_view(vec![src], false).unwrap()[0].file,
            Some(PathBuf::from("/newer/s.mp3"))
        );
        assert_eq!(s.relocations().unwrap().len(), 2);
    }

    #[test]
    fn rekey_moves_edits_entries_and_prefers_the_target() {
        let s = Store::in_memory().unwrap();
        let cue = Cue {
            hot_cue: 1,
            time_ms: 500,
            loop_end_ms: None,
            color: None,
            comment: None,
        };
        s.set_rating("a|1", Some(3)).unwrap();
        s.set_cues("a|1", Some(&[cue.clone()])).unwrap();
        let pl = s
            .add_node(None, "P", crate::store::NodeKind::Playlist)
            .unwrap();
        s.add_entries(pl, &["a|1".into(), "z|9".into()]).unwrap();
        s.set_analysis_lock("a|1", true).unwrap();
        s.rekey("a|1", "b|2").unwrap();
        let e = s.edits("b|2").unwrap();
        assert_eq!((e.rating, e.cues), (Some(3), Some(vec![cue])));
        assert_eq!(s.entries(pl).unwrap(), ["b|2", "z|9"]);
        assert!(s.analysis_locked("b|2").unwrap() && !s.analysis_locked("a|1").unwrap());
        assert_eq!(s.edits("a|1").unwrap().rating, None);
        // Target already edited: its values stay, the source's are gone.
        s.set_rating("c|3", Some(5)).unwrap();
        s.rekey("b|2", "c|3").unwrap();
        assert_eq!(s.edits("c|3").unwrap().rating, Some(5));
        assert_eq!(s.edits("b|2").unwrap(), Default::default());
    }

    #[test]
    fn removing_a_track_closes_playlist_gaps_and_taglist_round_trips() {
        let s = Store::in_memory().unwrap();
        let x = t("X", "/m/x.mp3");
        let pl = s
            .add_node(None, "P", crate::store::NodeKind::Playlist)
            .unwrap();
        s.add_entries(pl, &["k1".into(), key(&x), "k3".into()])
            .unwrap();
        s.forget_entries(&key(&x)).unwrap();
        assert_eq!(s.entries(pl).unwrap(), ["k1", "k3"]);
        s.set_taglist(&["p".into(), "q".into()]).unwrap();
        s.set_taglist(&["q".into()]).unwrap();
        assert_eq!(s.taglist().unwrap(), ["q"]);
    }
}
