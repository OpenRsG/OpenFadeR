//! `Track` and `PerformanceData` rows: a track the library holds by path is refreshed in place, any other is added.
//!
//! What a refresh keeps (observed in the schema, inferred for the players): `dateAdded`, the id, a `bpm`, key, bitrate the
//! snapshot does not know, and every performance blob it has nothing for (a waveform or loudness the player analysed).

use crate::blob;
use crate::perf;
use crate::schema::now;
use chrono::NaiveDate;
use openfader_plugin::library::{Analysis, Track};
use rusqlite::{params, OptionalExtension, Transaction};
use std::path::Path;

/// What one track needs besides its [`Track`] row: where its audio is, and the numbers behind the sample positions.
pub struct Placed<'a> {
    pub track: &'a Track,
    pub analysis: Option<&'a Analysis>,
    /// Path as the `Track.path` column holds it: relative to `Engine Library`.
    pub path: String,
    pub file: &'a Path,
    /// Sample rate, 0 when unknown (no cue, grid or waveform is written then).
    pub rate: u32,
    /// The schema has `Track.lastEditTime` ([`crate::schema::has_edit_time`]): set on a new row.
    pub edit_time: bool,
}

/// Which parts of the performance data of a track were written, for the report.
#[derive(Default)]
pub struct Written {
    pub added: bool,
    /// Memory cues and hot cues above H that Engine has no place for, loops past the eighth.
    pub dropped: usize,
    /// A waveform was written: the player has nothing left to analyse.
    pub analyzed: bool,
}

/// `YYYY-MM-DD…` as unix seconds; the current time when the text is no date.
fn date_added(s: &str) -> i64 {
    s.get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map_or_else(now, |t| t.and_utc().timestamp())
}

fn mtime(file: &Path) -> i64 {
    std::fs::metadata(file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(now, |d| d.as_secs() as i64)
}

/// `Some(x)` for a number the snapshot knows, so a refresh keeps what the library has for a `None`.
fn known(n: i64) -> Option<i64> {
    (n > 0).then_some(n)
}

/// Writes `p` into the library; returns its `Track.id` and what was written of its performance data.
pub fn upsert(tx: &Transaction, uuid: &str, p: &Placed) -> rusqlite::Result<(i64, Written)> {
    let t = p.track;
    let a = p.analysis;
    let filename = p.path.rsplit('/').next().unwrap_or(&p.path);
    let file_type = filename
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_lowercase();
    let title = if t.title.is_empty() {
        filename.rsplit_once('.').map_or(filename, |(s, _)| s)
    } else {
        &t.title
    };
    let bpm_x100 = known(t.bpm_x100).or_else(|| {
        a.and_then(|a| a.beats.first())
            .and_then(|b| known(i64::from(b.bpm_x100)))
    });
    let key = a
        .and_then(|a| a.key.as_deref())
        .and_then(perf::key_id)
        .or_else(|| perf::key_id(&t.key));
    let bytes = std::fs::metadata(p.file).map_or(0, |m| m.len() as i64);
    let bpm = bpm_x100.map(|b| (b as f64 / 100.0).round() as i64);
    let bpm_analyzed = bpm_x100.map(|b| b as f64 / 100.0);
    let rating = i64::from(t.rating.min(5)) * 20;
    let existing: Option<i64> = tx
        .query_row("SELECT id FROM Track WHERE path = ?1", [&p.path], |r| {
            r.get(0)
        })
        .optional()?;
    let id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE Track SET length = ?2, bpm = COALESCE(?3, bpm), bpmAnalyzed = COALESCE(?4, bpmAnalyzed), \
                 filename = ?5, bitrate = COALESCE(?6, bitrate), fileBytes = ?7, title = ?8, artist = ?9, album = ?10, \
                 genre = ?11, comment = ?12, label = ?13, key = COALESCE(?14, key), rating = ?15, fileType = ?16, \
                 isAvailable = 1 WHERE id = ?1",
                params![
                    id,
                    t.length_s,
                    bpm,
                    bpm_analyzed,
                    filename,
                    known(i64::from(t.bitrate)),
                    bytes,
                    title,
                    t.artist,
                    t.album,
                    t.genre,
                    t.comment,
                    t.label,
                    key,
                    rating,
                    file_type
                ],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO Track (length, bpm, path, filename, bitrate, bpmAnalyzed, albumArtId, fileBytes, title, artist, \
                 album, genre, comment, label, composer, remixer, key, rating, isPlayed, fileType, isAnalyzed, dateCreated, \
                 dateAdded, isAvailable, isMetadataOfPackedTrackChanged, isPerfomanceDataOfPackedTrackChanged, \
                 isMetadataImported, pdbImportKey, isBeatGridLocked, originDatabaseUuid, originTrackId, streamingFlags, \
                 explicitLyrics) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9, ?10, ?11, ?12, ?13, '', '', ?14, ?15, 0, ?16, 0, ?17, ?18, \
                 1, 0, 0, 1, 0, 0, ?19, 0, 0, 0)",
                params![
                    t.length_s,
                    bpm,
                    p.path,
                    filename,
                    known(i64::from(t.bitrate)),
                    bpm_analyzed,
                    bytes,
                    title,
                    t.artist,
                    t.album,
                    t.genre,
                    t.comment,
                    t.label,
                    key,
                    rating,
                    file_type,
                    mtime(p.file),
                    date_added(&t.date_added),
                    uuid
                ],
            )?;
            let id = tx.last_insert_rowid();
            if p.edit_time {
                tx.execute(
                    "UPDATE Track SET lastEditTime = ?2 WHERE id = ?1",
                    params![id, now()],
                )?;
            }
            id
        }
    };
    let mut w = performance(tx, id, p, key)?;
    w.added = existing.is_none();
    Ok((id, w))
}

/// The performance blobs of the track `id`. A blob the snapshot has no data for stays as it is (empty for a new track).
fn performance(
    tx: &Transaction,
    id: i64,
    p: &Placed,
    key: Option<i32>,
) -> rusqlite::Result<Written> {
    let mut w = Written::default();
    let Some(a) = p.analysis.filter(|_| p.rate > 0) else {
        return Ok(w);
    };
    let rate = f64::from(p.rate);
    let total = p.track.length_s.max(0) * i64::from(p.rate);
    let (slots, main, lost) = perf::quick_cues(&a.cues, rate);
    let (loops, more) = perf::loops(&a.cues, rate);
    w.dropped = lost + more;
    tx.execute(
        "UPDATE PerformanceData SET quickCues = ?2, loops = ?3, \
         trackData = COALESCE(trackData, ?4), \
         beatData = COALESCE(?5, beatData), overviewWaveFormData = COALESCE(?6, overviewWaveFormData) \
         WHERE trackId = ?1",
        params![
            id,
            blob::quick_cues(&slots, main),
            blob::loops(&loops),
            (total > 0).then(|| blob::track_data(rate, total, key.unwrap_or(0))),
            // The grid needs the length for its last marker; without one the player keeps its own.
            if total > 0 {
                let grid = perf::markers(&a.beats, rate, total as f64);
                (!grid.is_empty()).then(|| blob::beat_data(rate, total as f64, &grid))
            } else {
                None
            },
            perf::overview(&a.three_band_preview, total, rate).map(|o| blob::overview(&o)),
        ],
    )?;
    w.analyzed = tx.query_row(
        "SELECT overviewWaveFormData IS NOT NULL AND beatData IS NOT NULL FROM PerformanceData WHERE trackId = ?1",
        [id],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE Track SET isAnalyzed = ?2 WHERE id = ?1 AND isAnalyzed <> ?2",
        params![id, w.analyzed],
    )?;
    Ok(w)
}

/// Whether a new row for this track gets a waveform and a grid of its own: what `performance` leaves in `isAnalyzed`.
pub fn has_analysis(a: Option<&Analysis>, length_s: i64, rate: u32) -> bool {
    let total = length_s.max(0) * i64::from(rate);
    a.filter(|_| rate > 0 && total > 0).is_some_and(|a| {
        let rate = f64::from(rate);
        !perf::markers(&a.beats, rate, total as f64).is_empty()
            && perf::overview(&a.three_band_preview, total, rate).is_some()
    })
}

/// How many cues, hot cues and loops a reader finds again in the performance data written for `a` at `rate`.
pub fn expected_cues(a: &Analysis, rate: u32) -> usize {
    let rate = f64::from(rate);
    let (slots, main, _) = perf::quick_cues(&a.cues, rate);
    let (loops, _) = perf::loops(&a.cues, rate);
    perf::cues_from(&slots, main, &loops, rate).len()
}
