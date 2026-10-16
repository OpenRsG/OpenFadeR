//! Cue sync: a track that is in the collection and on a device (same [`store::key`]: same audio file name and size, so the
//! same music) has two copies of its cues, hot cues, loops, beat grid and phrases ([`Marks`]). [`settle`] decides, part by part,
//! which way each one goes:
//! - with a winner (the copy just loaded on a deck), every part the winner has goes to the other copy;
//! - without one (a device just plugged in), only a copy that lacks a part gets it.
//!
//! An empty part never wins: it is filled from the other copy instead, so nothing is wiped. The collection copy is our store's
//! edits over the collection ([`crate::store::Overlay`]); a pull writes the store ([`pull`]). The device copy is what its plugin
//! reads; a push is an export of just those tracks ([`crate::library::snapshot::Snapshot::of_tracks`]), done by the caller.

use crate::analysis::{Analysis, Beat, Cue, Phrases};
use crate::library::{AnalysisReader, Library, Track};
use crate::store::{self, Store};
use std::collections::HashMap;
use std::path::Path;

/// The parts of a track's analysis that follow it from copy to copy.
#[derive(Clone, Debug, Default, PartialEq)]
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

/// What [`settle`] decided.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Moves {
    /// What the collection copy takes from the device copy ([`pull`]).
    pub to_collection: Parts,
    /// What the device copy takes from the collection copy (an export of the track).
    pub to_device: Parts,
}

/// One track in both places: the collection's row and the device's.
#[derive(Clone, Debug)]
pub struct Pair {
    pub collection: Track,
    pub device: Track,
}

/// The linked tracks of one device, by [`store::key`].
pub type Links = HashMap<String, Pair>;

/// Times within a millisecond are the same (players store finer units).
fn near(a: u32, b: u32) -> bool {
    a.abs_diff(b) <= 1
}

/// The same cue list, in any order: kind, position and loop end; a colour only counts when both copies carry one, a comment
/// when either has a non-empty one.
fn same_cues(a: &[Cue], b: &[Cue]) -> bool {
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

fn same_beats(a: &[Beat], b: &[Beat]) -> bool {
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

/// Moves one part: `dev` / `col` are the device's and the collection's, `to_col` / `to_dev` where it lands.
fn part<T: Clone>(
    way: Option<bool>,
    dev: &T,
    col: &T,
    to_col: &mut Option<T>,
    to_dev: &mut Option<T>,
) {
    match way {
        Some(false) => *to_col = Some(dev.clone()),
        Some(true) => *to_dev = Some(col.clone()),
        None => {}
    }
}

/// Makes the two copies of a track agree, part by part (see the module docs).
pub fn settle(device: &Marks, collection: &Marks, w: Winner) -> Moves {
    let mut m = Moves::default();
    let (c, d) = (&mut m.to_collection, &mut m.to_device);
    part(
        way(
            !device.cues.is_empty(),
            !collection.cues.is_empty(),
            same_cues(&device.cues, &collection.cues),
            w,
        ),
        &device.cues,
        &collection.cues,
        &mut c.cues,
        &mut d.cues,
    );
    part(
        way(
            !device.beats.is_empty(),
            !collection.beats.is_empty(),
            same_beats(&device.beats, &collection.beats),
            w,
        ),
        &device.beats,
        &collection.beats,
        &mut c.beats,
        &mut d.beats,
    );
    let ways = way(
        device.phrases.is_some(),
        collection.phrases.is_some(),
        device.phrases == collection.phrases,
        w,
    );
    match ways {
        Some(false) => c.phrases.clone_from(&device.phrases),
        Some(true) => d.phrases.clone_from(&collection.phrases),
        None => {}
    }
    m
}

/// Writes `parts` of the track `key` into the store (the collection copy, and what we show for the device copy too); `before`
/// is the collection's grid before (what RESET goes back to). A track under an Analysis Lock keeps its grid.
pub fn pull(store: &Store, key: &str, parts: &Parts, before: &[Beat]) -> Result<(), String> {
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
}

/// The tracks of `collection` that the device library `device` (mounted at `mount`) also holds, by key. Collection tracks that
/// are that device's own files are left out: they are the device copy.
pub fn links(device: &dyn Library, mount: &Path, collection: Vec<Track>) -> Result<Links, String> {
    let mut on_device: HashMap<String, Track> = device
        .tracks(None)?
        .into_iter()
        .filter(|t| t.file.is_some())
        .map(|t| (store::key(&t), t))
        .collect();
    Ok(collection
        .into_iter()
        .filter(|t| t.file.as_deref().is_some_and(|f| !f.starts_with(mount)))
        .filter_map(|t| {
            let key = store::key(&t);
            let device = on_device.remove(&key)?;
            Some((
                key,
                Pair {
                    collection: t,
                    device,
                },
            ))
        })
        .collect())
}

/// The collection copy's analysis on a worker thread: `reader` (the collection's own) with the store's edits on top.
pub fn collection_analysis(reader: &AnalysisReader, store: &Store, t: &Track) -> Analysis {
    let mut a = reader(t).unwrap_or_default();
    if let Ok(e) = store.edits(&store::key(t)) {
        store::apply(&mut a, &e);
    }
    a
}

/// A device just plugged in: fills the copy that lacks a part, for every linked track. What the collection takes is written to
/// `store` now; the tracks whose device copy must be written are returned with the collection copy's analysis, for one export.
/// Returns how many tracks took something, and those tracks.
pub fn fill(
    links: &Links,
    device: &dyn Library,
    reader: &AnalysisReader,
    store: &Store,
) -> Result<(usize, Vec<(Track, Option<Analysis>)>), String> {
    let (mut pulled, mut push) = (0, Vec::new());
    for (key, p) in links {
        let col = collection_analysis(reader, store, &p.collection);
        let dev = device.analysis(&p.device).unwrap_or_default();
        let m = settle(&Marks::from(&dev), &Marks::from(&col), Winner::Neither);
        if !m.to_collection.is_empty() {
            pull(store, key, &m.to_collection, &col.beats)?;
            pulled += 1;
        }
        if !m.to_device.is_empty() {
            push.push((p.collection.clone(), Some(col)));
        }
    }
    Ok((pulled, push))
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
}
