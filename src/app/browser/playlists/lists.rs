//! The track lists the sidebar entries show, and the order edits on a playlist (sort, renumber, remove, move).

use crate::app::browser::{
    App, Home, View,
    smart::Smart,
    table::{bpm_key, cell},
};
use openfader::{library::Track, store};

/// `playlist_op` names that sort by a column, indexed by the table column they sort on ("" = no such sort).
const SORT_OPS: [&str; 8] = [
    "sort-title",
    "sort-artist",
    "sort-genre",
    "sort-key",
    "",
    "",
    "",
    "sort-comment",
];

impl View {
    /// The library tracks CUE Analysis set cues on.
    pub(in crate::app::browser) fn cue_analysis_list(&mut self) -> Result<Vec<Track>, String> {
        let (marked, cache) = (crate::app::analyze::cue_analysed(), self.cache()?);
        Ok(cache
            .tracks
            .iter()
            .zip(&cache.keys)
            .filter(|(_, k)| marked.contains(*k))
            .map(|(t, _)| t.clone())
            .collect())
    }

    /// Every library track that passes the smart playlist's conditions (library order).
    pub(in crate::app::browser) fn smart_list(&mut self, id: i64) -> Result<Vec<Track>, String> {
        let text = self.store.smart(id)?.ok_or(tr!(
            "this smart playlist has no conditions yet: EDIT adds some"
        ))?;
        let smart = Smart::parse(&text)?;
        let cache = self.cache()?;
        let hit = |t: &Track, k: &String| {
            let (bpm, key) = bpm_key(t);
            smart.matches(t, bpm, &key, self.tag_map.get(k).map_or(&[], Vec::as_slice))
        };
        Ok(cache
            .tracks
            .iter()
            .zip(&cache.keys)
            .filter(|(t, k)| hit(t, k))
            .map(|(t, _)| t.clone())
            .collect())
    }

    /// The tracks of the keys in `keys`, in that order (keys the library lacks are skipped); `slots` gets each one's index in `keys`.
    pub(in crate::app::browser) fn tracks_of(
        &mut self,
        keys: &[String],
        slots: bool,
    ) -> Result<Vec<Track>, String> {
        let cache = self.cache()?;
        let mut out = Vec::with_capacity(keys.len());
        for (n, k) in keys.iter().enumerate() {
            if let Some(&i) = cache.by_key.get(k) {
                out.push(cache.tracks[i].clone());
                if slots {
                    self.slots.push(n);
                }
            }
        }
        Ok(out)
    }

    pub(in crate::app::browser) fn user_list(&mut self, id: i64) -> Result<Vec<Track>, String> {
        let keys = self.store.entries(id)?;
        self.tracks_of(&keys, true)
    }

    pub(in crate::app::browser) fn day_list(&mut self, day: &str) -> Result<Vec<Track>, String> {
        let keys = self.store.history_day(day)?;
        self.tracks_of(&keys, false)
    }

    /// The plays of `day` with their times; plays of tracks the library lacks are skipped.
    pub(in crate::app::browser) fn day_plays(
        &mut self,
        day: &str,
    ) -> Result<Vec<(Track, i64)>, String> {
        let mut out = Vec::new();
        for (key, at) in self.store.history_day_plays(day)? {
            out.extend(self.tracks_of(&[key], false)?.pop().map(|t| (t, at)));
        }
        Ok(out)
    }

    pub(in crate::app::browser) fn dupes_list(&mut self) -> Result<Vec<Track>, String> {
        let cache = self.cache()?;
        let groups = crate::app::library::dupes::find(&cache.tracks);
        self.dupe_groups = groups.len();
        Ok(groups
            .into_iter()
            .flatten()
            .map(|i| cache.tracks[i].clone())
            .collect())
    }

    /// Sort Items / Renumber Track Order on the playlist on screen: the order is stored. Renumber keeps the order and drops the
    /// places of tracks the library no longer has.
    pub(super) fn reorder(&mut self, app: &App, op: &str) {
        let Home::User(id) = self.home.clone() else {
            return self.say(
                app,
                tr!("choose one of your playlists first (My Playlists)"),
            );
        };
        if self.protected(app, tr!("reorder a playlist")) {
            return;
        }
        let r = self.user_list(id);
        let Some(mut tracks) = self.try_store(app, r) else {
            return;
        };
        match op {
            "sort-bpm" => tracks.sort_by_key(|t| bpm_key(t).0),
            "sort-time" => tracks.sort_by_key(|t| t.length_s),
            "sort-rating" => tracks.sort_by_key(|t| t.rating),
            "renumber" => {}
            _ => {
                let col = SORT_OPS.iter().position(|c| *c == op).unwrap_or(0);
                tracks.sort_by_cached_key(|t| cell(t, col).to_lowercase());
            }
        }
        let keys: Vec<String> = tracks.iter().map(store::key).collect();
        let r = self.store.set_entries(id, &keys);
        if self.try_store(app, r).is_some() {
            self.load(app);
            let did = if op == "renumber" {
                tr!("renumbered")
            } else {
                tr!("sorted")
            };
            self.say(app, tr!("{} tracks {}", keys.len(), did));
        }
    }

    /// Takes list entry `pos` out of the Tag List / user playlist on screen; false when neither is.
    pub(in crate::app::browser) fn remove_at(&mut self, app: &App, pos: usize) -> bool {
        if matches!(self.home, Home::User(_))
            && self.protected(app, tr!("remove tracks from a playlist"))
        {
            return false;
        }
        match self.home.clone() {
            Home::Tags if pos < self.tags.len() => {
                self.tags.remove(pos);
                self.tags_changed(app);
            }
            Home::User(id) if pos < self.slots.len() => {
                let r = self.store.remove_entry(id, self.slots[pos]);
                if self.try_store(app, r).is_none() {
                    return false;
                }
                self.load(app);
            }
            Home::Bank(id) => return self.bank_remove_at(app, id, pos),
            _ => return false,
        }
        true
    }

    /// Moves list entry `from` to where entry `to` is (Tag List / user playlist on screen).
    pub(in crate::app::browser) fn move_entry(&mut self, app: &App, from: usize, to: usize) {
        match self.home.clone() {
            Home::Tags if from < self.tags.len() && to < self.tags.len() => {
                self.tags.swap(from, to);
                self.tags_changed(app);
            }
            Home::User(id) if from < self.slots.len() && to < self.slots.len() => {
                let r = self.store.move_entry(id, self.slots[from], self.slots[to]);
                if self.try_store(app, r).is_some() {
                    self.load(app);
                }
            }
            Home::Bank(id) if from < self.slots.len() && to < self.slots.len() => {
                self.bank_swap(app, id, self.slots[from], self.slots[to])
            }
            _ => {}
        }
    }
}
