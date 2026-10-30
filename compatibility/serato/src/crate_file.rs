//! `_Serato_/Subcrates/<parent>%%<child>.crate`: one file per playlist, the folders above it joined with `%%`. The file is
//! `vrsn` (version text), `osrt` (sort column), `ovct` (one per visible column) and one `otrk` per member holding a `ptrk` path
//! (relative to the drive root, like `pfil` in the database). Record layout: Mixxx's reader (see [`crate::tlv`]); the header
//! records are observed in crates Serato wrote. A folder gets an empty crate of its name only when none exists, so the nesting
//! shows without touching a crate the user filled.

use crate::tlv::{self, rec, text};
use openfader_plugin::library::{clean_name, Playlist};

/// Folder of the crate files below the drive root.
pub const DIR: &str = "_Serato_/Subcrates";
const VERSION: &str = "1.0/Serato ScratchLive Crate";
const COLUMNS: [&str; 6] = ["song", "artist", "bpm", "key", "album", "length"];

/// The crate name (file stem) of each playlist of the tree: its own name below its folders', joined with `%%`.
pub fn names(playlists: &[Playlist]) -> Vec<String> {
    let mut stack: Vec<String> = Vec::new();
    playlists
        .iter()
        .map(|p| {
            stack.truncate(p.depth);
            stack.push(clean_name(&p.name, "Crate").replace('%', "_"));
            stack.join("%%")
        })
        .collect()
}

/// A crate listing `paths`.
pub fn build(paths: &[&str]) -> Vec<u8> {
    let mut v = text(b"vrsn", VERSION);
    v.extend(rec(
        b"osrt",
        &[text(b"tvcn", "song"), rec(b"brev", &[0])].concat(),
    ));
    for c in COLUMNS {
        v.extend(rec(
            b"ovct",
            &[text(b"tvcn", c), text(b"tvcw", "0")].concat(),
        ));
    }
    for p in paths {
        v.extend(rec(b"otrk", &text(b"ptrk", p)));
    }
    v
}

/// The member paths of a crate file, in order.
pub fn paths(buf: &[u8]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for r in tlv::parse(buf)?.iter().filter(|r| &r.tag == b"otrk") {
        if let Some(p) = tlv::find(&tlv::parse(r.data)?, b"ptrk") {
            out.push(tlv::from_utf16(p));
        }
    }
    Ok(out)
}
