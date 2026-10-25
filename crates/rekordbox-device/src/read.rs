//! Reading the libraries of a device: the `export.pdb` Device Library ([`pdb`]) and the encrypted OneLibrary
//! ([`onelibrary`], `exportLibrary.db`). Both list the playlists (histories under the SDK's "Histories" folder, ids
//! `history:<id>`), the tracks with the absolute path of their analysis file as `analysis_ref`, and answer for that
//! analysis through [`rekordbox_common::anlz`].

mod onelibrary;
mod pdb;

use openfader_plugin::library::{Analysis, Probe, Source, Track};
use rekordbox_common::{anlz, onelibrary_key};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// The library files below a device root.
const PDB: &str = "PIONEER/rekordbox/export.pdb";
const ONE: &str = "PIONEER/rekordbox/exportLibrary.db";

/// A device root holding either library file (the score 90 leaves room for plugins that know better). A OneLibrary's key
/// starts deriving now, on a worker ([`onelibrary::warm`]).
pub fn probe(path: &Path) -> Option<Probe> {
    let one = path.join(ONE).is_file();
    if one {
        onelibrary::warm(path);
    }
    let kind = if path.join(PDB).is_file() {
        "USB (Device Library)"
    } else if one {
        "USB (OneLibrary)"
    } else {
        return None;
    };
    Some(Probe {
        score: 90,
        kind: kind.into(),
    })
}

/// How much older than `exportLibrary.db` an `export.pdb` may be and still come from the same export: FAT keeps times to 2 s,
/// and an export writes the two files one after the other.
const SAME_EXPORT: Duration = Duration::from_secs(10);

/// The `export.pdb` library when it is as new as the OneLibrary (one export writes both, with the same tracks, playlists and
/// cues): it needs no key derivation (about 70 ms). The OneLibrary (key: [`rekordbox_common::onelibrary_key`]) when it is the
/// only library, the newer one (a player wrote to it since), or `export.pdb` cannot be read.
pub fn open(path: &Path, settings: &Value) -> Result<Box<dyn Source>, String> {
    if path.join(ONE).is_file() {
        if same_export(path)
            && let Ok(pdb) = pdb::Export::open(path)
        {
            return Ok(Box::new(pdb));
        }
        Ok(Box::new(onelibrary::OneLibrary::open(
            path,
            &onelibrary_key(settings),
        )?))
    } else if path.join(PDB).is_file() {
        Ok(Box::new(pdb::Export::open(path)?))
    } else {
        Err(format!("{}: no device library", path.display()))
    }
}

/// Whether the `export.pdb` under `root` was written with or after its `exportLibrary.db` ([`SAME_EXPORT`]).
fn same_export(root: &Path) -> bool {
    let at = |f: &str| std::fs::metadata(root.join(f)).and_then(|m| m.modified()).ok();
    matches!((at(PDB), at(ONE)), (Some(pdb), Some(one)) if pdb + SAME_EXPORT >= one)
}

/// The analysis behind a track's `analysis_ref` (the absolute path of its `.DAT`).
pub fn analysis_ref(reference: &str) -> Option<Analysis> {
    anlz::load_track(Path::new(reference)).ok()
}

/// [`analysis_ref`] of `track`.
fn analysis(track: &Track) -> Option<Analysis> {
    analysis_ref(track.analysis_ref.as_deref()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What both libraries of the device at `root` list, one line each (playlists, all tracks, each list's tracks, each
    /// track's cues), as `export.pdb` and as the OneLibrary. A cover is compared by its bytes: rekordbox writes each picture
    /// twice, `a<n>.jpg` for the Device Library and `b<n>.jpg` for the OneLibrary.
    pub(super) fn listings(root: &Path) -> (Vec<String>, Vec<String>) {
        let cover = |mut v: Vec<Track>| {
            for t in &mut v {
                t.artwork = t.artwork.take().map(|p| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    std::fs::read(&p).unwrap().hash(&mut h);
                    format!("#{:x}", h.finish()).into()
                });
            }
            v
        };
        let list = |s: &dyn Source| {
            let lists = s.playlists().unwrap();
            let all = s.tracks(None).unwrap();
            let mut out = vec![format!("{lists:?}"), format!("{:?}", cover(all.clone()))];
            out.extend(lists.iter().map(|p| format!("{} {:?}", p.id, cover(s.tracks(Some(&p.id)).unwrap()))));
            out.extend(all.iter().map(|t| format!("{} {:?}", t.title, s.analysis(t).map(|a| a.cues))));
            out
        };
        let one = onelibrary::OneLibrary::open(root, &onelibrary_key(&json!({}))).unwrap();
        (list(&pdb::Export::open(root).unwrap()), list(&one))
    }

    /// A stick written by the exporter: three tracks with cues, a playlist in a folder, both libraries.
    fn stick(dir: &Path) -> std::path::PathBuf {
        use openfader_plugin::library::{Analysis, Beat, Cue, ExportOptions, Playlist, Snapshot};
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let data = vec![0u8; 4 * 44100];
        let mut w = [b"RIFF".as_slice(), &(36 + data.len() as u32).to_le_bytes(), b"WAVEfmt ", &16u32.to_le_bytes()].concat();
        w.extend([1, 0, 2, 0]);
        w.extend(44100u32.to_le_bytes());
        w.extend((44100u32 * 4).to_le_bytes());
        w.extend([4, 0, 16, 0]);
        w.extend(b"data");
        w.extend((data.len() as u32).to_le_bytes());
        w.extend(&data);
        let mut snap = Snapshot::default();
        for (i, title) in ["Ünïcødé", "Plain", "Zed"].iter().enumerate() {
            let file = dir.join(format!("{i}.wav"));
            std::fs::write(&file, &w).unwrap();
            let cue = |hot_cue, time_ms| Cue { hot_cue, time_ms, loop_end_ms: None, color: Some([40, 226, 20]), comment: None };
            snap.tracks.push((
                Track { title: (*title).into(), artist: "A".into(), bpm_x100: 12000, length_s: 1, file: Some(file), ..Track::default() },
                Some(Analysis {
                    beats: vec![Beat { bar_pos: 1, bpm_x100: 12000, time_ms: 250 }],
                    cues: vec![cue(1, 100 * i as u32), cue(0, 500)],
                    ..Analysis::default()
                }),
            ));
        }
        let list = |id: &str, name: &str, depth, is_folder| Playlist { id: id.into(), name: name.into(), depth, is_folder };
        snap.playlists = vec![list("sets", "Sets", 0, true), list("peak", "Peak", 1, false), list("all", "All", 0, false)];
        snap.members = vec![vec![], vec![2, 0], vec![0, 1, 2]];
        let root = dir.join("stick");
        rekordbox_export::export(&root, &snap, &ExportOptions::default(), &json!({})).unwrap();
        root
    }

    #[test]
    fn export_pdb_lists_what_the_onelibrary_lists_and_is_read_when_as_new() {
        let dir = std::env::temp_dir().join(format!("openfader-read-both-{}", std::process::id()));
        let root = stick(&dir);
        let (pdb, one) = listings(&root);
        assert_eq!(pdb, one);
        assert!(pdb.iter().any(|l| l.contains("time_ms: 500")), "the cues are compared");
        let kind = || open(&root, &json!({})).unwrap().kind();
        assert_eq!(kind(), "USB (Device Library)");
        // A player wrote to the OneLibrary since the export: it is the newer library.
        let later = std::fs::metadata(root.join(PDB)).unwrap().modified().unwrap() + Duration::from_secs(60);
        std::fs::File::options().write(true).open(root.join(ONE)).unwrap().set_modified(later).unwrap();
        assert_eq!(kind(), "USB (OneLibrary)");
        // export.pdb as new again but damaged: the OneLibrary.
        std::fs::File::options().write(true).open(root.join(PDB)).unwrap().set_modified(later).unwrap();
        assert_eq!(kind(), "USB (Device Library)");
        std::fs::write(root.join(PDB), b"x").unwrap();
        assert_eq!(kind(), "USB (OneLibrary)");
        let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
    }

    #[test]
    #[ignore = "needs OPENFADER_COMPARE=<device root>[:<device root>…]"]
    fn both_libraries_of_real_devices_list_the_same() {
        for root in std::env::var("OPENFADER_COMPARE").unwrap_or_default().split(':').filter(|r| !r.is_empty()) {
            let (pdb, one) = listings(Path::new(root));
            for (a, b) in pdb.iter().zip(&one) {
                if a != b {
                    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
                    let at = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
                    let s = |v: &[char]| v[at.saturating_sub(150)..(at + 150).min(v.len())].iter().collect::<String>();
                    println!("{root}:\n PDB …{}\n ONE …{}", s(&a), s(&b));
                }
            }
            assert_eq!(pdb, one, "{root}");
        }
    }

    #[test]
    fn probe_and_open_pick_the_library() {
        let dir = std::env::temp_dir().join(format!("openfader-read-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("PIONEER/rekordbox")).unwrap();
        assert!(probe(&dir).is_none() && open(&dir, &json!({})).is_err());
        std::fs::write(dir.join(ONE), b"x").unwrap();
        assert_eq!(probe(&dir).unwrap().kind, "USB (OneLibrary)");
        // garbage under the built-in key: an error when it is read, not a panic
        assert!(open(&dir, &json!({})).and_then(|s| s.tracks(None)).is_err());
        std::fs::write(dir.join(PDB), b"x").unwrap();
        let p = probe(&dir).unwrap();
        assert_eq!((p.score, p.kind.as_str()), (90, "USB (Device Library)"));
        std::fs::remove_file(dir.join(ONE)).unwrap();
        assert!(open(&dir, &json!({})).is_err()); // export.pdb is no database either: the error, not a panic
        let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
    }
}
