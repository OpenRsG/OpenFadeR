//! Transfer: tracks put onto a device straight from the browser (drag onto the device, or onto one of its playlists, "Transfer to
//! <device>"), written as an export of just those tracks ([`super::export::export`]) with the analysis the collection holds.
//!
//! - onto the device: a snapshot of the tracks and no playlist ([`Snapshot::of_tracks`]); a track the device already has is
//!   refreshed, the rest is copied.
//! - onto a device playlist: a snapshot playlist with the folder path and name of the device's one, which the format writers
//!   merge by (same parent, name and kind: its content is *replaced*). So the snapshot lists every current member first, then the
//!   new tracks ([`append`]): a member the collection also has is written with the collection's analysis, its marks only filled
//!   from the device's copy where the collection has none; any other member is carried with the analysis read from the device
//!   ([`members`]), so nothing already on the device loses its cues.

use super::link::{settle, Marks, Winner};
use crate::analysis::{cache::Glance, Analysis};
use crate::library::{snapshot::Snapshot, Library, Playlist, Track};
use crate::store;
use std::collections::{HashMap, HashSet};

/// A snapshot to export and how many of the picked tracks are in it / were left out as already there.
pub struct Plan {
    pub snapshot: Snapshot,
    pub added: usize,
    pub skipped: usize,
}

/// Whether `t` is analysed before it is written: it has a file and lacks a beat grid or a key everywhere they could come from (the
/// library's analysis `stored`, the track's own key, the analysis cache's `cached`).
pub fn needs_analysis(t: &Track, stored: Option<&Analysis>, cached: Option<&Glance>) -> bool {
    let grid =
        stored.is_some_and(|a| !a.beats.is_empty()) || cached.is_some_and(|g| g.bpm_x100 > 0);
    let key = !t.key.is_empty()
        || stored.is_some_and(|a| a.key.is_some())
        || cached.is_some_and(|g| g.key.is_some());
    t.file.is_some() && !(grid && key)
}

/// The path of playlist `id` in `playlists` (names, top level first), else the playlist that has `path`: a library rewritten
/// since the tree was drawn may number its playlists again.
pub fn find<'a>(playlists: &'a [Playlist], id: &str, path: &[String]) -> Option<&'a Playlist> {
    let plain = |p: &&Playlist| !p.is_folder;
    playlists
        .iter()
        .filter(plain)
        .find(|p| p.id == id)
        .or_else(|| {
            playlists
                .iter()
                .filter(plain)
                .find(|p| super::playlist_path(playlists, &p.id).as_deref() == Some(path))
        })
}

/// What device playlist `playlist` of `device` holds, as snapshot tracks: a track the collection has too (`own`, by
/// [`store::key`]) is the collection's track with `analysis` of it, the parts it lacks filled from the device's copy; any other is
/// the device's track with the analysis the device library gives.
pub fn members(
    device: &dyn Library,
    playlist: &str,
    own: &HashMap<String, &Track>,
    analysis: &dyn Fn(&Track) -> Option<Analysis>,
) -> Result<Vec<(Track, Option<Analysis>)>, String> {
    Ok(device
        .tracks(Some(playlist))?
        .into_iter()
        .map(|t| {
            let on_device = device.analysis(&t);
            let Some(c) = own.get(&store::key(&t)) else {
                return (t, on_device);
            };
            let mut a = analysis(c).unwrap_or_default();
            if let Some(d) = &on_device {
                let m = settle(&Marks::from(d), &Marks::from(&a), Winner::Neither).to_collection;
                (a.cues, a.beats) = (m.cues.unwrap_or(a.cues), m.beats.unwrap_or(a.beats));
                a.phrases = m.phrases.or(a.phrases);
            }
            ((*c).clone(), Some(a))
        })
        .collect())
}

/// The snapshot that makes the device playlist `name` (below the folders `folders`) hold `current` followed by `picks`; a pick
/// that `current` holds already (or an earlier pick) is left out and counted.
pub fn append(
    picks: Vec<(Track, Option<Analysis>)>,
    current: Vec<(Track, Option<Analysis>)>,
    folders: &[String],
    name: &str,
) -> Plan {
    let (n, have) = (picks.len(), current.len());
    let mut seen: HashSet<String> = current.iter().map(|(t, _)| store::key(t)).collect();
    let mut all = current;
    all.extend(picks.into_iter().filter(|(t, _)| seen.insert(store::key(t))));
    let added = all.len() - have;
    Plan {
        snapshot: Snapshot::single_playlist(all, folders, name),
        added,
        skipped: n - added,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Beat, Cue};

    fn track(title: &str) -> Track {
        Track {
            title: title.into(),
            ..Track::default()
        }
    }

    fn cue(ms: u32) -> Cue {
        Cue {
            hot_cue: 1,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    fn with_cue(title: &str, ms: u32) -> (Track, Option<Analysis>) {
        let a = Analysis {
            cues: vec![cue(ms)],
            ..Analysis::default()
        };
        (track(title), Some(a))
    }

    fn titles(s: &Snapshot) -> Vec<&str> {
        s.tracks().iter().map(|(t, _)| t.title.as_str()).collect()
    }

    #[test]
    fn a_transfer_to_the_device_has_the_tracks_and_no_playlist() {
        let s = Snapshot::of_tracks(vec![with_cue("a", 1000), with_cue("b", 2000)]);
        assert_eq!(titles(&s), ["a", "b"]);
        assert_eq!(s.playlists().count(), 0);
        assert_eq!(s.tracks()[1].1.as_ref().unwrap().cues[0].time_ms, 2000);
    }

    #[test]
    fn appending_keeps_every_member_first_and_skips_what_is_there() {
        let current = vec![with_cue("old1", 10), with_cue("old2", 20)];
        let picks = vec![with_cue("new", 30), with_cue("old2", 99), with_cue("new", 31)];
        let p = append(picks, current, &[], "Mix");
        assert_eq!((p.added, p.skipped), (1, 2));
        assert_eq!(titles(&p.snapshot), ["old1", "old2", "new"]);
        // The members keep their own analysis (the device's), not the pick's.
        let cues: Vec<u32> = p
            .snapshot
            .tracks()
            .iter()
            .map(|(_, a)| a.as_ref().unwrap().cues[0].time_ms)
            .collect();
        assert_eq!(cues, [10, 20, 30]);
        let (pl, members) = p.snapshot.playlists().next().unwrap();
        assert_eq!((pl.name.as_str(), pl.depth, pl.is_folder), ("Mix", 0, false));
        assert_eq!(members, [0, 1, 2]);
    }

    #[test]
    fn the_folder_path_of_the_device_playlist_is_kept() {
        let folders = ["Sets".to_owned(), "2026".to_owned()];
        let p = append(vec![with_cue("x", 1)], Vec::new(), &folders, "Friday");
        let rows: Vec<_> = p
            .snapshot
            .playlists()
            .map(|(p, m)| (p.name.clone(), p.depth, p.is_folder, m.len()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Sets".to_owned(), 0, true, 0),
                ("2026".to_owned(), 1, true, 0),
                ("Friday".to_owned(), 2, false, 1)
            ]
        );
    }

    struct Device(Vec<(Track, Option<Analysis>)>);

    impl Library for Device {
        fn kind(&self) -> &str {
            "device"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            Ok(Vec::new())
        }
        fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
            Ok(self.0.iter().map(|(t, _)| t.clone()).collect())
        }
        fn analysis(&self, t: &Track) -> Option<Analysis> {
            self.0.iter().find(|(x, _)| x.title == t.title)?.1.clone()
        }
    }

    #[test]
    fn members_use_the_collections_analysis_and_fill_what_it_lacks_from_the_device() {
        let beats = |bpm| {
            vec![Beat {
                bar_pos: 1,
                bpm_x100: bpm,
                time_ms: 0,
            }]
        };
        let mut dev_a = with_cue("shared", 500);
        dev_a.1.as_mut().unwrap().beats = beats(12000);
        let device = Device(vec![dev_a, with_cue("only-here", 700)]);
        let mut own_track = track("shared");
        own_track.album = "from the collection".into();
        let own: HashMap<String, &Track> = [(store::key(&own_track), &own_track)].into();
        // The collection has a grid but no cues for the shared track.
        let analysis = |_: &Track| {
            Some(Analysis {
                beats: beats(12800),
                ..Analysis::default()
            })
        };
        let m = members(&device, "p", &own, &analysis).unwrap();
        assert_eq!(m[0].0.album, "from the collection");
        let a = m[0].1.as_ref().unwrap();
        assert_eq!((a.beats[0].bpm_x100, a.cues[0].time_ms), (12800, 500));
        assert_eq!(m[1].0.title, "only-here");
        assert_eq!(m[1].1.as_ref().unwrap().cues[0].time_ms, 700);
    }

    #[test]
    fn only_tracks_without_a_grid_or_a_key_are_analysed() {
        let file = |key: &str| Track {
            file: Some("/a.mp3".into()),
            key: key.into(),
            ..Track::default()
        };
        let grid = Analysis {
            beats: vec![Beat {
                bar_pos: 1,
                bpm_x100: 12000,
                time_ms: 0,
            }],
            ..Analysis::default()
        };
        let glance = |bpm_x100, key: Option<&str>| Glance {
            bpm_x100,
            key: key.map(Into::into),
        };
        assert!(needs_analysis(&file(""), None, None));
        assert!(needs_analysis(&file("8A"), None, None)); // key, no grid
        assert!(needs_analysis(&file(""), Some(&grid), None)); // grid, no key
        assert!(!needs_analysis(&file("8A"), Some(&grid), None));
        assert!(!needs_analysis(&file(""), None, Some(&glance(12000, Some("8A")))));
        assert!(needs_analysis(&file(""), None, Some(&glance(0, Some("8A")))));
        assert!(!needs_analysis(&track("no file"), None, None));
    }

    #[test]
    fn a_playlist_is_found_by_id_or_by_its_path() {
        let p = |id: &str, name: &str, depth, is_folder| Playlist {
            id: id.into(),
            name: name.into(),
            depth,
            is_folder,
        };
        let pls = [p("1", "Sets", 0, true), p("2", "Mix", 1, false), p("3", "Mix", 0, false)];
        assert_eq!(find(&pls, "2", &[]).unwrap().id, "2");
        let path = ["Sets".to_owned(), "Mix".to_owned()];
        assert_eq!(find(&pls, "gone", &path).unwrap().id, "2");
        assert!(find(&pls, "1", &["Sets".to_owned()]).is_none());
    }
}
