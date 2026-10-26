//! The store laid over a [`Library`]: tracks carry the store's text and rating overrides, and a track's
//! analysis carries the store's cue list and beat grid. The store wins; what it has no edit for stays
//! the library's. Playlists are the library's own (user playlists are read from the store by the browser).

use super::{Edits, R, Store, global, key, track::Fields};
use crate::{
    analysis::Analysis,
    library::{Library, Playlist, Track},
};
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};

/// `inner` seen through the process-wide [`global`] store's edits.
pub fn overlay(inner: Box<dyn Library>) -> Box<dyn Library> {
    Box::new(Overlay::new(inner, global()))
}

/// Text / rating edits by track: a Collection track's by its id (no key string built per track on a 100k listing), the
/// rest by [`key`].
#[derive(Default, PartialEq)]
struct Edited {
    by_id: HashMap<i64, Fields>,
    by_key: HashMap<String, Fields>,
}

impl Edited {
    fn of(fields: HashMap<String, Fields>) -> Self {
        let mut e = Edited::default();
        for (k, f) in fields {
            match id_of_key(&k) {
                Some(id) => e.by_id.insert(id, f).map(drop),
                None => e.by_key.insert(k, f).map(drop),
            };
        }
        e
    }

    fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_key.is_empty()
    }

    fn get(&self, t: &Track) -> Option<&Fields> {
        if let Some(id) = t.id {
            return self.by_id.get(&id);
        }
        let k = key(t);
        match id_of_key(&k) {
            Some(id) => self.by_id.get(&id),
            None => self.by_key.get(&k),
        }
    }
}

impl Edited {
    /// Puts the edited text and rating on the tracks they belong to.
    fn apply(&self, tracks: &mut [Track]) {
        for t in tracks {
            let Some(f) = self.get(t) else { continue };
            if let Some(r) = f.rating {
                t.rating = r;
            }
            for (field, edit) in [
                (&mut t.title, &f.title),
                (&mut t.artist, &f.artist),
                (&mut t.album, &f.album),
                (&mut t.genre, &f.genre),
                (&mut t.label, &f.label),
                (&mut t.comment, &f.comment),
            ] {
                if let Some(value) = edit {
                    field.clone_from(value);
                }
            }
        }
    }
}

/// `tracks` with the store's text and rating edits on, as [`Overlay`] shows a library's list: for a list the store holds itself
/// (the stages).
pub(super) fn edited(store: &Store, tracks: &mut [Track]) {
    Edited::of(store.track_fields().unwrap_or_default()).apply(tracks);
}

/// The library's list, the edits it was merged with and the result.
type Merged = (Arc<Vec<Track>>, Arc<Edited>, Arc<Vec<Track>>);

/// What [`Store::collection_edited`] keeps: the list, the edits on it, and the [`Store::generation`] and
/// [`Store::collection_generation`] they were last found current at.
pub(super) struct EditedList {
    generation: u64,
    rows: u64,
    edits: Edited,
    tracks: Arc<Vec<Track>>,
}

impl Store {
    /// The Collection ([`Store::collection_tracks`]) with the text and rating edits on, the edits put on the rows as they are
    /// read (no copy of the list): what [`crate::library::own::Own`] lists. Kept while no Collection row changes and the edits
    /// stay the same (a write elsewhere, a play or a setting, costs one read of the edits, not of the list).
    pub fn collection_edited(&self) -> R<Arc<Vec<Track>>> {
        let mut kept = self.edited.lock();
        // Read before the data: a write in between makes the next call look again.
        let (generation, rows) = (self.generation(), self.collection_generation());
        if let Some(k) = kept.as_mut().filter(|k| k.rows == rows) {
            if k.generation == generation {
                return Ok(k.tracks.clone());
            }
            let edits = Edited::of(self.track_fields()?);
            if edits == k.edits {
                k.generation = generation;
                return Ok(k.tracks.clone());
            }
        }
        let edits = Edited::of(self.track_fields()?);
        let mut tracks = self.collection_rows()?;
        edits.apply(&mut tracks);
        let tracks = Arc::new(tracks);
        *kept = Some(EditedList {
            generation,
            rows,
            edits,
            tracks: tracks.clone(),
        });
        Ok(tracks)
    }
}

/// `42` of the key `t42` ([`super::id_key`]).
fn id_of_key(k: &str) -> Option<i64> {
    k.strip_prefix('t')?.parse().ok()
}

pub struct Overlay {
    inner: Box<dyn Library>,
    store: Arc<Store>,
    /// The store's text / rating edits as of the [`Store::generation`] in `.0`.
    fields: Mutex<Option<(u64, Arc<Edited>)>>,
    /// [`Library::tracks_all`] of `inner` with those edits on: valid while both `Arc`s are the ones it was made from.
    merged: Mutex<Option<Merged>>,
}

impl Overlay {
    pub fn new(inner: Box<dyn Library>, store: Arc<Store>) -> Self {
        Self {
            inner,
            store,
            fields: Mutex::new(None),
            merged: Mutex::new(None),
        }
    }

    /// The store's text / rating edits, read again only when the store changed since the last call.
    fn fields(&self) -> Arc<Edited> {
        let generation = self.store.generation();
        let mut c = self.fields.lock();
        if let Some((g, f)) = &*c
            && *g == generation
        {
            return f.clone();
        }
        let f = Arc::new(Edited::of(self.store.track_fields().unwrap_or_else(|e| {
            eprintln!("store: {e}");
            Default::default()
        })));
        *c = Some((generation, f.clone()));
        f
    }
}

/// Puts the stored cue list, grid and phrases onto `a`; an unedited part is left alone. A stored phrase list replaces the analysed
/// one even when empty (CLEAR): `Some` with no phrases, which [`crate::analysis::auto::fill`] leaves alone. A stored grid with
/// no stored phrases makes the analysed phrases stale (they are numbered by the old grid): ours are derived from the edited grid
/// instead, `None` while `a` has no 3-band detail ([`crate::analysis::auto::fill`] derives them once it has).
pub fn apply(a: &mut Analysis, e: &Edits) {
    if let Some(c) = &e.cues {
        a.cues.clone_from(c);
    }
    if let Some(g) = &e.grid {
        a.beats.clone_from(g);
        if e.phrases.is_none() {
            a.phrases = None;
            crate::analysis::auto::fill(a);
        }
    }
    if let Some(p) = &e.phrases {
        a.phrases = Some(p.clone());
    }
}

/// `own`, the analysis a library gave for `track`, with the store's cues / grid / phrases on top ([`apply`]); `own` itself, unchanged,
/// for a track the store has no such edit for. [`Overlay::analysis`], and what a worker does with a [`crate::library::AnalysisReader`]'s
/// answer to get the same.
pub fn with_edits(own: Option<Analysis>, track: &Track, store: &Store) -> Option<Analysis> {
    let edits = store.edits(&key(track)).unwrap_or_else(|e| {
        eprintln!("store: {e}");
        Edits::default()
    });
    if edits.cues.is_none() && edits.grid.is_none() && edits.phrases.is_none() {
        return own;
    }
    let mut a = own.unwrap_or_default();
    apply(&mut a, &edits);
    Some(a)
}

impl Library for Overlay {
    fn kind(&self) -> &str {
        self.inner.kind()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        self.inner.playlists()
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        if self.inner.edited() {
            return self.inner.tracks(playlist);
        }
        let mut tracks = self.inner.tracks(playlist)?;
        let fields = self.fields();
        if !fields.is_empty() {
            fields.apply(&mut tracks);
        }
        Ok(tracks)
    }

    /// The library's list with the edits on, copied once per change of the library or the store, not per call.
    fn tracks_all(&self) -> Result<Arc<Vec<Track>>, String> {
        if self.inner.edited() {
            return self.inner.tracks_all();
        }
        let (inner, fields) = (self.inner.tracks_all()?, self.fields());
        if fields.is_empty() {
            return Ok(inner);
        }
        let mut merged = self.merged.lock();
        if let Some((i, f, m)) = &*merged
            && Arc::ptr_eq(i, &inner)
            && Arc::ptr_eq(f, &fields)
        {
            return Ok(m.clone());
        }
        let mut tracks = (*inner).clone();
        fields.apply(&mut tracks);
        let all = Arc::new(tracks);
        *merged = Some((inner, fields, all.clone()));
        Ok(all)
    }

    /// The library's analysis with the store's cues / grid / phrases on top; the library's own answer, unchanged, for a
    /// track the store has no such edit for.
    fn analysis(&self, track: &Track) -> Option<Analysis> {
        with_edits(self.inner.analysis(track), track, &self.store)
    }

    fn analysis_reader(&self) -> Option<crate::library::AnalysisReader> {
        self.inner.analysis_reader()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Beat, Phrase, PhraseKind, Phrases};

    /// 64 bars at 120 BPM whose downbeat is beat `first` (1-based); a waveform with a quiet half and a loud half.
    fn track(first: usize) -> Analysis {
        let beats = (0..256)
            .map(|i| Beat {
                bar_pos: ((i + 5 - first) % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: 500 * i as u32,
            })
            .collect();
        let detail = (0..64usize)
            .flat_map(|bar| {
                vec![
                    if bar < 32 {
                        [20, 30, 20]
                    } else {
                        [90, 100, 80]
                    };
                    300
                ]
            })
            .collect();
        Analysis {
            beats,
            three_band_detail: detail,
            ..Default::default()
        }
    }

    fn stale() -> Phrases {
        Phrases {
            mood: crate::analysis::Mood::Mid,
            list: vec![Phrase {
                beat: 2,
                end_beat: 6,
                kind: PhraseKind::Intro,
                variant: 0,
                fill: None,
            }],
        }
    }

    #[test]
    fn grid_edit_rederives_the_stale_phrases_on_the_edited_bars() {
        let edited = track(3);
        let mut a = track(1);
        a.three_band_detail.clone_from(&edited.three_band_detail);
        a.phrases = Some(stale());
        apply(
            &mut a,
            &Edits {
                grid: Some(edited.beats.clone()),
                ..Default::default()
            },
        );
        let p = a.phrases.expect("phrases from the edited grid");
        assert!(p.list.len() > 1 && p.list != stale().list);
        for ph in p.list.iter().skip(1) {
            assert_eq!(edited.beats[ph.beat as usize - 1].bar_pos, 1, "{ph:?}");
        }
    }

    #[test]
    fn grid_edit_without_waveform_leaves_phrases_for_fill_and_a_phrase_edit_wins() {
        let edited = track(3);
        let mut a = Analysis {
            phrases: Some(stale()),
            ..Default::default()
        };
        apply(
            &mut a,
            &Edits {
                grid: Some(edited.beats.clone()),
                ..Default::default()
            },
        );
        assert!(a.phrases.is_none());
        let mut b = track(1);
        b.phrases = Some(stale());
        let mine = Phrases {
            mood: crate::analysis::Mood::Low,
            list: vec![],
        };
        apply(
            &mut b,
            &Edits {
                grid: Some(edited.beats),
                phrases: Some(mine.clone()),
                ..Default::default()
            },
        );
        assert_eq!(b.phrases, Some(mine));
    }

    #[test]
    fn reload_tag_keeps_edits_over_the_files_text_and_discard_drops_them_until_undo() {
        let s = Arc::new(Store::in_memory().unwrap());
        let t = crate::library::Track {
            title: "Old".into(),
            file: Some("/m/a.mp3".into()),
            ..Default::default()
        };
        s.collection_put(&[t.clone()], false).unwrap();
        let id = s.id_of_path(t.file.as_deref().unwrap()).unwrap().unwrap();
        let key = crate::store::id_key(id);
        s.set_rating(&key, Some(4)).unwrap();
        s.set_metadata(&key, "Mine", "Me", "", "", "", "note").unwrap();
        let ov = Overlay::new(Box::new(crate::library::own::Own::new(s.clone())), s.clone());
        let shown = || {
            let t = ov.tracks_all().unwrap()[0].clone();
            (t.title, t.artist, t.album, t.comment, t.rating)
        };
        let edited = ("Mine".to_string(), "Me".into(), "".into(), "note".into(), 4);
        assert_eq!(shown(), edited);
        let fresh = crate::library::Track {
            title: "From File".into(),
            artist: "File Artist".into(),
            album: "File Album".into(),
            ..s.collection_track(id).unwrap().unwrap()
        };
        s.action("Reload Tag", || s.reload_tags(&key, &fresh, true)).unwrap();
        assert_eq!(shown(), edited);
        s.action("Reload Tag (discard edits)", || s.reload_tags(&key, &fresh, false)).unwrap();
        assert_eq!(shown(), ("From File".into(), "File Artist".into(), "File Album".into(), "".into(), 4));
        s.undo().unwrap();
        assert_eq!(shown(), edited);
    }

    #[test]
    fn the_edited_collection_is_kept_across_other_writes_and_read_again_after_an_edit() {
        let s = Store::in_memory().unwrap();
        let t = |n: &str| crate::library::Track {
            title: n.into(),
            file: Some(format!("/m/{n}.mp3").into()),
            ..Default::default()
        };
        s.collection_put(&[t("a"), t("b")], false).unwrap();
        let first = s.collection_edited().unwrap();
        let key = crate::store::id_key(first[1].id.unwrap());
        s.history_append("2026-10-25", &key, 1).unwrap(); // not an edit: the same list
        assert!(Arc::ptr_eq(&first, &s.collection_edited().unwrap()));
        s.set_rating(&key, Some(5)).unwrap();
        let rated = s.collection_edited().unwrap();
        assert_eq!((rated[0].rating, rated[1].rating), (0, 5));
        s.collection_put(&[t("c")], false).unwrap(); // a new row: read again, the edit still on
        let more = s.collection_edited().unwrap();
        assert_eq!((more.len(), more[1].rating), (3, 5));
        assert_eq!(*more, *Overlay::new(Box::new(Raw(s.collection_tracks().unwrap())), Arc::new(s)).tracks_all().unwrap());
    }

    /// The raw Collection as a plain library: what the generic overlay merges the edits onto.
    struct Raw(Arc<Vec<Track>>);

    impl Library for Raw {
        fn kind(&self) -> &str {
            "raw"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            Ok(Vec::new())
        }
        fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
            Ok(self.0.to_vec())
        }
    }
}
