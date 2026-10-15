//! Finding files that moved (the Missing File Manager): by file name inside folders the user listed
//! ("Auto Relocate Search Folders": matches on the file name), or next to a file the user picked for a
//! sibling track (several selected tracks relocate to the folder of the first one's chosen file).
//! File names are compared case-insensitively (libraries often come from Windows); of several matches the one whose
//! path shares the most trailing components with the lost path wins.

use super::walk_files;
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
}
