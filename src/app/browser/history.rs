//! Session history: the sidebar entry "Session history (N)" lists every track that played at least
//! [`MIN_ON_AIR`] *on air* in this session, in play order; SAVE M3U8 writes the list into the shared
//! output folder ([`paths`]). Never written to the library.
//!
//! The stored play history (sidebar "History" > day) uses the same on-air rule with the longer [`MIN_PLAYED`]: a
//! track that was on air that long is appended to today's history in the store ([`log_today`]). The threshold is 60 s.
//!
//! On air (our reading of a "played" rule; 30 s): the deck is playing and
//! its channel fader times its crossfader gain is at least [`ON_AIR`]. Only time on air counts, and each
//! LOAD of a track counts separately (the deck's load serial), so a track played twice is listed twice.
//! [`Watcher`] samples the decks from the browser's 200 ms timer.

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

fn write_new(dir: &Path, tracks: &[Track]) -> Result<(PathBuf, usize), String> {
    let path = paths::at(paths::timestamped(dir, "history", "m3u8"), dir)?;
    let (text, n) = m3u8(tracks);
    let mut f = paths::at(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path),
        &path,
    )?;
    paths::at(f.write_all(text.as_bytes()), &path)?;
    Ok((path, n))
}

/// Writes `<$OPENFADER_EXPORT_DIR | …>/history-YYYYmmdd-HHMMSS.m3u8` (never overwrites). Returns the path and the track count.
pub fn save(tracks: &[Track]) -> Result<(PathBuf, usize), String> {
    if tracks.is_empty() {
        return Err("this list is empty".into());
    }
    write_new(
        &paths::output_dir("OPENFADER_EXPORT_DIR").ok_or("set OPENFADER_EXPORT_DIR or HOME")?,
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
