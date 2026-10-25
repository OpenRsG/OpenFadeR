//! "Update Collection" (rekordbox: right-click a device > Update Collection, spec p3791-3808): what was edited on the player (cue
//! and loop points, hot cues, beat grids, colours, ratings, comments) is read back from the device library and its analysis
//! files and returned as [`TrackEdit`]s; the host applies them to its store.
//!
//! A device track is a collection track when both have the same [`track_key`] (file name and size); tracks exported from another
//! computer, or no longer in the collection, are not updated (spec). Collection edits made after the export must not be undone by
//! a device that was not touched, so each export returns baselines ([`baselines`]: a signature of what the device held then) and
//! only tracks whose signature changed since count as edited on the player (inferred; rekordbox's own bookkeeping is not
//! documented). A track without a baseline is compared with the collection directly.
//!
//! The collection is a [`Snapshot`] of the (overlaid) library: its tracks carry the store's rating and comment, its analysis the
//! store's cues and grid, its colours the store's track colours.

use super::pdb::{db::Item, read};
use super::progress::Progress;
use super::settings::DeviceConfig;
use openfader_plugin::library::{
    Analysis, Beat, Cue, Snapshot, Track, TrackEdit, UpdateReport, host, track_key,
};
use rekordbox_common::{anlz, on_device};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{self, Write};
use std::path::Path;

/// A track as the device holds it.
struct Dev {
    rating: u8,
    comment: String,
    color: u8,
    /// `None` when the device has no readable analysis for it.
    anlz: Option<(Vec<Cue>, Vec<Beat>)>,
}

/// Cues in a comparable form: sorted, a missing colour as black (the `PCP2` writer stores three colour bytes always).
fn norm(mut c: Vec<Cue>) -> Vec<Cue> {
    c.sort_by_key(|c| (c.hot_cue, c.time_ms));
    c.into_iter()
        .map(|c| Cue {
            color: c.color.or(Some([0; 3])),
            ..c
        })
        .collect()
}

impl Dev {
    fn load(root: &Path, it: &Item) -> Self {
        let anlz = anlz::load_track(&on_device(root, &it.analyze_path))
            .ok()
            .map(|a| (norm(a.cues), a.beats));
        Self {
            rating: it.rating,
            comment: it.comment.clone(),
            color: it.color,
            anlz,
        }
    }

    /// FNV-1a over a canonical text of everything [`update`] reads (stable across builds, unlike `DefaultHasher`).
    fn sig(&self) -> i64 {
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        let _ = write!(h, "{}|{}|{}", self.rating, self.comment, self.color);
        for c in self.anlz.iter().flat_map(|a| &a.0) {
            let _ = write!(
                h,
                "|c{}:{}:{:?}:{:?}:{:?}",
                c.hot_cue, c.time_ms, c.loop_end_ms, c.color, c.comment
            );
        }
        for b in self.anlz.iter().flat_map(|a| &a.1) {
            let _ = write!(h, "|b{}:{}:{}", b.time_ms, b.bpm_x100, b.bar_pos);
        }
        // Stored as SQLite's signed 64-bit integer: the same bits.
        h.0 as i64
    }
}

/// FNV-1a over the bytes written to it, so [`Dev::sig`] hashes its text without building it.
struct Fnv(u64);

impl Write for Fnv {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 = s.bytes().fold(self.0, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        Ok(())
    }
}

/// `(collection key, index into the collection, device track)` for every device track that is a collection track, and how many are not.
fn matched(
    root: &Path,
    items: &[Item],
    collection: &Snapshot,
    progress: &mut Progress,
) -> Result<(Vec<(String, usize, Dev)>, usize), String> {
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (i, (t, _)) in collection.tracks.iter().enumerate() {
        by_key.entry(track_key(t)).or_insert(i);
    }
    progress.phase("Reading the library", items.len())?;
    let (mut out, mut unmatched) = (Vec::new(), 0);
    for (n, it) in items.iter().enumerate() {
        progress.tick(n)?;
        let file = on_device(root, &it.file_path);
        let found = file
            .is_file()
            .then(|| {
                let key = track_key(&Track {
                    file: Some(file),
                    ..Track::default()
                });
                by_key.get(&key).map(|&i| (key, i))
            })
            .flatten();
        match found {
            Some((key, i)) => out.push((key, i, Dev::load(root, it))),
            None => unmatched += 1,
        }
    }
    Ok((out, unmatched))
}

/// The cues and grid the collection has for track `i`: what the snapshot carries, else the track's own analysis.
fn current(collection: &Snapshot, i: usize) -> Analysis {
    let (t, a) = &collection.tracks[i];
    a.clone()
        .or_else(|| host::analysis(t, false))
        .unwrap_or_default()
}

/// What changed on the player since `baselines`; see the module docs for what counts as changed.
pub fn update(
    root: &Path,
    collection: &Snapshot,
    baselines: &BTreeMap<String, i64>,
) -> Result<UpdateReport, String> {
    let (db, _) = read::open(root, &DeviceConfig::default())?;
    let mut progress = Progress::new();
    let (found, unmatched) = matched(root, &db.tracks, collection, &mut progress)?;
    let mut report = UpdateReport {
        matched: found.len(),
        unmatched,
        ..UpdateReport::default()
    };
    // The collection's own track colour (0 = none), by index into its tracks.
    let colors: HashMap<usize, u8> = collection.colors.iter().copied().collect();
    for (key, i, dev) in found {
        let sig = dev.sig();
        if baselines.get(&key) == Some(&sig) {
            continue; // as we exported it: nothing was edited on the player
        }
        let t = &collection.tracks[i].0;
        let mut edit = TrackEdit {
            key: key.clone(),
            ..TrackEdit::default()
        };
        if dev.rating != t.rating {
            edit.rating = Some(dev.rating);
        }
        if dev.comment != t.comment {
            edit.comment = Some(dev.comment.clone());
        }
        if dev.color != colors.get(&i).copied().unwrap_or(0) {
            edit.color = Some(dev.color);
        }
        if let Some((cues, beats)) = &dev.anlz {
            let now = current(collection, i);
            if *cues != norm(now.cues) {
                edit.cues = Some(cues.clone());
            }
            if !beats.is_empty() && *beats != now.beats {
                edit.grid = Some((beats.clone(), now.beats));
            }
        }
        if edit.rating.is_some()
            || edit.comment.is_some()
            || edit.color.is_some()
            || edit.cues.is_some()
            || edit.grid.is_some()
        {
            report.edits.push(edit);
        }
        report.baselines.push((key, sig));
    }
    Ok(report)
}

/// `(collection key, signature)` of what the device holds for the device tracks with these ids (just written), so the next
/// [`update`] only imports what the player changes after this.
pub fn baselines(
    root: &Path,
    ids: &[u32],
    collection: &Snapshot,
    progress: &mut Progress,
) -> Result<Vec<(String, i64)>, String> {
    let (db, _) = read::open(root, &DeviceConfig::default())?;
    let wanted: HashSet<u32> = ids.iter().copied().collect();
    let items: Vec<Item> = db
        .tracks
        .into_iter()
        .filter(|t| wanted.contains(&t.id))
        .collect();
    let (found, _) = matched(root, &items, collection, progress)?;
    Ok(found
        .into_iter()
        .map(|(key, _, dev)| (key, dev.sig()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baselines are stored, so the hash of a given text must never change.
    #[test]
    fn signature_is_fnv1a_of_the_canonical_text() {
        let dev = Dev {
            rating: 3,
            comment: "a|b".into(),
            color: 2,
            anlz: None,
        };
        let want = "3|a|b|2".bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        assert_eq!(dev.sig(), want as i64);
    }
}
