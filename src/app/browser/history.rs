//! Session history: the sidebar entry "Session history (N)" lists every track that played at least
//! [`MIN_ON_AIR`] *on air* in this session, in play order; SAVE M3U8 writes the list into the shared
//! output folder ([`paths`]), SAVE SET LIST writes it (or a stored day, below) as `.csv` and `.txt` set lists next to it.
//! Never written to the library.
//!
//! The stored play history (sidebar "History" > day) uses the same on-air rule with the longer [`MIN_PLAYED`]: a
//! track that was on air that long is appended to today's history in the store ([`log_today`]). The threshold is 60 s.
//!
//! On air (our reading of a "played" rule; 30 s): the deck is playing and
//! its channel fader times its crossfader gain is at least [`ON_AIR`]. Only time on air counts, and each
//! LOAD of a track counts separately (the deck's load serial), so a track played twice is listed twice.
//! [`Watcher`] samples the decks from the browser's 200 ms timer.

use super::{table::bpm_key, Home, View};
use crate::app::{
    deck::{active, Decks},
    mixer, App, DeckState, MixerState,
};
use openfader::{
    library::{playlist_file::m3u8, Track},
    paths,
    store::{self, Store},
};
use slint::{ComponentHandle, Model};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Time on air before a track is logged in the session history.
pub const MIN_ON_AIR: Duration = Duration::from_secs(30);
/// Time on air before a track is appended to the stored play history.
pub const MIN_PLAYED: Duration = Duration::from_secs(60);
/// Channel fader × crossfader gain from which a deck counts as on air (−20 dB).
const ON_AIR: f32 = 0.1;
/// A longer gap between samples (the UI stalled) is not counted as air time.
const MAX_STEP: Duration = Duration::from_secs(1);

/// Whether a deck is audible in the master mix.
pub fn on_air(playing: bool, fader: f32, side: f32) -> bool {
    playing && fader * side >= ON_AIR
}

/// One deck's air time for its current load.
#[derive(Default)]
struct Slot {
    load: Option<u32>,
    air: Duration,
    logged: bool,
}

/// Adds `dt` of air time to the deck's current `load` (`None`: empty deck); a different load starts from zero.
/// True exactly once per load: when its air time first reaches `min`.
fn advance(slot: &mut Slot, load: Option<u32>, on_air: bool, dt: Duration, min: Duration) -> bool {
    if slot.load != load {
        *slot = Slot {
            load,
            ..Slot::default()
        };
    }
    if load.is_none() || !on_air || slot.logged {
        return false;
    }
    slot.air += dt;
    slot.logged = slot.air >= min;
    slot.logged
}

pub struct Watcher {
    slots: Vec<Slot>,
    last: Instant,
    min: Duration,
}

impl Watcher {
    /// Reports each loaded track once it has been on air for `min`.
    pub fn new(min: Duration) -> Self {
        Watcher {
            slots: Vec::new(),
            last: Instant::now(),
            min,
        }
    }

    /// Tracks that reached the threshold since the last call.
    pub fn poll(&mut self, app: &App, decks: &Decks) -> Vec<Track> {
        let now = Instant::now();
        let dt = now
            .duration_since(std::mem::replace(&mut self.last, now))
            .min(MAX_STEP);
        let (view, channels, side) = (
            app.global::<DeckState>().get_decks(),
            app.global::<MixerState>().get_channels(),
            mixer::sides(app),
        );
        let n = active().min(decks.len());
        self.slots.resize_with(n, Slot::default);
        let min = self.min;
        (0..n)
            .filter_map(|i| {
                let playing = view.row_data(i).is_some_and(|d| d.playing);
                let fader = channels.row_data(i).map_or(0.0, |c| c.fader);
                let load = decks[i].serial();
                let air = on_air(playing, fader, side.get(i).copied().unwrap_or(1.0));
                advance(&mut self.slots[i], load, air, dt, min)
                    .then(|| decks[i].track().map(|t| t.1))
                    .flatten()
            })
            .collect()
    }
}

/// Appends `t` to today's stored play history (the key and the local day are taken now).
pub fn log_today(store: &Store, t: &Track) -> Result<(), String> {
    let now = chrono::Local::now();
    store.history_append(
        &now.format("%Y-%m-%d").to_string(),
        &store::key(t),
        now.timestamp(),
    )
}

/// Writes `text` to a new `<dir>/<prefix>-YYYYmmdd-HHMMSS.<ext>` (never overwrites).
fn write_text(dir: &Path, prefix: &str, ext: &str, text: &str) -> Result<PathBuf, String> {
    let path = paths::at(paths::timestamped(dir, prefix, ext), dir)?;
    write_to(&path, text)?;
    Ok(path)
}

fn write_to(path: &Path, text: &str) -> Result<(), String> {
    let mut f = paths::at(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path),
        path,
    )?;
    paths::at(f.write_all(text.as_bytes()), path)
}

fn write_new(dir: &Path, tracks: &[Track]) -> Result<(PathBuf, usize), String> {
    let (text, n) = m3u8(tracks);
    Ok((write_text(dir, "history", "m3u8", &text)?, n))
}

/// One play of a set list: when it started (Unix seconds) and what the history row shows.
struct Row {
    at: i64,
    title: String,
    artist: String,
    /// BPM × 100, 0 = unknown.
    bpm: i64,
    key: String,
    length_s: i64,
}

fn clock(at: i64, seconds: bool) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format(if seconds { "%H:%M:%S" } else { "%H:%M" })
                .to_string()
        })
        .unwrap_or_default()
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// `position,start,title,artist,bpm,key,length`, one line per play. The history records when a track reached the on-air
/// threshold, not how long it then played, so the last column is the track's length (empty when unknown).
fn set_list_csv(rows: &[Row]) -> String {
    let mut out = String::from("position,start,title,artist,bpm,key,length\n");
    for (i, r) in rows.iter().enumerate() {
        let bpm = if r.bpm > 0 { format!("{:.2}", r.bpm as f64 / 100.0) } else { String::new() };
        let length = if r.length_s > 0 { format!("{}:{:02}", r.length_s / 60, r.length_s % 60) } else { String::new() };
        out += &format!(
            "{},{},{},{},{},{},{}\n",
            i + 1,
            clock(r.at, true),
            csv_cell(&r.title),
            csv_cell(&r.artist),
            bpm,
            csv_cell(&r.key),
            length
        );
    }
    out
}

/// `HH:MM  Artist - Title` per play, the shape of a track list pasted into a post (no artist: just the title).
fn set_list_txt(rows: &[Row]) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for r in rows {
        let (artist, title) = (flat(&r.artist), flat(&r.title));
        let name = if artist.is_empty() { title } else { format!("{artist} - {title}") };
        out += &format!("{}  {name}\n", clock(r.at, false));
    }
    out
}

/// The set list in the format of the file name extension (`csv` or `txt`); `None` for any other.
fn set_list(rows: &[Row], ext: &str) -> Option<String> {
    match ext {
        "csv" => Some(set_list_csv(rows)),
        "txt" => Some(set_list_txt(rows)),
        _ => None,
    }
}

fn write_set_list(dir: &Path, rows: &[Row]) -> Result<(PathBuf, PathBuf), String> {
    let csv = paths::at(paths::timestamped(dir, "setlist", "csv"), dir)?;
    let txt = csv.with_extension("txt");
    for p in [&csv, &txt] {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
        write_to(p, &set_list(rows, ext).ok_or_else(|| format!("{}: unknown format", p.display()))?)?;
    }
    Ok((csv, txt))
}

/// SAVE SET LIST: the stored day or the session history on screen, as `setlist-YYYYmmdd-HHMMSS.csv` and `.txt` in the
/// export folder (never overwrites). Returns both paths.
pub(super) fn save_set_list(v: &mut View) -> Result<(PathBuf, PathBuf), String> {
    let plays: Vec<(Track, i64)> = match v.home.clone() {
        Home::Day(day) => v.day_plays(&day)?,
        Home::History => v.history.iter().cloned().zip(v.history_at.iter().copied()).collect(),
        _ => return Err(tr!("open the Session history or a day of History first").into()),
    };
    if plays.is_empty() {
        return Err(tr!("this list is empty").into());
    }
    let rows: Vec<Row> = plays
        .into_iter()
        .map(|(t, at)| {
            let (bpm, key) = bpm_key(&t);
            Row { at, title: t.title, artist: t.artist, bpm, key, length_s: t.length_s }
        })
        .collect();
    write_set_list(
        &paths::output_dir("OPENFADER_EXPORT_DIR").ok_or(tr!("set OPENFADER_EXPORT_DIR or HOME"))?,
        &rows,
    )
}

/// Writes `<$OPENFADER_EXPORT_DIR | …>/history-YYYYmmdd-HHMMSS.m3u8` (never overwrites). Returns the path and the track count.
pub fn save(tracks: &[Track]) -> Result<(PathBuf, usize), String> {
    if tracks.is_empty() {
        return Err(tr!("this list is empty").into());
    }
    write_new(
        &paths::output_dir("OPENFADER_EXPORT_DIR")
            .ok_or(tr!("set OPENFADER_EXPORT_DIR or HOME"))?,
        tracks,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, file: Option<&str>, length_s: i64) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            length_s,
            file: file.map(PathBuf::from),
            ..Track::default()
        }
    }

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn on_air_needs_play_and_both_faders_open() {
        assert!(on_air(true, 1.0, 1.0));
        assert!(on_air(true, 1.0, 0.707)); // crossfader centre, constant power
        assert!(on_air(true, 0.5, 0.5));
        assert!(!on_air(false, 1.0, 1.0)); // paused
        assert!(!on_air(true, 0.0, 1.0)); // channel fader down
        assert!(!on_air(true, 1.0, 0.0)); // crossfaded away
        assert!(!on_air(true, 0.2, 0.2)); // both nearly closed: −28 dB
    }

    #[test]
    fn a_track_is_logged_once_after_30_seconds_on_air() {
        let mut s = Slot::default();
        assert!((0..29).all(|_| !advance(&mut s, Some(1), true, S, MIN_ON_AIR)));
        assert!(advance(&mut s, Some(1), true, S, MIN_ON_AIR)); // the 30th second
        assert!((0..60).all(|_| !advance(&mut s, Some(1), true, S, MIN_ON_AIR)));
        // never again for this load
    }

    #[test]
    fn the_stored_history_waits_for_60_seconds() {
        let mut s = Slot::default();
        assert!((0..59).all(|_| !advance(&mut s, Some(1), true, S, MIN_PLAYED)));
        assert!(advance(&mut s, Some(1), true, S, MIN_PLAYED));
        assert!(!advance(&mut s, Some(1), true, 100 * S, MIN_PLAYED));
    }

    #[test]
    fn only_time_on_air_counts() {
        let mut s = Slot::default();
        for _ in 0..25 {
            advance(&mut s, Some(1), true, S, MIN_ON_AIR);
        }
        for _ in 0..100 {
            assert!(!advance(&mut s, Some(1), false, S, MIN_ON_AIR)); // paused / faded out: no progress
        }
        assert!(!advance(&mut s, Some(1), true, 4 * S, MIN_ON_AIR)); // 29 s
        assert!(advance(&mut s, Some(1), true, S, MIN_ON_AIR));
    }

    #[test]
    fn a_new_load_starts_over_and_an_empty_deck_never_logs() {
        let mut s = Slot::default();
        for _ in 0..20 {
            advance(&mut s, Some(1), true, S, MIN_ON_AIR);
        }
        assert!(
            !advance(&mut s, Some(2), true, 20 * S, MIN_ON_AIR),
            "a reload resets the clock: 20 s of the new load"
        );
        assert!(advance(&mut s, Some(2), true, 10 * S, MIN_ON_AIR));
        assert!(advance(&mut s, Some(3), true, 30 * S, MIN_ON_AIR)); // a later load of anything counts again
        assert!(!advance(
            &mut Slot::default(),
            None,
            true,
            60 * S,
            MIN_ON_AIR
        ));
    }

    #[test]
    fn played_tracks_append_to_todays_history_in_the_store() {
        let store = Store::in_memory().unwrap();
        let t = Track {
            title: "One".into(),
            ..Track::default()
        };
        log_today(&store, &t).unwrap();
        log_today(&store, &t).unwrap();
        let days = store.history_days().unwrap();
        assert_eq!(days, [chrono::Local::now().format("%Y-%m-%d").to_string()]);
        assert_eq!(
            store.history_day(&days[0]).unwrap(),
            [store::key(&t), store::key(&t)]
        );
    }

    #[test]
    fn m3u8_lists_tracks_with_files_in_order() {
        let (text, n) = m3u8(&[
            track("One", "A", Some("/m/one.mp3"), 200),
            track("No file", "B", None, 10),
            track("Two\nlines", "", Some("/m/two.flac"), 0),
        ]);
        assert_eq!(n, 2);
        assert_eq!(
            text,
            "#EXTM3U\n#EXTINF:200,A - One\n/m/one.mp3\n#EXTINF:-1,Two lines\n/m/two.flac\n"
        );
        assert_eq!(m3u8(&[]), ("#EXTM3U\n".to_owned(), 0));
    }

    fn row(at: i64, title: &str, artist: &str, bpm: i64, key: &str, length_s: i64) -> Row {
        Row { at, title: title.into(), artist: artist.into(), bpm, key: key.into(), length_s }
    }

    fn at(h: u32, m: u32, s: u32) -> i64 {
        use chrono::TimeZone;
        chrono::Local.with_ymd_and_hms(2026, 10, 25, h, m, s).unwrap().timestamp()
    }

    #[test]
    fn set_list_csv_escapes_and_leaves_unknowns_empty() {
        let rows = [
            row(at(21, 5, 9), "Hello, \"World\"", "A", 12850, "8A", 245),
            row(at(21, 9, 0), "Two\nlines", "", 0, "", 0),
        ];
        assert_eq!(
            set_list(&rows, "csv").unwrap(),
            "position,start,title,artist,bpm,key,length\n\
             1,21:05:09,\"Hello, \"\"World\"\"\",A,128.50,8A,4:05\n\
             2,21:09:00,\"Two\nlines\",,,,\n"
        );
    }

    #[test]
    fn set_list_txt_is_clock_artist_dash_title() {
        let rows = [
            row(at(9, 7, 59), "One", "A", 0, "", 0),
            row(at(23, 0, 0), "Two\nlines", "", 0, "", 0),
        ];
        assert_eq!(set_list(&rows, "txt").unwrap(), "09:07  A - One\n23:00  Two lines\n");
        assert_eq!(set_list(&rows, "mp3"), None);
    }

    #[test]
    fn set_list_saves_both_formats_side_by_side() {
        let dir = std::env::temp_dir().join(format!("openfader-setlist-{}", std::process::id()));
        let (csv, txt) = write_set_list(&dir, &[row(at(1, 2, 3), "One", "A", 0, "", 0)]).unwrap();
        assert_eq!(csv.with_extension(""), txt.with_extension(""));
        assert_eq!(std::fs::read_to_string(&txt).unwrap(), "01:02  A - One\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_writes_a_new_file_each_time_and_refuses_an_empty_history() {
        let dir = std::env::temp_dir().join(format!("openfader-history-{}", std::process::id()));
        let t = [track("One", "A", Some("/m/one.mp3"), 200)];
        let (a, n) = write_new(&dir, &t).unwrap();
        let (b, _) = write_new(&dir, &t).unwrap();
        assert_eq!(n, 1);
        assert_ne!(a, b);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), m3u8(&t).0);
        assert_eq!(a.extension().unwrap(), "m3u8");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(save(&[]).is_err());
    }
}
