//! [`Snapshot`]: the owned, `Send` input of a collection or device export: a library's unique tracks with their
//! analysis, its playlist tree (history playlists left out) and what only our store has (track colours, tags, user
//! playlists, see [`extra`]). Take it on the thread that owns the [`Library`], write it anywhere: a library plugin writes
//! the collection as a file ([`Snapshot::to_wire`] is what crosses the plugin ABI), the device export reads it directly.

mod extra;

use super::{Library, Playlist, Track};
use crate::analysis::{Analysis, Beat, Cue};
use openfader_plugin::library as wire;
use std::collections::HashMap;

#[derive(Clone)]
pub struct Snapshot {
    /// Unique tracks (identified by file + title + artist, so duplicates share one TrackID) and their analysis from
    /// [`Library::analysis`]; `None`: the library has none.
    tracks: Vec<(Track, Option<Analysis>)>,
    /// Playlists without history; `members[i]` = 0-based indexes into `tracks` (empty for folders).
    pls: Vec<Playlist>,
    members: Vec<Vec<usize>>,
    /// Store-only data ([`Snapshot::add_store`]): track colour (1..=8) by index into `tracks`.
    colors: HashMap<usize, u8>,
    /// tag categories with their tag names, and `(track, category, tag)` assignments (indexes).
    mytags: Vec<(String, Vec<String>)>,
    tagged: Vec<(usize, usize, usize)>,
}

/// Identity of a track in the snapshot: duplicates (same file, title, artist) share one TrackID.
fn track_key(t: &Track) -> String {
    format!("{:?}\0{}\0{}", t.file, t.title, t.artist)
}

impl Snapshot {
    /// Tracks of the snapshot with the analysis the library gave (`None`: it has none).
    pub fn tracks(&self) -> &[(Track, Option<Analysis>)] {
        &self.tracks
    }

    /// The tracks, to be fitted to a format ([`crate::library::translate::fit_tracks`]).
    pub fn tracks_mut(&mut self) -> &mut [(Track, Option<Analysis>)] {
        &mut self.tracks
    }

    /// Playlists and folders in tree order with the indexes (into [`Snapshot::tracks`]) of their tracks.
    pub fn playlists(&self) -> impl Iterator<Item = (&Playlist, &[usize])> {
        self.pls.iter().zip(self.members.iter().map(Vec::as_slice))
    }

    /// Store track colour (1..=8) of track `i`, 0 when none.
    pub fn color(&self, i: usize) -> u8 {
        self.colors.get(&i).copied().unwrap_or(0)
    }

    /// tag categories with their tag names, and `(track index, category, tag)` assignments.
    pub fn mytags(&self) -> (&[(String, Vec<String>)], &[(usize, usize, usize)]) {
        (&self.mytags, &self.tagged)
    }

    /// Replaces the beat grid and cues of `t`'s entry with a session's (a deck's edited grid, hot / memory cues and saved
    /// loops). The track's BPM follows the new grid's tempo when that differs from the old grid's. `false` when `t` is not
    /// in the snapshot.
    pub fn override_session(&mut self, t: &Track, beats: Vec<Beat>, cues: Vec<Cue>) -> bool {
        let Some((track, slot)) = self
            .tracks
            .iter_mut()
            .find(|(x, _)| (&x.file, &x.title, &x.artist) == (&t.file, &t.title, &t.artist))
        else {
            return false;
        };
        let mut a = slot.take().unwrap_or_default();
        if let Some(b) = beats
            .first()
            .filter(|b| a.beats.first().map(|o| o.bpm_x100) != Some(b.bpm_x100))
        {
            track.bpm_x100 = b.bpm_x100.into();
        }
        (a.beats, a.cues) = (beats, cues);
        *slot = Some(a);
        true
    }

    /// The snapshot of `lib`: all its tracks, its playlists and their members.
    // ponytail: asks the library for every track's analysis here, on the caller's thread (a library that reads files for it
    // makes this slow for a big collection); `Library` is not `Send`, so it cannot move to the writing thread.
    pub fn take(lib: &dyn Library) -> Result<Self, String> {
        let (mut ids, mut tracks): (HashMap<String, usize>, Vec<(Track, Option<Analysis>)>) =
            Default::default();
        let mut intern = |t: Track| {
            *ids.entry(track_key(&t)).or_insert_with(|| {
                let a = lib.analysis(&t);
                tracks.push((t, a));
                tracks.len() - 1
            })
        };
        for t in lib.tracks(None)? {
            intern(t);
        }
        let pls: Vec<Playlist> = lib
            .playlists()?
            .into_iter()
            .filter(|p| !super::is_history(&p.id))
            .collect();
        let members = pls
            .iter()
            .map(|p| {
                if p.is_folder {
                    Ok(Vec::new())
                } else {
                    lib.tracks(Some(&p.id))
                        .map(|v| v.into_iter().map(&mut intern).collect())
                }
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            tracks,
            pls,
            members,
            colors: HashMap::new(),
            mytags: Vec::new(),
            tagged: Vec::new(),
        })
    }

    /// A snapshot of just these tracks, no playlists: what a device export refreshes on tracks it holds (their cues, grid,
    /// phrases), leaving its playlists alone.
    pub fn of_tracks(tracks: Vec<(Track, Option<Analysis>)>) -> Self {
        Self {
            tracks,
            pls: Vec::new(),
            members: Vec::new(),
            colors: HashMap::new(),
            mytags: Vec::new(),
            tagged: Vec::new(),
        }
    }

    /// The tracks with their analysis, giving up the snapshot.
    pub fn into_tracks(self) -> Vec<(Track, Option<Analysis>)> {
        self.tracks
    }

    /// `tracks` as the one playlist `name` inside the folders `folders` (outermost first) and nothing else: a device export merges
    /// a playlist by its folder path and name ([`crate::device::transfer`]).
    pub fn single_playlist(
        tracks: Vec<(Track, Option<Analysis>)>,
        folders: &[String],
        name: &str,
    ) -> Self {
        let mut s = Self::of_tracks(tracks);
        for (depth, n) in folders.iter().map(String::as_str).chain([name]).enumerate() {
            let is_folder = depth < folders.len();
            s.members.push(if is_folder {
                Vec::new()
            } else {
                (0..s.tracks.len()).collect()
            });
            s.pls.push(Playlist {
                id: format!("transfer:{depth}"),
                name: n.to_owned(),
                depth,
                is_folder,
            });
        }
        s
    }

    /// Points every track whose [`crate::store::key`] is in `copies` at that file instead (the copy a device holds), so the
    /// next format written to that device uses the audio already there.
    pub fn retarget(&mut self, copies: &HashMap<String, std::path::PathBuf>) {
        for (t, _) in &mut self.tracks {
            if let Some(f) = copies.get(&crate::store::key(t)) {
                t.file = Some(f.clone());
            }
        }
    }

    /// Number of tracks in the snapshot.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// The snapshot as a library plugin's collection writer takes it. Waveforms stay behind: a collection file holds
    /// beat grids, cues, key and phrases.
    pub fn to_wire(&self) -> wire::Snapshot {
        let slim = |a: &Analysis| Analysis {
            path: a.path.clone(),
            beats: a.beats.clone(),
            cues: a.cues.clone(),
            key: a.key.clone(),
            phrases: a.phrases.clone(),
            ..Analysis::default()
        };
        let mut colors: Vec<(usize, u8)> = self.colors.iter().map(|(&i, &c)| (i, c)).collect();
        colors.sort_unstable();
        wire::Snapshot {
            tracks: self
                .tracks
                .iter()
                .map(|(t, a)| (t.clone(), a.as_ref().map(slim)))
                .collect(),
            playlists: self.pls.clone(),
            members: self.members.clone(),
            colors,
            mytags: self.mytags.clone(),
            tagged: self.tagged.clone(),
        }
    }
}
