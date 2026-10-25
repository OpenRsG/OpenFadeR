//! "Update Collection": what was edited on the player (cue and loop points, hot cues, beat grids, colours, ratings, comments) is
//! read back from the device by its plugin ([`Provider::update`]) and applied here to the collection, i.e. to our store
//! ([`crate::store`]).
//!
//! Two steps, so that the user can look first: [`plan`] asks the plugin and lays out one [`Row`] per track with what would change
//! ([`Diff`]), writing nothing; [`apply`] writes the rows that are ticked, keeps the way back ([`super::undo`]) and remembers the
//! baselines of the tracks it dealt with. A track left unticked keeps its old baseline, so the next UPDATE COLLECTION reports it
//! again.
//!
//! A device track is a collection track when both have the same file name and size (the plugin's [`TrackEdit::key`]); the plugin's
//! key is mapped to the collection track's [`store::key`](crate::store::key) here, from the collection [`Snapshot`]. Tracks
//! exported from another computer, or no longer in the collection, are not updated. Collection edits made after the export must
//! not be undone by a device that was not touched, so each export records baselines (the plugin's signature of what the device
//! held then, [`Store::device_sigs`]): the plugin only reports tracks whose signature changed since.
//!
//! The collection is a [`Snapshot`] of the (overlaid) library, so the work can run on a thread: its tracks carry the store's
//! rating and comment, its analysis the store's cues and grid.

use super::link::{same_beats, same_cues};
use super::undo::{self, Change, Entry, Field};
use crate::analysis::{key, Analysis, Beat, Cue};
use crate::library::Track;
use crate::library::snapshot::Snapshot;
use crate::plugin::device::{Control, Provider, TrackEdit, UpdateReport};
use crate::store::Store;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// What [`apply`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Updated {
    /// Device tracks that are collection tracks.
    pub matched: usize,
    /// Matched tracks with at least one change imported.
    pub changed: usize,
    /// Device tracks not in the collection (not updated).
    pub unmatched: usize,
    pub ratings: usize,
    pub comments: usize,
    pub colors: usize,
    pub cues: usize,
    pub grids: usize,
    pub keys: usize,
}

/// One thing that would change on a track.
#[derive(Clone, Debug, PartialEq)]
pub enum Diff {
    Rating { from: u8, to: u8 },
    Comment { from: String, to: String },
    /// Track colour ids 1..=8 ([`crate::store::TRACK_COLORS`]); `from` `None`: none.
    Color { from: Option<u8>, to: u8 },
    /// Cues the player added or deleted, and hot cues it moved.
    Cues { added: usize, removed: usize, moved: usize },
    /// The first beat moved by `moved_ms` (device minus collection); `bpm` = (collection, device) in 1/100 BPM when it differs.
    Grid { moved_ms: i64, bpm: Option<(u16, u16)> },
    /// The musical key (as each side writes it).
    Key { from: String, to: String },
}

/// One track of a [`Plan`].
#[derive(Clone, Debug)]
pub struct Row {
    /// The collection tracks' [`crate::store::key`]s the edit goes to (the plugin's own key when none was found).
    pub keys: Vec<String>,
    /// "Title — Artist" of the collection track.
    pub title: String,
    pub artist: String,
    pub diff: Vec<Diff>,
    edit: TrackEdit,
    /// The device's musical key, to write ([`row_of`]; UPDATE COLLECTION does not carry one).
    key: Option<String>,
}

/// What the player changed: nothing is written until [`apply`].
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Device tracks that are collection tracks.
    pub matched: usize,
    /// Device tracks not in the collection.
    pub unmatched: usize,
    pub rows: Vec<Row>,
    baselines: Vec<(String, i64)>,
}

/// Cues added, deleted and (hot cues) moved between `old` and `new`: a hot cue is the same cue by its slot, a memory cue or loop by
/// its position.
fn cue_delta(old: &[Cue], new: &[Cue]) -> (usize, usize, usize) {
    let (mut added, mut moved) = (0, 0);
    let mut left: Vec<&Cue> = old.iter().collect();
    for n in new {
        let found = left.iter().position(|o| {
            if n.hot_cue > 0 {
                o.hot_cue == n.hot_cue
            } else {
                o.hot_cue == 0 && o.time_ms.abs_diff(n.time_ms) <= 1
            }
        });
        match found {
            Some(i) => {
                let o = left.remove(i);
                moved += usize::from(o.time_ms.abs_diff(n.time_ms) > 1);
            }
            None => added += 1,
        }
    }
    (added, left.len(), moved)
}

/// What `e` would change on a track that has `now` (rating, comment, colour, cues, grid), as the user is told.
fn diffs(e: &TrackEdit, rating: u8, comment: &str, color: Option<u8>, cues: &[Cue], beats: &[Beat]) -> Vec<Diff> {
    let mut d = Vec::new();
    if let Some(to) = e.rating.filter(|&r| r != rating) {
        d.push(Diff::Rating { from: rating, to });
    }
    if let Some(to) = e.comment.as_deref().filter(|&c| c != comment) {
        d.push(Diff::Comment {
            from: comment.to_owned(),
            to: to.to_owned(),
        });
    }
    if let Some(to) = e.color.filter(|&c| Some(c) != color) {
        d.push(Diff::Color { from: color, to });
    }
    if let Some(new) = e.cues.as_deref().filter(|n| !same_cues(n, cues)) {
        let (added, removed, moved) = cue_delta(cues, new);
        d.push(Diff::Cues { added, removed, moved });
    }
    if let Some((now, was)) = e.grid.as_ref().filter(|(n, _)| !same_beats(n, beats)) {
        let first = |b: &[Beat]| b.first().map(|b| (i64::from(b.time_ms), b.bpm_x100));
        let (n, w) = (first(now), first(was));
        d.push(Diff::Grid {
            moved_ms: n.zip(w).map_or(0, |(n, w)| n.0 - w.0),
            bpm: n.zip(w).filter(|(n, w)| n.1 != w.1).map(|(n, w)| (w.1, n.1)),
        });
    }
    d
}

/// Lays out `report` against the collection `snap`: one row per track with something to change.
pub fn plan_of(report: UpdateReport, snap: &Snapshot, store: &Store) -> Plan {
    // The plugin names a track by file name and size: candidates by name first, the size is read only for those.
    let wanted: HashSet<&str> = report
        .edits
        .iter()
        .filter_map(|e| e.key.rsplit_once('|').map(|(n, _)| n))
        .collect();
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, (t, _)) in snap.tracks().iter().enumerate() {
        if let Some(n) = t.file.as_deref().and_then(Path::file_name).map(|n| n.to_string_lossy()) {
            if wanted.contains(&*n) {
                by_name.entry(n.into_owned()).or_default().push(i);
            }
        }
    }
    let mut rows = Vec::new();
    for e in &report.edits {
        let found: Vec<usize> = e
            .key
            .rsplit_once('|')
            .and_then(|(n, size)| Some((by_name.get(n)?, size.parse::<u64>().ok()?)))
            .map(|(c, size)| {
                c.iter()
                    .copied()
                    .filter(|&i| {
                        snap.tracks()[i].0.file.as_deref().and_then(|f| std::fs::metadata(f).ok()).map_or(0, |m| m.len()) == size
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (keys, title, artist, diff) = match found.first().map(|&i| &snap.tracks()[i]) {
            Some((t, a)) => {
                let key = store.key_of(t);
                let empty = Default::default();
                let a = a.as_ref().unwrap_or(&empty);
                let color = store.track_color(&key).ok().flatten();
                (
                    found.iter().map(|&i| store.key_of(&snap.tracks()[i].0)).collect(),
                    t.title.clone(),
                    t.artist.clone(),
                    diffs(e, t.rating, &t.comment, color, &a.cues, &a.beats),
                )
            }
            None => {
                let name = e.key.rsplit_once('|').map_or(e.key.as_str(), |(n, _)| n);
                (vec![e.key.clone()], name.to_owned(), String::new(), diffs(e, 0, "", None, &[], &[]))
            }
        };
        if !diff.is_empty() {
            rows.push(Row { keys, title, artist, diff, edit: e.clone(), key: None });
        }
    }
    Plan {
        matched: report.matched,
        unmatched: report.unmatched,
        rows,
        baselines: report.baselines,
    }
}

/// A device track `dev` that the collection holds as `col` (row `key`, with the store's edits applied; `col_a` its analysis with
/// them, `color` its colour) as an import from the device sees it: every value the device has that the collection's copy does not
/// share, taking the device's (no baselines: the device holds what it holds). `None` when the copies agree.
pub fn row_of(key: String, col: &Track, col_a: &Analysis, color: Option<u8>, dev: &Track, dev_a: Option<&Analysis>) -> Option<Row> {
    let marks = |f: fn(&Analysis) -> bool| dev_a.filter(|a| f(a));
    let edit = TrackEdit {
        key: key.clone(),
        rating: (dev.rating > 0).then_some(dev.rating),
        comment: (!dev.comment.is_empty()).then(|| dev.comment.clone()),
        color: None,
        cues: marks(|a| !a.cues.is_empty()).map(|a| a.cues.clone()),
        grid: marks(|a| !a.beats.is_empty()).map(|a| (a.beats.clone(), col_a.beats.clone())),
    };
    let mut diff = diffs(&edit, col.rating, &col.comment, color, &col_a.cues, &col_a.beats);
    // The same key in two notations (8A, Am) is no difference.
    let same = |a: &str, b: &str| match (key::key_id(a), key::key_id(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    };
    let new_key = (!dev.key.is_empty() && !same(&dev.key, &col.key)).then(|| dev.key.clone());
    if let Some(k) = &new_key {
        diff.push(Diff::Key { from: col.key.clone(), to: k.clone() });
    }
    (!diff.is_empty()).then(|| Row {
        keys: vec![key],
        title: col.title.clone(),
        artist: col.artist.clone(),
        diff,
        edit,
        key: new_key,
    })
}

/// [`row_of`]'s rows as a plan for [`apply`]: no baselines to remember.
pub fn of_rows(rows: Vec<Row>) -> Plan {
    Plan { matched: rows.len(), unmatched: 0, rows, baselines: Vec::new() }
}

/// Asks `provider` what changed on the player since the baselines of `device_id`; writes nothing.
pub fn plan(
    provider: &Provider,
    root: &Path,
    device_id: &str,
    collection: &Snapshot,
    store: &Store,
    ctl: &Arc<Control>,
) -> Result<Plan, String> {
    let baselines = store.device_sigs(device_id)?.into_iter().collect();
    Ok(plan_of(
        provider.update(root, collection, &baselines, ctl)?,
        collection,
        store,
    ))
}

/// What [`apply`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub updated: Updated,
    /// The kept way back ([`undo::run`]); `None` when nothing changed.
    pub undo: Option<i64>,
}

/// Writes the rows of `plan` that are ticked (`ticked[i]` for row `i`) to `store` (one Edit > Undo step, and one kept way back
/// called `label`) and remembers the baselines of every track but the unticked ones.
pub fn apply(
    plan: &Plan,
    ticked: &[bool],
    device_id: &str,
    label: &str,
    store: &Store,
    now: i64,
) -> Result<Applied, String> {
    let on = |i: usize| ticked.get(i).copied().unwrap_or(false);
    let mut u = Updated {
        matched: plan.matched,
        unmatched: plan.unmatched,
        ..Updated::default()
    };
    let mut entry = Entry::default();
    store.action("Update Collection", || -> Result<(), String> {
        for (_, row) in plan.rows.iter().enumerate().filter(|(i, _)| on(*i)) {
            let e = &row.edit;
            for key in &row.keys {
                let mut fields = Vec::new();
                fields.extend(e.rating.map(|r| Field::Rating(Some(r.min(5)))));
                fields.extend(e.comment.clone().map(|c| Field::Comment(Some(c))));
                fields.extend(e.color.filter(|c| (1..=8).contains(c)).map(|c| Field::Color(Some(c))));
                fields.extend(e.cues.clone().map(|c| Field::Cues(Some(c))));
                fields.extend(e.grid.as_ref().map(|(now, _)| Field::Grid(Some(now.clone()))));
                fields.extend(row.key.clone().map(|k| Field::Key(Some(k))));
                for f in fields {
                    let Some(c) = Change::of(store, key, f)? else { continue };
                    match (&c.after, &e.grid) {
                        (Field::Grid(Some(g)), Some((_, was))) => store.set_grid(key, g, was)?,
                        _ => c.after.write(store, key)?,
                    }
                    entry.changes.push(c);
                }
            }
            for d in &row.diff {
                match d {
                    Diff::Rating { .. } => u.ratings += 1,
                    Diff::Comment { .. } => u.comments += 1,
                    Diff::Color { .. } => u.colors += 1,
                    Diff::Cues { .. } => u.cues += 1,
                    Diff::Grid { .. } => u.grids += 1,
                    Diff::Key { .. } => u.keys += 1,
                }
            }
            u.changed += 1;
        }
        let unticked: HashSet<&str> = plan
            .rows
            .iter()
            .enumerate()
            .filter(|(i, _)| !on(*i))
            .map(|(_, r)| r.edit.key.as_str())
            .collect();
        let kept: Vec<_> = plan.baselines.iter().filter(|(k, _)| !unticked.contains(k.as_str())).cloned().collect();
        store.set_device_sigs(device_id, &kept)
    })?;
    Ok(Applied {
        updated: u,
        undo: undo::keep(store, label, device_id, &entry, now)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Track;

    fn beat(ms: u32, bpm: u16) -> Beat {
        Beat { bar_pos: 1, bpm_x100: bpm, time_ms: ms }
    }

    fn cue(hot: u16, ms: u32) -> Cue {
        Cue { hot_cue: hot, time_ms: ms, loop_end_ms: None, color: None, comment: None }
    }

    /// A collection of one track `x.mp3` (row id 7, rating 3) with a file of 5 bytes, and the key the plugin names it by.
    fn collection(dir: &Path) -> (Snapshot, String) {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join("x.mp3");
        std::fs::write(&file, b"12345").unwrap();
        let t = Track { id: Some(7), file: Some(file), title: "T".into(), artist: "A".into(), rating: 3, ..Track::default() };
        let a = crate::analysis::Analysis { cues: vec![cue(1, 1000), cue(0, 5000)], beats: vec![beat(100, 12000)], ..Default::default() };
        (Snapshot::of_tracks(vec![(t, Some(a))]), "x.mp3|5".into())
    }

    fn report(key: &str) -> UpdateReport {
        UpdateReport {
            matched: 2,
            unmatched: 1,
            edits: vec![
                TrackEdit {
                    key: key.into(),
                    rating: Some(5),
                    comment: Some("hi".into()),
                    color: Some(2),
                    cues: Some(vec![cue(1, 1500), cue(2, 2000)]),
                    grid: Some((vec![beat(112, 12400)], vec![beat(100, 12000)])),
                },
                TrackEdit { key: "gone.mp3|9".into(), rating: Some(2), ..TrackEdit::default() },
                TrackEdit { key: key.into(), rating: Some(3), ..TrackEdit::default() }, // what the collection has already
            ],
            baselines: vec![(key.into(), 7), ("gone.mp3|9".into(), 8)],
        }
    }

    #[test]
    fn a_row_tells_what_changes_and_leaves_out_what_is_the_same() {
        let dir = std::env::temp_dir().join(format!("openfader-update-plan-{}", std::process::id()));
        let (snap, key) = collection(&dir);
        let store = Store::in_memory().unwrap();
        let p = plan_of(report(&key), &snap, &store);
        assert_eq!(p.rows.len(), 2, "the no-op edit is not a row");
        assert_eq!(p.rows[0].keys, ["t7"]);
        assert_eq!((p.rows[0].title.as_str(), p.rows[0].artist.as_str()), ("T", "A"));
        assert_eq!(
            p.rows[0].diff,
            [
                Diff::Rating { from: 3, to: 5 },
                Diff::Comment { from: String::new(), to: "hi".into() },
                Diff::Color { from: None, to: 2 },
                // slot 1 moved, the memory cue deleted, slot 2 added
                Diff::Cues { added: 1, removed: 1, moved: 1 },
                Diff::Grid { moved_ms: 12, bpm: Some((12000, 12400)) },
            ]
        );
        assert_eq!(p.rows[1].keys, ["gone.mp3|9"], "not in the collection: the plugin's key stays");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_ticked_rows_are_written_and_an_unticked_one_keeps_its_baseline() {
        let dir = std::env::temp_dir().join(format!("openfader-update-apply-{}", std::process::id()));
        let (snap, key) = collection(&dir);
        let store = Store::in_memory().unwrap();
        store.set_rating("t7", Some(3)).unwrap();
        store.set_device_sigs("dev", &[("gone.mp3|9".into(), 1)]).unwrap();
        let p = plan_of(report(&key), &snap, &store);
        let a = apply(&p, &[true, false], "dev", "Update Collection of KEY", &store, 1).unwrap();
        assert_eq!(
            a.updated,
            Updated { matched: 2, changed: 1, unmatched: 1, ratings: 1, comments: 1, colors: 1, cues: 1, grids: 1, keys: 0 }
        );
        let e = store.edits("t7").unwrap();
        assert_eq!((e.rating, e.comment.as_deref(), e.color), (Some(5), Some("hi"), Some(2)));
        assert_eq!(e.cues.as_ref().map(Vec::len), Some(2));
        assert_eq!(e.grid, Some(vec![beat(112, 12400)]));
        assert_eq!(store.edits("gone.mp3|9").unwrap().rating, None, "unticked: not written");
        let sigs = store.device_sigs("dev").unwrap();
        assert_eq!((sigs.get("x.mp3|5"), sigs.get("gone.mp3|9")), (Some(&7), Some(&1)));
        // And the way back puts the old values back.
        let u = undo::run(&store, a.undo.unwrap()).unwrap();
        assert_eq!((u.restored, u.kept), (5, 0));
        let e = store.edits("t7").unwrap();
        assert_eq!((e.rating, e.comment, e.color, e.cues, e.grid), (Some(3), None, None, None, None));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
