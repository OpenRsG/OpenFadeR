//! Finding files that moved (the Missing File Manager): by file name inside folders the user listed
//! ("Auto Relocate Search Folders": matches on the file name), by audio ([`ByAudio`]: a renamed file, found by its
//! fingerprint), or next to a file the user picked for a sibling track (several selected tracks relocate to the folder of
//! the first one's chosen file).
//! File names are compared case-insensitively (libraries often come from Windows); of several matches the one whose
//! path shares the most trailing components with the lost path wins.

use super::{folder, walk_files, Track};
use crate::{analysis::fingerprint, audio::decode, store::Store};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Directories nested deeper than this are not searched.
const MAX_DEPTH: usize = 24;

/// Every file below the folders, by lower-cased file name.
pub type Index = HashMap<String, Vec<PathBuf>>;

fn name(p: &Path) -> Option<String> {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase())
}

/// Walks `folders` (symlinked directories not followed) and indexes every file by name.
pub fn index(folders: &[PathBuf]) -> Index {
    let mut out = Index::new();
    for f in folders {
        walk_files(f, MAX_DEPTH, |p| {
            out.entry(name(&p).unwrap_or_default()).or_default().push(p)
        });
    }
    out
}

fn shared_tail(a: &Path, b: &Path) -> usize {
    let tail = |p: &Path| {
        p.components()
            .rev()
            .filter(|c| matches!(c, Component::Normal(_)))
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
    };
    tail(a)
        .iter()
        .zip(tail(b))
        .take_while(|(x, y)| **x == *y)
        .count()
}

/// The indexed file that is `lost` under a new place, if any.
pub fn find<'a>(index: &'a Index, lost: &Path) -> Option<&'a Path> {
    let cands = index.get(&name(lost)?)?;
    cands
        .iter()
        .max_by_key(|c| shared_tail(c, lost))
        .map(PathBuf::as_path)
}

/// `lost`'s file name in the folder of `chosen`, when such a file exists.
pub fn beside(chosen: &Path, lost: &Path) -> Option<PathBuf> {
    let p = chosen.parent()?.join(lost.file_name()?);
    p.is_file().then_some(p)
}

/// Candidates whose tags one [`ByAudio`] reads (the cheap length check before decoding), over all the tracks it looks for.
const MAX_TAG_READS: usize = 5000;
/// Candidates decoded and fingerprinted per lost track: the same extension, a tag length within [`LENGTH_SLACK_S`] and not
/// the file of another track in the Collection.
pub const MAX_DECODES: usize = 8;
/// How far a candidate's tag length may be from the lost track's (a lossy re-encode of the audio changes it a little).
const LENGTH_SLACK_S: i64 = 2;

/// Finds the files of lost tracks by their audio when no file of the same name exists: candidates in the search folders with
/// the lost file's extension and a tag length within [`LENGTH_SLACK_S`] are decoded (at most [`MAX_DECODES`] per lost track)
/// and the one whose fingerprint ([`fingerprint::of`]) equals the track's `collection.fingerprint` is the track. A track
/// whose audio was never analysed has no fingerprint and is not found this way. Lengths and fingerprints of candidates are
/// kept, so a candidate is read and decoded once for all the tracks of one search.
pub struct ByAudio<'a> {
    index: &'a Index,
    tags_left: usize,
    lengths: HashMap<&'a Path, i64>,
    prints: HashMap<&'a Path, String>,
}

impl<'a> ByAudio<'a> {
    pub fn new(index: &'a Index) -> Self {
        Self {
            index,
            tags_left: MAX_TAG_READS,
            lengths: HashMap::new(),
            prints: HashMap::new(),
        }
    }

    /// The indexed file with the audio of `lost`, if any.
    pub fn find(&mut self, store: &Store, lost: &Track) -> Option<&'a Path> {
        let want = store.fingerprint(lost.id?).ok()??;
        let ext = lost.file.as_deref()?.extension()?.to_str()?;
        let mut decodes = 0;
        for c in self.index.values().flatten().map(PathBuf::as_path) {
            if !c
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
            {
                continue;
            }
            let len = match self.lengths.get(c) {
                Some(&l) => l,
                None if self.tags_left == 0 => continue,
                None => {
                    self.tags_left -= 1;
                    let l = folder::read_track(c).length_s;
                    self.lengths.insert(c, l);
                    l
                }
            };
            if lost.length_s > 0 && (len - lost.length_s).abs() > LENGTH_SLACK_S {
                continue;
            }
            if store.id_of_path(c).ok().flatten().is_some() {
                continue; // another track's file
            }
            if decodes == MAX_DECODES {
                break;
            }
            decodes += 1;
            let print = self
                .prints
                .entry(c)
                .or_insert_with(|| decode(c).map(|p| fingerprint::of(&p)).unwrap_or_default());
            if *print == want {
                return Some(c);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_name_preferring_the_closest_path_and_ignoring_case() {
        let root = std::env::temp_dir().join(format!("openfader-reloc-{}", std::process::id()));
        for d in ["a/Album", "b/Other", "b/Album"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in [
            "a/Album/Song.mp3",
            "b/Other/song.MP3",
            "b/Album/Song.mp3",
            "b/Album/x.wav",
        ] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        let idx = index(&[root.join("b")]);
        assert_eq!(idx.values().map(Vec::len).sum::<usize>(), 3); // `a/` was not searched
        let lost = Path::new("/gone/Album/SONG.mp3");
        assert_eq!(
            find(&idx, lost),
            Some(root.join("b/Album/Song.mp3").as_path())
        );
        assert_eq!(find(&idx, Path::new("/gone/none.mp3")), None);
        assert_eq!(
            beside(&root.join("b/Album/Song.mp3"), Path::new("/gone/x.wav")),
            Some(root.join("b/Album/x.wav"))
        );
        assert_eq!(
            beside(&root.join("b/Album/Song.mp3"), Path::new("/gone/y.wav")),
            None
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A mono 16-bit WAV of `secs` seconds of `hz` at 8 kHz.
    fn wav(path: &Path, hz: f32, secs: u32) {
        let rate = 8000u32;
        let data: Vec<u8> = (0..rate * secs)
            .flat_map(|i| {
                (((i as f32 / rate as f32 * hz * 6.2831855).sin() * 12000.0) as i16).to_le_bytes()
            })
            .collect();
        let mut w = b"RIFF".to_vec();
        w.extend((36 + data.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend([1, 0, 1, 0]); // PCM, mono
        w.extend(rate.to_le_bytes());
        w.extend((rate * 2).to_le_bytes());
        w.extend([2, 0, 16, 0]);
        w.extend(b"data");
        w.extend((data.len() as u32).to_le_bytes());
        w.extend(data);
        std::fs::write(path, w).unwrap();
    }

    #[test]
    fn a_renamed_file_is_found_by_its_audio_among_decoys() {
        let root =
            std::env::temp_dir().join(format!("openfader-reloc-audio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::create_dir_all(root.join("new")).unwrap();
        let lost_file = root.join("old/song.wav");
        wav(&lost_file, 440.0, 3);
        let store = Store::in_memory().unwrap();
        let mut lost = folder::read_track(&lost_file);
        let id = store.collection_upsert(&lost, None).unwrap();
        lost.id = Some(id);
        store
            .set_fingerprint(id, &fingerprint::of(&decode(&lost_file).unwrap()))
            .unwrap();
        std::fs::remove_file(&lost_file).unwrap();
        wav(&root.join("new/decoy.wav"), 523.0, 3); // same length, other audio
        wav(&root.join("new/short.wav"), 440.0, 8); // same audio, another length: not a candidate
        wav(&root.join("new/renamed.wav"), 440.0, 3);
        let idx = index(&[root.join("new")]);
        assert_eq!(find(&idx, &lost_file), None); // no file of that name
        let found = ByAudio::new(&idx).find(&store, &lost);
        assert_eq!(found, Some(root.join("new/renamed.wav").as_path()));
        // No fingerprint, no match; a file the Collection lists is another track's.
        let plain = Track {
            id: Some(id + 1),
            ..lost.clone()
        };
        assert_eq!(ByAudio::new(&idx).find(&store, &plain), None);
        store
            .collection_put(
                &[folder::read_track(&root.join("new").join("renamed.wav"))],
                false,
            )
            .unwrap();
        assert_eq!(ByAudio::new(&idx).find(&store, &lost), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
