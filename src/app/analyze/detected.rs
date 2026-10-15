//! The session map of what our own analysis found, and the choice of which tracks a background run still has to analyse.

use super::{Found, summary};
use crate::app::camelot::camelot;
use openfader::{
    analysis::{cache, options::Parts},
    library::Track,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Default)]
struct Inner {
    map: RefCell<HashMap<PathBuf, Found>>,
    version: Cell<u64>,
}

/// What our own analysis found, by audio path. One per UI thread: get it with [`super::detected`]; clones share the map.
#[derive(Clone, Default)]
pub struct Detected(Rc<Inner>);

impl Detected {
    /// Changes whenever bulk analysis added results; the browser re-renders its rows when it moves.
    pub fn version(&self) -> u64 {
        self.0.version.get()
    }

    pub fn get(&self, audio: &Path) -> Option<Found> {
        self.0.map.borrow().get(audio).cloned()
    }

    /// `t`'s BPM × 100 and key, the detected value standing in for each one the library leaves empty.
    pub fn fill(&self, t: &Track) -> Found {
        let map = self.0.map.borrow();
        let found = t.file.as_deref().and_then(|f| map.get(f));
        (
            if t.bpm_x100 > 0 {
                t.bpm_x100
            } else {
                found.map_or(0, |f| f.0)
            },
            if t.key.is_empty() {
                found.map_or_else(String::new, |f| f.1.clone())
            } else {
                t.key.clone()
            },
        )
    }

    /// [`get`](Self::get), else whatever the persistent cache holds for the file (remembered, so a repeat is free).
    pub fn lookup(&self, audio: &Path) -> Option<Found> {
        self.get(audio).or_else(|| {
            let found = summary(&cache::load(audio).ok()??);
            self.0
                .map
                .borrow_mut()
                .insert(audio.to_owned(), found.clone());
            Some(found)
        })
    }

    pub(super) fn insert(&self, audio: PathBuf, found: Found) {
        self.0.map.borrow_mut().insert(audio, found);
        self.0.version.set(self.0.version.get() + 1);
    }
}

/// Different keys; names in any notation ("F#m" = "11A") compare by wheel position.
pub fn keys_differ(a: &str, b: &str) -> bool {
    match (camelot(a), camelot(b)) {
        (Some(x), Some(y)) => x != y,
        _ => a.trim() != b.trim(),
    }
}

/// The tracks `parts` would fill in: those lacking a BPM (BPM / Grid part on) or a key (KEY part on), with a file that
/// nobody has analysed yet this session.
pub(super) fn pending<'a>(tracks: &'a [Track], have: &Detected, parts: &Parts) -> Vec<&'a Track> {
    let lacking = |t: &&Track| (parts.grid && t.bpm_x100 == 0) || (parts.key && t.key.is_empty());
    tracks
        .iter()
        .filter(lacking)
        .filter(|t| t.file.as_deref().is_some_and(|f| have.get(f).is_none()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(file: &str, bpm_x100: i64, key: &str) -> Track {
        Track {
            key: key.into(),
            bpm_x100,
            file: (!file.is_empty()).then(|| file.into()),
            ..Default::default()
        }
    }

    fn files(t: Vec<&Track>) -> Vec<PathBuf> {
        t.into_iter().filter_map(|t| t.file.clone()).collect()
    }

    #[test]
    fn only_tracks_lacking_what_the_enabled_parts_fill_are_queued() {
        let have = Detected::default();
        have.insert("/m/done.wav".into(), (12800, "8A".into()));
        let tracks = [
            track("/m/full.wav", 12800, "8A"),
            track("/m/nobpm.wav", 0, "8A"),
            track("/m/nokey.wav", 12800, ""),
            track("", 0, ""),
            track("/m/done.wav", 0, ""),
        ];
        let all = Parts::default();
        assert_eq!(
            files(pending(&tracks, &have, &all)),
            [PathBuf::from("/m/nobpm.wav"), "/m/nokey.wav".into()]
        );
        let no_key = Parts { key: false, ..all };
        assert_eq!(
            files(pending(&tracks, &have, &no_key)),
            [PathBuf::from("/m/nobpm.wav")],
            "a missing key is no reason with KEY off"
        );
        let no_grid = Parts { grid: false, ..all };
        assert_eq!(
            files(pending(&tracks, &have, &no_grid)),
            [PathBuf::from("/m/nokey.wav")]
        );
        let neither = Parts {
            grid: false,
            key: false,
            ..all
        };
        assert!(pending(&tracks, &have, &neither).is_empty());
    }

    #[test]
    fn detected_fills_only_what_the_library_leaves_empty() {
        let have = Detected::default();
        have.insert("/m/a.wav".into(), (12750, "9A".into()));
        assert_eq!(have.fill(&track("/m/a.wav", 0, "")), (12750, "9A".into()));
        assert_eq!(
            have.fill(&track("/m/a.wav", 12800, "8A")),
            (12800, "8A".into())
        );
        assert_eq!(have.fill(&track("/m/a.wav", 0, "8A")), (12750, "8A".into()));
        assert_eq!(have.fill(&track("/m/other.wav", 0, "")), (0, String::new()));
        assert_eq!(have.version(), 1);
    }

    #[test]
    fn keys_compare_by_wheel_position() {
        assert!(!keys_differ("11A", "F#m") && !keys_differ("8a", "8A"));
        assert!(keys_differ("8A", "8B") && keys_differ("Cmaj7", "8B"));
    }
}
