//! Cue sync: a track that is in the collection and on a device (same [`store::key`]: same audio file name and size, so the
//! same music) has two copies of its cues, hot cues, loops, beat grid and phrases ([`Marks`]). [`settle`] decides, part by part,
//! which way each one goes:
//! - with a winner (the copy just loaded on a deck), every part the winner has goes to the other copy;
//! - without one (a device just plugged in), only a copy that lacks a part gets it, and a part both copies have but that differs
//!   is a [`Conflict`]: nothing moves until the user chooses a side.
//!
//! An empty part never wins: it is filled from the other copy instead, so nothing is wiped. The collection copy is our store's
//! edits over the collection ([`crate::store::Overlay`]); a pull writes the store ([`pull`]). The device copy is what its plugin
//! reads; a push is an export of just those tracks ([`crate::library::snapshot::Snapshot::of_tracks`]), done by the caller.

use super::undo;
use crate::analysis::{Analysis, Beat, Cue, Phrases};
use crate::library::{AnalysisReader, Library, Track};
use crate::store::{self, Store};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// The parts of a track's analysis that follow it from copy to copy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Marks {
    pub cues: Vec<Cue>,
    pub beats: Vec<Beat>,
    /// `None` or an empty list: no phrases.
    pub phrases: Option<Phrases>,
}

impl From<&Analysis> for Marks {
    fn from(a: &Analysis) -> Self {
        Self {
            cues: a.cues.clone(),
            beats: a.beats.clone(),
            phrases: a.phrases.clone().filter(|p| !p.list.is_empty()),
        }
    }
}

/// Which copy wins where both have a part and they differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Winner {
    Device,
    Collection,
    /// Neither: only empty parts are filled.
    Neither,
}

/// Some parts of a track's [`Marks`]; `None`: that part does not move.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parts {
    pub cues: Option<Vec<Cue>>,
    pub beats: Option<Vec<Beat>>,
    pub phrases: Option<Phrases>,
}

impl Parts {
    pub fn is_empty(&self) -> bool {
        self.cues.is_none() && self.beats.is_none() && self.phrases.is_none()
    }
}

/// Which parts both copies have and disagree on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Differ {
    pub cues: bool,
    pub beats: bool,
    pub phrases: bool,
}

impl Differ {
    pub fn any(&self) -> bool {
        self.cues || self.beats || self.phrases
    }
}

/// What [`settle`] decided.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Moves {
    /// What the collection copy takes from the device copy ([`pull`]).
    pub to_collection: Parts,
    /// What the device copy takes from the collection copy (an export of the track).
    pub to_device: Parts,
    /// What the collection copy had of the parts it takes (only where it had something else): the way back.
    pub lost_collection: Parts,
    /// What the device copy had of the parts it takes (only where it had something else): the way back.
    pub lost_device: Parts,
    /// Where both copies have the part and disagree, whoever wins.
    pub differ: Differ,
}

/// One track in both places: the collection's row and the device's.
#[derive(Clone, Debug)]
pub struct Pair {
    pub collection: Track,
    pub device: Track,
}

/// The linked tracks of one device by the collection copy's [`store::key`], and the device copies that are found by their content
/// ([`PrintIndex`]) because their file name differs: their own [`store::key`] points at the collection's.
#[derive(Clone, Debug, Default)]
pub struct Links {
    pairs: HashMap<String, Pair>,
    alias: HashMap<String, String>,
}

impl Links {
    /// The pair of `key`: the collection copy's key, or the key of a device copy known under another name.
    pub fn get(&self, key: &str) -> Option<&Pair> {
        self.pairs.get(self.alias.get(key).map_or(key, String::as_str))
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The collection copy's key for `key` (itself when it is one).
    pub fn resolve<'a>(&'a self, key: &'a str) -> &'a str {
        self.alias.get(key).map_or(key, String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Pair)> {
        self.pairs.iter()
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// Times within a millisecond are the same (players store finer units).
fn near(a: u32, b: u32) -> bool {
    a.abs_diff(b) <= 1
}

/// The same cue list, in any order: kind, position and loop end; a colour only counts when both copies carry one, a comment
/// when either has a non-empty one.
pub(super) fn same_cues(a: &[Cue], b: &[Cue]) -> bool {
    let sorted = |v: &[Cue]| {
        let mut v = v.to_vec();
        v.sort_by_key(|c| (c.hot_cue, c.time_ms));
        v
    };
    let text = |c: &Cue| c.comment.clone().filter(|s| !s.trim().is_empty());
    a.len() == b.len()
        && sorted(a).iter().zip(&sorted(b)).all(|(x, y)| {
            x.hot_cue == y.hot_cue
                && near(x.time_ms, y.time_ms)
                && match (x.loop_end_ms, y.loop_end_ms) {
                    (None, None) => true,
                    (Some(p), Some(q)) => near(p, q),
                    _ => false,
                }
                && (x.color.is_none() || y.color.is_none() || x.color == y.color)
                && text(x) == text(y)
        })
}

pub(super) fn same_beats(a: &[Beat], b: &[Beat]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            near(x.time_ms, y.time_ms) && x.bar_pos == y.bar_pos && x.bpm_x100 == y.bpm_x100
        })
}

/// Where one part goes: `Some(true)` to the device, `Some(false)` to the collection, `None` nowhere.
fn way(device_has: bool, collection_has: bool, same: bool, w: Winner) -> Option<bool> {
    match (device_has, collection_has) {
        _ if same => None,
        (false, false) => None,
        (true, false) => Some(false),
        (false, true) => Some(true),
        (true, true) => match w {
            Winner::Device => Some(false),
            Winner::Collection => Some(true),
            Winner::Neither => None,
        },
    }
}

/// One part's verdict: where it goes, what the loser had, whether the copies disagree.
struct Verdict<T> {
    to_collection: Option<T>,
    to_device: Option<T>,
    lost_collection: Option<T>,
    lost_device: Option<T>,
    differ: bool,
}

fn verdict<T: Clone>(
    device_has: bool,
    collection_has: bool,
    same: bool,
    w: Winner,
    dev: &T,
    col: &T,
) -> Verdict<T> {
    let differ = device_has && collection_has && !same;
    let mut v = Verdict {
        to_collection: None,
        to_device: None,
        lost_collection: None,
        lost_device: None,
        differ,
    };
    match way(device_has, collection_has, same, w) {
        Some(false) => {
            v.to_collection = Some(dev.clone());
            v.lost_collection = differ.then(|| col.clone());
        }
        Some(true) => {
            v.to_device = Some(col.clone());
            v.lost_device = differ.then(|| dev.clone());
        }
        None => {}
    }
    v
}

/// Makes the two copies of a track agree, part by part (see the module docs).
pub fn settle(device: &Marks, collection: &Marks, w: Winner) -> Moves {
    let cues = verdict(
        !device.cues.is_empty(),
        !collection.cues.is_empty(),
        same_cues(&device.cues, &collection.cues),
        w,
        &device.cues,
        &collection.cues,
    );
    let beats = verdict(
        !device.beats.is_empty(),
        !collection.beats.is_empty(),
        same_beats(&device.beats, &collection.beats),
        w,
        &device.beats,
        &collection.beats,
    );
    let phrases = verdict(
        device.phrases.is_some(),
        collection.phrases.is_some(),
        device.phrases == collection.phrases,
        w,
        &device.phrases,
        &collection.phrases,
    );
    Moves {
        to_collection: Parts {
            cues: cues.to_collection,
            beats: beats.to_collection,
            phrases: phrases.to_collection.flatten(),
        },
        to_device: Parts {
            cues: cues.to_device,
            beats: beats.to_device,
            phrases: phrases.to_device.flatten(),
        },
        lost_collection: Parts {
            cues: cues.lost_collection,
            beats: beats.lost_collection,
            phrases: phrases.lost_collection.flatten(),
        },
        lost_device: Parts {
            cues: cues.lost_device,
            beats: beats.lost_device,
            phrases: phrases.lost_device.flatten(),
        },
        differ: Differ {
            cues: cues.differ,
            beats: beats.differ,
            phrases: phrases.differ,
        },
    }
}

/// Writes `parts` of the track `key` into the store (the collection copy, and what we show for the device copy too); `before`
/// is the collection's grid before (what RESET goes back to). A track under an Analysis Lock keeps its grid. A sync, not an
/// undo step.
pub fn pull(store: &Store, key: &str, parts: &Parts, before: &[Beat]) -> Result<(), String> {
    store.system(|| {
        if let Some(c) = &parts.cues {
            store.set_cues(key, Some(c))?;
        }
        if let Some(b) = &parts.beats
            && !store.analysis_locked(key)?
        {
            store.set_grid(key, b, before)?;
        }
        if let Some(p) = &parts.phrases {
            store.set_phrase_edit(key, p)?;
        }
        Ok(())
    })
}

/// [`pull`] that also records, in `entry`, what the store held for the fields it writes: the way back ([`undo::run`]).
pub fn pull_recorded(
    store: &Store,
    key: &str,
    parts: &Parts,
    before: &[Beat],
    entry: &mut undo::Entry,
) -> Result<(), String> {
    let grid = parts.beats.clone().filter(|_| !store.analysis_locked(key).unwrap_or(false));
    let fields = [
        parts.cues.clone().map(|c| undo::Field::Cues(Some(c))),
        grid.map(|b| undo::Field::Grid(Some(b))),
        parts.phrases.clone().map(|p| undo::Field::Phrases(Some(p))),
    ];
    for f in fields.into_iter().flatten() {
        entry.changes.extend(undo::Change::of(store, key, f)?);
    }
    pull(store, key, parts, before)
}

/// The device copy's marks that a push replaced (`lost`), as the entry that writes them back; `None` when it lost nothing.
pub fn restore_of(device: &str, key: &str, lost: &Parts) -> Option<undo::Restore> {
    (!lost.is_empty()).then(|| undo::Restore {
        device: device.to_owned(),
        key: key.to_owned(),
        marks: Marks {
            cues: lost.cues.clone().unwrap_or_default(),
            beats: lost.beats.clone().unwrap_or_default(),
            phrases: lost.phrases.clone(),
        },
    })
}

impl Marks {
    /// Puts the parts this has (an empty part is left alone) on `a`.
    pub fn put_on(&self, a: &mut Analysis) {
        if !self.cues.is_empty() {
            a.cues.clone_from(&self.cues);
        }
        if !self.beats.is_empty() {
            a.beats.clone_from(&self.beats);
        }
        if self.phrases.is_some() {
            a.phrases.clone_from(&self.phrases);
        }
    }
}

/// The collection's content prints ([`crate::library::print`]) by file size: finds the collection track that a device file is a
/// copy of when the names differ (a stick's file name is cleaned or shortened, a file was renamed by hand). A file is only read
/// when some collection file has its size.
pub struct PrintIndex {
    by_size: HashMap<i64, Vec<(i64, String)>>,
}

impl PrintIndex {
    pub fn new(store: &Store) -> Result<Self, String> {
        Ok(Self {
            by_size: store.prints_by_size()?,
        })
    }

    /// The Collection row id whose file has the bytes of `file`.
    pub fn find(&self, file: &Path) -> Option<i64> {
        let size = i64::try_from(std::fs::metadata(file).ok()?.len()).ok()?;
        let candidates = self.by_size.get(&size)?;
        let print = crate::library::print::of_file(file).ok()?;
        candidates
            .iter()
            .find(|(_, p)| *p == print)
            .map(|(id, _)| *id)
    }
}

/// The tracks of `collection` that the device library `device` (mounted at `mount`) also holds: by [`store::key`] (same file
/// name and size), else by the content of the files ([`PrintIndex`]). Collection tracks that are that device's own files are left
/// out: they are the device copy.
pub fn links(
    device: &dyn Library,
    mount: &Path,
    collection: Vec<Track>,
    store: &Store,
) -> Result<Links, String> {
    let mut on_device: HashMap<String, Track> = device
        .tracks(None)?
        .into_iter()
        .filter(|t| t.file.is_some())
        .map(|t| (store::key(&t), t))
        .collect();
    let mut out = Links::default();
    let mut unpaired = HashMap::new();
    for t in collection
        .into_iter()
        .filter(|t| t.file.as_deref().is_some_and(|f| !f.starts_with(mount)))
    {
        let key = store::key(&t);
        match on_device.remove(&key) {
            Some(device) => {
                out.pairs.insert(
                    key,
                    Pair {
                        collection: t,
                        device,
                    },
                );
            }
            None => {
                if let Some(id) = t.id {
                    unpaired.insert(id, t);
                }
            }
        }
    }
    if unpaired.is_empty() || on_device.is_empty() {
        return Ok(out);
    }
    let index = PrintIndex::new(store)?;
    for (name, d) in on_device {
        let Some(c) = d
            .file
            .as_deref()
            .and_then(|f| index.find(f))
            .and_then(|id| unpaired.remove(&id))
        else {
            continue;
        };
        let key = store::key(&c);
        out.alias.insert(name, key.clone());
        out.pairs.insert(
            key,
            Pair {
                collection: c,
                device: d,
            },
        );
    }
    Ok(out)
}

/// The collection copy's analysis on a worker thread: `reader` (the collection's own) with the store's edits on top.
pub fn collection_analysis(reader: &AnalysisReader, store: &Store, t: &Track) -> Analysis {
    let mut a = reader(t).unwrap_or_default();
    if let Ok(e) = store.edits(&store::key(t)) {
        store::apply(&mut a, &e);
    }
    a
}

/// A track whose two copies both have a part and disagree: nothing moved, the user chooses ([`settle`] with a winner).
#[derive(Clone, Debug)]
pub struct Conflict {
    /// The collection copy's [`store::key`].
    pub key: String,
    pub title: String,
    pub artist: String,
    pub device: Marks,
    pub collection: Marks,
    pub differ: Differ,
}

/// What [`fill`] did and found.
#[derive(Default)]
pub struct Filled {
    /// Tracks whose collection copy took something (written to the store already).
    pub pulled: usize,
    /// The tracks whose device copy must be written, with the collection copy's analysis, for one export.
    pub push: Vec<(Track, Option<Analysis>)>,
    /// The tracks left alone because both copies have different marks.
    pub conflicts: Vec<Conflict>,
}

/// A device just plugged in: fills the copy that lacks a part, for every linked track, and lists the tracks whose copies differ.
/// What the collection takes is written to `store` now; the tracks whose device copy must be written are returned with the
/// collection copy's analysis, for one export.
pub fn fill(
    links: &Links,
    device: &dyn Library,
    reader: &AnalysisReader,
    store: &Store,
) -> Result<Filled, String> {
    let mut out = Filled::default();
    for (key, p) in links.iter() {
        let col = collection_analysis(reader, store, &p.collection);
        let dev = device.analysis(&p.device).unwrap_or_default();
        let (dm, cm) = (Marks::from(&dev), Marks::from(&col));
        let m = settle(&dm, &cm, Winner::Neither);
        if !m.to_collection.is_empty() {
            pull(store, key, &m.to_collection, &col.beats)?;
            out.pulled += 1;
        }
        if m.differ.any() {
            // An export writes the whole collection copy: it would overwrite the device's different part. The user decides first.
            out.conflicts.push(Conflict {
                key: key.clone(),
                title: p.collection.title.clone(),
                artist: p.collection.artist.clone(),
                device: dm,
                collection: cm,
                differ: m.differ,
            });
        } else if !m.to_device.is_empty() {
            out.push.push((p.collection.clone(), Some(col)));
        }
    }
    out.conflicts.sort_by(|a, b| (&a.artist, &a.title).cmp(&(&b.artist, &b.title)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Mood, Phrase, PhraseKind};

    fn cue(hot: u16, ms: u32) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    fn beats(start: u32) -> Vec<Beat> {
        (0..4)
            .map(|i| Beat {
                bar_pos: i as u16 + 1,
                bpm_x100: 12000,
                time_ms: start + i * 500,
            })
            .collect()
    }

    fn phrases() -> Phrases {
        Phrases {
            mood: Mood::Mid,
            list: vec![Phrase {
                beat: 1,
                end_beat: 33,
                kind: PhraseKind::Intro,
                variant: 0,
                fill: None,
            }],
        }
    }

    fn marks(cues: Vec<Cue>, b: Vec<Beat>, p: Option<Phrases>) -> Marks {
        Marks {
            cues,
            beats: b,
            phrases: p,
        }
    }

    #[test]
    fn the_loaded_copy_wins_but_an_empty_part_is_filled_instead() {
        let dev = marks(vec![cue(1, 1000)], beats(0), None);
        let col = marks(vec![cue(1, 2000)], Vec::new(), Some(phrases()));
        // Loaded from the device: its cues and grid go to the collection, the collection's phrases go to the device.
        let m = settle(&dev, &col, Winner::Device);
        assert_eq!(
            m.to_collection,
            Parts {
                cues: Some(vec![cue(1, 1000)]),
                beats: Some(beats(0)),
                phrases: None
            }
        );
        assert_eq!(
            m.to_device,
            Parts {
                phrases: Some(phrases()),
                ..Parts::default()
            }
        );
        // Loaded from the collection: its cues and phrases go to the device; its empty grid takes the device's.
        let m = settle(&dev, &col, Winner::Collection);
        assert_eq!(
            m.to_collection,
            Parts {
                beats: Some(beats(0)),
                ..Parts::default()
            }
        );
        assert_eq!(
            m.to_device,
            Parts {
                cues: Some(vec![cue(1, 2000)]),
                phrases: Some(phrases()),
                beats: None
            }
        );
    }

    #[test]
    fn plugging_in_only_fills_and_agreeing_copies_do_nothing() {
        let dev = marks(vec![cue(1, 1000)], beats(0), None);
        let col = marks(vec![cue(2, 5000)], beats(0), Some(phrases()));
        let m = settle(&dev, &col, Winner::Neither);
        assert!(m.to_collection.is_empty());
        assert_eq!(
            m.to_device,
            Parts {
                phrases: Some(phrases()),
                ..Parts::default()
            }
        );
        // Order, a missing colour, an empty comment and a millisecond of rounding are not differences.
        let mut a = cue(1, 1000);
        a.color = Some([255, 0, 0]);
        a.comment = Some(" ".into());
        let both = settle(
            &marks(vec![cue(0, 300), a], beats(1), None),
            &marks(vec![cue(1, 1001), cue(0, 300)], beats(0), None),
            Winner::Device,
        );
        assert_eq!(both, Moves::default());
        // A moved hot cue is a difference.
        let moved = settle(
            &marks(vec![cue(1, 1000)], Vec::new(), None),
            &marks(vec![cue(1, 1500)], Vec::new(), None),
            Winner::Device,
        );
        assert_eq!(moved.to_collection.cues, Some(vec![cue(1, 1000)]));
        assert_eq!(
            settle(&Marks::default(), &Marks::default(), Winner::Collection),
            Moves::default()
        );
    }

    #[test]
    fn a_pull_writes_the_store_and_spares_a_locked_grid() {
        let s = Store::in_memory().unwrap();
        let p = Parts {
            cues: Some(vec![cue(3, 42)]),
            beats: Some(beats(7)),
            phrases: Some(phrases()),
        };
        pull(&s, "a|1", &p, &[]).unwrap();
        let e = s.edits("a|1").unwrap();
        assert_eq!(
            (e.cues, e.grid, e.phrases),
            (Some(vec![cue(3, 42)]), Some(beats(7)), Some(phrases()))
        );
        s.set_analysis_lock("b|2", true).unwrap();
        pull(&s, "b|2", &p, &[]).unwrap();
        assert_eq!(s.edits("b|2").unwrap().grid, None);
    }

    #[test]
    fn copies_that_both_have_and_that_differ_are_conflicts_and_a_winners_loser_is_recorded() {
        let dev = marks(vec![cue(1, 1000)], beats(0), Some(phrases()));
        let col = marks(vec![cue(1, 2000)], beats(0), None);
        // Both have cues and they differ: a conflict, nothing moves for it. The empty phrases are filled as always.
        let m = settle(&dev, &col, Winner::Neither);
        assert_eq!(m.differ, Differ { cues: true, beats: false, phrases: false });
        assert_eq!(m.to_collection.cues, None);
        assert_eq!(m.to_collection.phrases, Some(phrases()));
        assert!(m.lost_collection.is_empty() && m.lost_device.is_empty());
        // One side empty is a fill, never a conflict; equal copies agree.
        assert!(!settle(&marks(vec![], vec![], None), &col, Winner::Neither).differ.any());
        assert!(!settle(&col, &col, Winner::Neither).differ.any());
        // With a winner what the loser had is kept for the way back (only where it had something else).
        let m = settle(&dev, &col, Winner::Device);
        assert_eq!(m.lost_collection.cues, Some(vec![cue(1, 2000)]));
        assert!(m.lost_collection.phrases.is_none() && m.lost_device.is_empty());
        let m = settle(&dev, &col, Winner::Collection);
        assert_eq!(m.lost_device.cues, Some(vec![cue(1, 1000)]));
        let back = restore_of("d", "k", &m.lost_device).unwrap();
        assert_eq!((back.marks.cues.clone(), back.marks.beats.len()), (vec![cue(1, 1000)], 0));
        assert!(restore_of("d", "k", &Parts::default()).is_none());
        let mut a = Analysis { cues: vec![cue(9, 9)], ..Default::default() };
        back.marks.put_on(&mut a);
        assert_eq!(a.cues, vec![cue(1, 1000)]);
    }

    #[test]
    fn a_recorded_pull_can_be_taken_back() {
        let s = Store::in_memory().unwrap();
        s.set_cues("k", Some(&[cue(1, 2000)])).unwrap();
        let p = Parts { cues: Some(vec![cue(1, 1000)]), beats: Some(beats(0)), phrases: None };
        let mut entry = undo::Entry::default();
        pull_recorded(&s, "k", &p, &[], &mut entry).unwrap();
        assert_eq!(s.edits("k").unwrap().cues, Some(vec![cue(1, 1000)]));
        let id = undo::keep(&s, "sync", "d", &entry, 1).unwrap().unwrap();
        undo::run(&s, id).unwrap();
        let e = s.edits("k").unwrap();
        assert_eq!((e.cues, e.grid), (Some(vec![cue(1, 2000)]), None));
    }

    #[test]
    fn a_device_file_under_another_name_is_found_by_its_bytes_and_a_file_of_another_size_is_not_read() {
        let dir = std::env::temp_dir().join(format!("openfader-link-print-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mine, theirs, other) = (dir.join("My Song.mp3"), dir.join("MY_SONG.mp3"), dir.join("x.mp3"));
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&mine, &bytes).unwrap();
        std::fs::write(&theirs, &bytes).unwrap();
        std::fs::write(&other, &bytes[..1000]).unwrap();
        let s = Store::in_memory().unwrap();
        let id = s.collection_upsert(&Track { file: Some(mine.clone()), ..Track::default() }, None).unwrap();
        let (size, mtime) = crate::library::print::stamp(&mine).unwrap();
        s.set_file_print(id, &crate::library::print::of_file(&mine).unwrap(), size, mtime).unwrap();
        let index = PrintIndex::new(&s).unwrap();
        assert_eq!(index.find(&theirs), Some(id));
        assert_eq!(index.find(&other), None);
        std::fs::write(&theirs, [&bytes[..150_000], &[0u8][..], &bytes[150_001..]].concat()).unwrap();
        assert_eq!(index.find(&theirs), None, "a byte in a sampled slice differs");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
