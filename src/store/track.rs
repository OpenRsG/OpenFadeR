//! Per-track edits: rating, comment, colour, the cue list (hot / memory cues and loops) and the beat
//! grid. `None` = never edited, the library's value stands; `Some` = the store's value wins (a stored
//! cue list replaces the library's as a whole, so a deleted library cue stays deleted).

use super::{R, Sql, Store, cue_at, exec, query_all, query_opt, rgb, scalar};
use crate::analysis::{Beat, Cue, Phrases};
use rusqlite::{ToSql, params};
use std::collections::HashMap;

/// Track colours by colour id − 1: name and RGB.
pub const TRACK_COLORS: [(&str, [u8; 3]); 8] = [
    ("Pink", [0xf8, 0x70, 0xf8]),
    ("Red", [0xff, 0x00, 0x00]),
    ("Orange", [0xf8, 0xa0, 0x30]),
    ("Yellow", [0xf8, 0xe3, 0x30]),
    ("Green", [0x00, 0xe0, 0x00]),
    ("Aqua", [0x00, 0xc0, 0xf8]),
    ("Blue", [0x00, 0x50, 0xf8]),
    ("Purple", [0x98, 0x08, 0xf8]),
];

/// Everything [`Store::edits`] returns for one track.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edits {
    /// Stars 0..=5.
    pub rating: Option<u8>,
    pub comment: Option<String>,
    /// Track colour: colour id 1..=8 (pink, red, orange, yellow, green, aqua, blue, purple).
    pub color: Option<u8>,
    pub cues: Option<Vec<Cue>>,
    pub grid: Option<Vec<Beat>>,
    /// The whole phrase list ([`Store::phrase_edit`]); an empty one = all phrases deleted.
    pub phrases: Option<Phrases>,
}

/// The cheap fields of one track ([`Store::track_fields`]): what a track list needs, without cues and grids.
#[derive(Debug)]
pub(super) struct Fields {
    pub rating: Option<u8>,
    pub comment: Option<String>,
}

/// Bytes per beat in the grid blob.
const BEAT: usize = 8;

fn grid_blob(grid: &[Beat]) -> Vec<u8> {
    let mut v = Vec::with_capacity(grid.len() * BEAT);
    for b in grid {
        v.extend(b.time_ms.to_le_bytes());
        v.extend(b.bpm_x100.to_le_bytes());
        v.extend(b.bar_pos.to_le_bytes());
    }
    v
}

fn grid_from(blob: &[u8]) -> Vec<Beat> {
    blob.chunks_exact(BEAT)
        .map(|c| Beat {
            time_ms: u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
            bpm_x100: u16::from_le_bytes([c[4], c[5]]),
            bar_pos: u16::from_le_bytes([c[6], c[7]]),
        })
        .collect()
}

impl Store {
    /// Sets one nullable column of `key`'s row (created on demand); `None` clears it.
    fn set_field(&self, col: &str, key: &str, v: impl ToSql) -> R<()> {
        // `col` is always one of our own literals, never user input.
        self.run(
            &format!(
                "INSERT INTO track(key, {col}) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET {col} = ?2"
            ),
            params![key, v],
        )
    }

    pub fn set_rating(&self, key: &str, stars: Option<u8>) -> R<()> {
        self.set_field("rating", key, stars.map(|s| s.min(5)))
    }

    pub fn set_comment(&self, key: &str, comment: Option<&str>) -> R<()> {
        self.set_field("comment", key, comment)
    }

    /// `color`: colour id 1..=8, `None` = no colour override (values above 8 are ignored).
    pub fn set_color(&self, key: &str, color: Option<u8>) -> R<()> {
        self.set_field("color", key, color.filter(|c| (1..=8).contains(c)))
    }

    /// Stores the whole cue list of `key` (hot cues, memory cues, loops; list order kept); `None` drops it
    /// so the library's cues show again.
    pub fn set_cues(&self, key: &str, cues: Option<&[Cue]>) -> R<()> {
        self.write(|tx| {
            exec(
                tx,
                "INSERT INTO track(key, cues_set) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET cues_set = ?2",
                params![key, cues.is_some()],
            )?;
            exec(tx, "DELETE FROM cue WHERE key = ?1", [key])?;
            let mut ins = tx
                .prepare_cached(
                    "INSERT INTO cue(key, pos, hot, time_ms, loop_end_ms, color, comment) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                )
                .s()?;
            for (pos, c) in cues.unwrap_or_default().iter().enumerate() {
                ins.execute(params![
                    key,
                    pos as i64,
                    c.hot_cue,
                    c.time_ms,
                    c.loop_end_ms,
                    c.color.map(rgb),
                    c.comment
                ])
                .s()?;
            }
            Ok(())
        })
    }

    /// Stores an edited beat grid. `original` is the grid before the edit; the first call for a track keeps
    /// it for [`Store::reset_grid`], later calls leave it alone.
    pub fn set_grid(&self, key: &str, grid: &[Beat], original: &[Beat]) -> R<()> {
        self.run(
            "INSERT INTO track(key, grid, grid_orig) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET grid = ?2, grid_orig = COALESCE(grid_orig, ?3)",
            params![key, grid_blob(grid), grid_blob(original)],
        )
    }

    /// Drops the edited grid and returns the original it replaced; `None` when the grid was never edited.
    pub fn reset_grid(&self, key: &str) -> R<Option<Vec<Beat>>> {
        let db = self.db();
        let orig = scalar::<Option<Vec<u8>>>(
            &db,
            "SELECT grid_orig FROM track WHERE key = ?1 AND grid IS NOT NULL",
            [key],
        )?
        .flatten();
        exec(
            &db,
            "UPDATE track SET grid = NULL, grid_orig = NULL WHERE key = ?1",
            [key],
        )?;
        Ok(orig.map(|b| grid_from(&b)))
    }

    /// Every edit of `key` (all `None` for an untouched track).
    pub fn edits(&self, key: &str) -> R<Edits> {
        let mut edits = {
            let db = self.db();
            let row = query_opt(
                &db,
                "SELECT rating, comment, color, cues_set, grid FROM track WHERE key = ?1",
                [key],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, bool>(3)?,
                        r.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                },
            )?;
            match row {
                None => Edits::default(),
                Some((rating, comment, color, cues_set, grid)) => Edits {
                    rating,
                    comment,
                    color,
                    cues: cues_set
                        .then(|| {
                            query_all(
                                &db,
                                "SELECT hot, time_ms, loop_end_ms, color, comment FROM cue WHERE key = ?1 ORDER BY pos",
                                [key],
                                |r| cue_at(r, 0),
                            )
                        })
                        .transpose()?,
                    grid: grid.map(|b| grid_from(&b)),
                    phrases: None,
                },
            }
        };
        edits.phrases = self.phrase_edit(key)?;
        Ok(edits)
    }

    /// Rating and comment of every track that has either, by [`super::key`].
    pub(super) fn track_fields(&self) -> R<HashMap<String, Fields>> {
        query_all(
            &self.db(),
            "SELECT key, rating, comment FROM track WHERE rating IS NOT NULL OR comment IS NOT NULL",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    Fields {
                        rating: r.get(1)?,
                        comment: r.get(2)?,
                    },
                ))
            },
        )
    }

    pub fn track_color(&self, key: &str) -> R<Option<u8>> {
        Ok(
            scalar::<Option<u8>>(&self.db(), "SELECT color FROM track WHERE key = ?1", [key])?
                .flatten(),
        )
    }

    /// Colour of every coloured track, by key.
    pub fn all_track_colors(&self) -> R<HashMap<String, u8>> {
        query_all(
            &self.db(),
            "SELECT key, color FROM track WHERE color IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}
