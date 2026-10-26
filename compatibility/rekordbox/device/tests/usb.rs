//! Runs against a real Device Library (`export.pdb`) named by `OPENFADER_USB`; skips otherwise.

mod common;

use openfader_plugin::library::Track;
use compat_rekordbox_common::list::HISTORY;
use compat_rekordbox_common::pdb::{rows, u32_at};
use std::collections::HashSet;
use std::path::Path;

fn audio_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            if p.is_dir() {
                audio_files(&p)
            } else {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                ["mp3", "m4a", "wav", "aif", "aiff", "flac"].contains(&ext.as_str()) as usize
            }
        })
        .sum()
}

#[test]
fn export_pdb() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let e = common::open(&root, None);
    assert_eq!(e.kind(), "USB (Device Library)");
    let tracks = e.tracks(None).unwrap();
    assert_eq!(tracks.len(), audio_files(&root.join("Contents")));
    for t in &tracks {
        assert!(t.file.as_ref().unwrap().exists());
        assert!(Path::new(t.analysis_ref.as_ref().unwrap()).exists());
        assert!(t.bpm_x100 > 0);
        assert!(t.rating <= 5);
        assert!(
            t.bitrate > 0 && t.sample_rate > 0,
            "bitrate/sample rate missing"
        );
    }
    // The table sizes straight from the file, through the shared byte readers.
    let d = std::fs::read(root.join("PIONEER/rekordbox/export.pdb")).unwrap();
    let n = |ty| rows(&d, ty).unwrap();
    assert_eq!(n(0).len(), tracks.len());
    // Artwork: a track's artwork_id that names an artwork row must resolve to an existing JPEG.
    let art_ids: HashSet<u32> = n(13).into_iter().map(|r| u32_at(&d, r).unwrap()).collect();
    let artwork_refs = n(0)
        .into_iter()
        .filter(|&r| art_ids.contains(&u32_at(&d, r + 0x1c).unwrap()))
        .count();
    let with_art: Vec<_> = tracks.iter().filter_map(|t| t.artwork.as_ref()).collect();
    assert_eq!(
        with_art.len(),
        artwork_refs,
        "artwork rows pointing at missing files"
    );
    if with_art.is_empty() {
        eprintln!("NOTE: this USB has no artwork; artwork checks ran on zero tracks");
    }
    for p in with_art {
        assert!(p.is_absolute() && p.is_file(), "{}", p.display());
        assert_eq!(
            std::fs::read(p).unwrap()[..2],
            [0xff, 0xd8],
            "not a JPEG: {}",
            p.display()
        );
    }
    eprintln!(
        "artwork={} commented={} rated={}",
        tracks.iter().filter(|t| t.artwork.is_some()).count(),
        tracks.iter().filter(|t| !t.comment.is_empty()).count(),
        tracks.iter().filter(|t| t.rating > 0).count()
    );

    let all_lists = e.playlists().unwrap();
    let is_history = |id: &str| id.starts_with(HISTORY);
    let histories = all_lists
        .iter()
        .filter(|p| is_history(&p.id) && !p.is_folder)
        .count();
    eprintln!(
        "histories: {histories} playlists{}",
        if histories == 0 {
            " — NOTE: this USB has no history playlists"
        } else {
            ""
        }
    );
    let pls: Vec<_> = all_lists
        .into_iter()
        .filter(|p| !is_history(&p.id))
        .collect();
    let mut prev: Option<(usize, bool)> = None;
    for p in &pls {
        match prev {
            None => assert_eq!(p.depth, 0),
            Some((d, folder)) => {
                assert!(p.depth <= d + 1);
                if p.depth == d + 1 {
                    assert!(folder, "child of a non-folder");
                }
            }
        }
        prev = Some((p.depth, p.is_folder));
    }
    let (n_tree, n_entries) = (n(7).len(), n(8).len());
    assert_eq!(pls.len(), n_tree, "unreachable playlist rows");
    // Every entry resolves to an existing track in an existing playlist.
    let resolved: usize = pls
        .iter()
        .filter(|p| !p.is_folder)
        .map(|p| e.tracks(Some(&p.id)).unwrap().len())
        .sum();
    assert_eq!(resolved, n_entries);
    let folders = pls.iter().filter(|p| p.is_folder).count();
    eprintln!(
        "tracks={} artists={} playlists={} folders={folders} entries={n_entries}",
        tracks.len(),
        n(2).len(),
        pls.len() - folders
    );
}

/// Lowercased letters and digits only.
fn norm(s: &str) -> Vec<char> {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The file stem is "Artist - Title", cut to a fixed length (observed: ~44 chars), so the
/// title must occur in it, or the stem must end inside the title (a prefix of 3+ characters).
/// A title with control characters is mangled in the source data itself (low-byte Cyrillic), so
/// the mismatch count must equal the number of such titles; this guards the pdb string decoders.
#[test]
fn titles_match_file_names() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let tracks = common::open(&root, None).tracks(None).unwrap();
    let mismatched = |t: &&Track| {
        let (title, stem) = (
            norm(&t.title),
            norm(
                &t.file
                    .as_ref()
                    .unwrap()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy(),
            ),
        );
        !(title.is_empty()
            || stem.windows(title.len()).any(|w| w == title)
            || (3..title.len()).any(|k| stem.ends_with(&title[..k])))
    };
    let mismatches = tracks.iter().filter(mismatched).count();
    let garbled = tracks
        .iter()
        .filter(|t| t.title.chars().any(char::is_control))
        .count();
    assert_eq!(
        mismatches, garbled,
        "title/file-name mismatches vs titles with control characters"
    );
}
