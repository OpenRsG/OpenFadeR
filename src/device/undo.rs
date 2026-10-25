//! The way back from an import. UPDATE COLLECTION, IMPORT HISTORIES and a cue sync that replaced a copy's marks each keep an
//! [`Entry`] ([`keep`], in the store: [`crate::store::Store::undo_push`], the last five): per track the previous value of exactly the
//! fields the import wrote and the value it wrote, the plays it added, the histories it marked as imported, and the device copies
//! whose marks it overwrote.
//!
//! [`run`] restores a field only while it still holds what the import wrote: a field the user edited since keeps the newer value
//! (counted in [`Undone::kept`]), and fields the import did not touch are never written. The device copies that lost their marks
//! are handed back ([`Undone::restore`]) for the caller to write (an export of that track).

use super::link::Marks;
use crate::analysis::{Beat, Cue, Phrases};
use crate::store::{Edits, Store};
use serde::{Deserialize, Serialize};

/// One field of a track's edits ([`Edits`]) with a value (`None`: no edit, the library's value stands).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Field {
    Rating(Option<u8>),
    Comment(Option<String>),
    Color(Option<u8>),
    Cues(Option<Vec<Cue>>),
    Grid(Option<Vec<Beat>>),
    Phrases(Option<Phrases>),
}

impl Field {
    /// The same field as `e` holds it now.
    pub fn read(&self, e: &Edits) -> Field {
        match self {
            Field::Rating(_) => Field::Rating(e.rating),
            Field::Comment(_) => Field::Comment(e.comment.clone()),
            Field::Color(_) => Field::Color(e.color),
            Field::Cues(_) => Field::Cues(e.cues.clone()),
            Field::Grid(_) => Field::Grid(e.grid.clone()),
            Field::Phrases(_) => Field::Phrases(e.phrases.clone()),
        }
    }

    /// Writes the value to the store as the edit of `key`.
    pub fn write(&self, store: &Store, key: &str) -> Result<(), String> {
        match self {
            Field::Rating(v) => store.set_rating(key, *v),
            Field::Comment(v) => store.set_comment(key, v.as_deref()),
            Field::Color(v) => store.set_color(key, *v),
            Field::Cues(v) => store.set_cues(key, v.as_deref()),
            Field::Grid(Some(g)) => store.set_grid(key, g, &[]),
            Field::Grid(None) => store.reset_grid(key).map(drop),
            Field::Phrases(Some(p)) => store.set_phrase_edit(key, p),
            Field::Phrases(None) => store.clear_phrase_edit(key),
        }
    }
}

/// A field an import wrote: what it was and what the import made it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The track's [`crate::store::key`].
    pub key: String,
    pub before: Field,
    pub after: Field,
}

impl Change {
    /// The change of writing `after` over what `store` holds for `key` now; `None` when it would change nothing.
    pub fn of(store: &Store, key: &str, after: Field) -> Result<Option<Change>, String> {
        let before = after.read(&store.edits(key)?);
        Ok((before != after).then(|| Change {
            key: key.to_owned(),
            before,
            after,
        }))
    }
}

/// A play an import added to the history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Play {
    pub day: String,
    pub key: String,
    pub at: i64,
}

/// A device copy whose marks an import replaced: to be written back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Restore {
    pub device: String,
    /// The collection copy's [`crate::store::key`] (what [`super::link::Links`] is keyed by).
    pub key: String,
    pub marks: Marks,
}

/// Everything one import changed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub changes: Vec<Change>,
    pub plays: Vec<Play>,
    /// `(device id, history identity)` marked as imported.
    pub histories: Vec<(String, String)>,
    pub restore: Vec<Restore>,
}

impl Entry {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.plays.is_empty()
            && self.histories.is_empty()
            && self.restore.is_empty()
    }
}

/// What [`run`] did.
#[derive(Debug, Default, PartialEq)]
pub struct Undone {
    /// Fields put back.
    pub restored: usize,
    /// Fields left as they are because they were edited after the import.
    pub kept: usize,
    /// Plays taken out of the history.
    pub plays: usize,
    /// Device copies to write back.
    pub restore: Vec<Restore>,
}

/// Keeps `entry` as the way back of the import called `label` from `device`; its id, `None` when the import changed nothing.
pub fn keep(
    store: &Store,
    label: &str,
    device: &str,
    entry: &Entry,
    now: i64,
) -> Result<Option<i64>, String> {
    if entry.is_empty() {
        return Ok(None);
    }
    let data = serde_json::to_string(entry).map_err(|e| e.to_string())?;
    store.undo_push(label, device, &data, now).map(Some)
}

/// Takes the import `id` back (see the module docs) and forgets it.
pub fn run(store: &Store, id: i64) -> Result<Undone, String> {
    let e = store
        .undo_entries()?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| "that import is not kept any more".to_owned())?;
    let entry: Entry = serde_json::from_str(&e.data).map_err(|e| e.to_string())?;
    let mut out = Undone::default();
    store.action("Undo import", || -> Result<(), String> {
        for c in entry.changes.iter().rev() {
            let now = c.after.read(&store.edits(&c.key)?);
            if now == c.after {
                c.before.write(store, &c.key)?;
                out.restored += 1;
            } else if now != c.before {
                out.kept += 1;
            }
        }
        Ok(())
    })?;
    let plays: Vec<_> = entry
        .plays
        .iter()
        .map(|p| (p.day.clone(), p.key.clone(), p.at))
        .collect();
    out.plays = store.history_remove(&plays)?;
    for (device, ident) in &entry.histories {
        store.history_unmark_imported(device, ident)?;
    }
    out.restore = entry.restore;
    store.undo_forget(id)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_fields_the_import_wrote_come_back_and_not_later_edits() {
        let s = Store::in_memory().unwrap();
        s.set_rating("a", Some(3)).unwrap();
        s.set_comment("a", Some("mine")).unwrap();
        s.set_rating("b", Some(1)).unwrap();
        let wrote = [
            ("a", Field::Rating(Some(5))),
            ("a", Field::Color(Some(2))),
            ("b", Field::Rating(Some(4))),
        ];
        let mut entry = Entry::default();
        for (k, f) in wrote {
            entry.changes.extend(Change::of(&s, k, f.clone()).unwrap());
            f.write(&s, k).unwrap();
        }
        assert_eq!(
            Change::of(&s, "a", Field::Rating(Some(5))).unwrap(),
            None,
            "no change, no entry"
        );
        let id = keep(&s, "import", "dev", &entry, 1).unwrap().unwrap();
        // After the import the user edits the comment of `a` and the rating of `b`.
        s.set_comment("a", Some("later")).unwrap();
        s.set_rating("b", Some(2)).unwrap();
        let u = run(&s, id).unwrap();
        assert_eq!((u.restored, u.kept), (2, 1));
        let a = s.edits("a").unwrap();
        assert_eq!(
            (a.rating, a.color, a.comment.as_deref()),
            (Some(3), None, Some("later"))
        );
        assert_eq!(
            s.edits("b").unwrap().rating,
            Some(2),
            "edited since: left alone"
        );
        assert!(
            s.undo_entries().unwrap().is_empty(),
            "an undone import is forgotten"
        );
        assert!(run(&s, id).is_err());
    }

    #[test]
    fn plays_and_marks_of_histories_are_taken_back() {
        let s = Store::in_memory().unwrap();
        s.history_append_many("2026-01-01", &[("a".into(), 10), ("b".into(), 11)])
            .unwrap();
        s.mark_history_imported("dev", "h|2|a").unwrap();
        let entry = Entry {
            plays: vec![Play {
                day: "2026-01-01".into(),
                key: "a".into(),
                at: 10,
            }],
            histories: vec![("dev".into(), "h|2|a".into())],
            restore: vec![Restore {
                device: "dev".into(),
                key: "a".into(),
                marks: Marks::default(),
            }],
            ..Entry::default()
        };
        let id = keep(&s, "histories", "dev", &entry, 1).unwrap().unwrap();
        let u = run(&s, id).unwrap();
        assert_eq!((u.plays, u.restore.len()), (1, 1));
        assert_eq!(s.history_day("2026-01-01").unwrap(), ["b"]);
        assert!(!s.history_imported("dev", "h|2|a").unwrap());
        assert_eq!(
            keep(&s, "nothing", "dev", &Entry::default(), 1).unwrap(),
            None
        );
    }
}
