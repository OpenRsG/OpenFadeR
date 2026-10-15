//! Manual phrase edits of a track (EXPORT mode GRID panel; the edits themselves are
//! [`crate::analysis::phrase_edit`]). One row per track: the mood and the whole phrase list, which replaces the analysed phrases
//! wherever they show or are exported ([`super::overlay`]). No row = no edit, the analysis stands; a row with an empty list = the
//! user deleted every phrase (CLEAR), the track has none. `list` is 14 bytes per phrase, little-endian:
//! `beat u32, end_beat u32, kind u8 (index in [`KINDS`]), variant u8, fill u32` (0 = no fill-in; beats are 1-based). The table layout
//! is ours.

use super::{R, Store, query_opt};
use crate::analysis::{Mood, Phrase, PhraseKind, Phrases};
use rusqlite::params;

const KINDS: [PhraseKind; 7] = [
    PhraseKind::Intro,
    PhraseKind::Verse,
    PhraseKind::Chorus,
    PhraseKind::Bridge,
    PhraseKind::Outro,
    PhraseKind::Up,
    PhraseKind::Down,
];
/// Mood numbers: 1 high, 2 mid, 3 low.
const MOODS: [Mood; 3] = [Mood::High, Mood::Mid, Mood::Low];
const ENTRY: usize = 14;

fn encode(p: &Phrases) -> Vec<u8> {
    let mut v = Vec::with_capacity(p.list.len() * ENTRY);
    for ph in &p.list {
        v.extend(ph.beat.to_le_bytes());
        v.extend(ph.end_beat.to_le_bytes());
        v.push(KINDS.iter().position(|&k| k == ph.kind).unwrap_or(0) as u8);
        v.push(ph.variant);
        v.extend(ph.fill.unwrap_or(0).to_le_bytes());
    }
    v
}

fn decode(mood: i64, blob: &[u8]) -> Phrases {
    let u32_at = |e: &[u8], o: usize| u32::from_le_bytes([e[o], e[o + 1], e[o + 2], e[o + 3]]);
    Phrases {
        mood: MOODS[(mood as usize).saturating_sub(1).min(2)],
        list: blob
            .chunks_exact(ENTRY)
            .map(|e| Phrase {
                beat: u32_at(e, 0),
                end_beat: u32_at(e, 4),
                kind: KINDS[usize::from(e[8]).min(KINDS.len() - 1)],
                variant: e[9],
                fill: Some(u32_at(e, 10)).filter(|&f| f != 0),
            })
            .collect(),
    }
}

impl Store {
    /// The edited phrases of `key` (an empty list = all deleted), `None` when never edited.
    pub fn phrase_edit(&self, key: &str) -> R<Option<Phrases>> {
        query_opt(
            &self.db(),
            "SELECT mood, list FROM phrase_edit WHERE key = ?1",
            [key],
            |r| Ok(decode(r.get(0)?, &r.get::<_, Vec<u8>>(1)?)),
        )
    }

    /// Stores `p` as the whole phrase list of `key` (an empty one = CLEAR).
    pub fn set_phrase_edit(&self, key: &str, p: &Phrases) -> R<()> {
        let mood = MOODS.iter().position(|&m| m == p.mood).unwrap_or(1) as i64 + 1;
        self.run(
            "INSERT INTO phrase_edit(key, mood, list) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET mood = ?2, list = ?3",
            params![key, mood, encode(p)],
        )
    }

    /// Drops the edit: the analysed phrases show again.
    pub fn clear_phrase_edit(&self, key: &str) -> R<()> {
        self.run("DELETE FROM phrase_edit WHERE key = ?1", [key])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Phrases {
        let ph = |beat, end_beat, kind, variant, fill| Phrase {
            beat,
            end_beat,
            kind,
            variant,
            fill,
        };
        Phrases {
            mood: Mood::Low,
            list: vec![
                ph(1, 17, PhraseKind::Intro, 0, None),
                ph(17, 33, PhraseKind::Verse, 2, Some(31)),
                ph(33, 49, PhraseKind::Down, 0, None),
            ],
        }
    }

    #[test]
    fn an_edit_round_trips_replaces_and_reverts() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.phrase_edit("a|1").unwrap(), None);
        assert_eq!(s.edits("a|1").unwrap().phrases, None);
        s.set_phrase_edit("a|1", &song()).unwrap();
        assert_eq!(s.phrase_edit("a|1").unwrap(), Some(song()));
        assert_eq!(
            s.edits("a|1").unwrap().phrases,
            Some(song()),
            "edits() carries it even for a track with no other edit"
        );
        // A second set replaces the whole list; an empty one is "cleared", not "no edit".
        let cleared = Phrases {
            mood: Mood::High,
            list: Vec::new(),
        };
        s.set_phrase_edit("a|1", &cleared).unwrap();
        assert_eq!(s.phrase_edit("a|1").unwrap(), Some(cleared));
        assert_eq!(
            s.phrase_edit("b|2").unwrap(),
            None,
            "other tracks are untouched"
        );
        s.clear_phrase_edit("a|1").unwrap();
        assert_eq!(s.phrase_edit("a|1").unwrap(), None);
    }

    #[test]
    fn rekey_carries_the_edit() {
        let s = Store::in_memory().unwrap();
        s.set_phrase_edit("a|1", &song()).unwrap();
        s.rekey("a|1", "b|2").unwrap();
        assert_eq!(s.phrase_edit("b|2").unwrap(), Some(song()));
        assert_eq!(s.phrase_edit("a|1").unwrap(), None);
    }

    #[test]
    fn the_overlay_puts_the_edit_on_the_analysis_even_when_it_deleted_every_phrase() {
        let s = Store::in_memory().unwrap();
        let mut a = crate::analysis::Analysis {
            phrases: Some(Phrases {
                mood: Mood::High,
                list: vec![song().list[0].clone()],
            }),
            ..Default::default()
        };
        super::super::apply(&mut a, &s.edits("a|1").unwrap());
        assert_eq!(
            a.phrases.as_ref().map(|p| p.list.len()),
            Some(1),
            "no edit: the analysis stands"
        );
        s.set_phrase_edit("a|1", &song()).unwrap();
        super::super::apply(&mut a, &s.edits("a|1").unwrap());
        assert_eq!(a.phrases, Some(song()));
        let cleared = Phrases {
            mood: Mood::Low,
            list: Vec::new(),
        };
        s.set_phrase_edit("a|1", &cleared).unwrap();
        super::super::apply(&mut a, &s.edits("a|1").unwrap());
        assert_eq!(a.phrases, Some(cleared));
    }
}
