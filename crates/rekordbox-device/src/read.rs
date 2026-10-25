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

/// The library files below a device root.
const PDB: &str = "PIONEER/rekordbox/export.pdb";
const ONE: &str = "PIONEER/rekordbox/exportLibrary.db";

/// A device root holding either library file (the score 90 leaves room for plugins that know better).
pub fn probe(path: &Path) -> Option<Probe> {
    let kind = if path.join(PDB).is_file() {
        "USB (Device Library)"
    } else if path.join(ONE).is_file() {
        "USB (OneLibrary)"
    } else {
        return None;
    };
    Some(Probe {
        score: 90,
        kind: kind.into(),
    })
}

/// The OneLibrary when it is there (key: [`rekordbox_common::onelibrary_key`]), else the `export.pdb` library.
pub fn open(path: &Path, settings: &Value) -> Result<Box<dyn Source>, String> {
    if path.join(ONE).is_file() {
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
