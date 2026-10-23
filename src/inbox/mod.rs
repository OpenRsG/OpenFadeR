//! INBOX: bringing new music into the Collection in steps. Our own design.
//!
//! Folders the user watches (Downloads) are scanned while the app runs ([`scan`]): a new audio file, or a zip holding audio
//! (unpacked beside the zip), joins the first stage, INBOX, as a Collection row waiting there ([`crate::store`] `inbox`), so it
//! has an id, edits and analysis from the start. A file the Collection has already (same path, or the same audio
//! fingerprint) is not added again: it is linked to the row it duplicates. The user moves tracks on, INBOX → REVIEW →
//! COLLECTION (stages can be renamed and added), by drag or action ([`move_tracks`]); with [`Config::move_files`] the file moves
//! too, into `<root>/<stage name>/<pattern>` ([`rules`]), never over another file. Every move is one batch in the change
//! journal: Edit > Undo, or [`undo_batch`] for an older one, moves the files back and restores stage and path.
//!
//! Children: [`scan`] (the watched folders), [`rules`] (where a file goes), [`moves`] (a batch of moves and its undo).

mod moves;
mod rules;
mod scan;

pub use moves::{move_tracks, undo_batch, Report};
pub use rules::target;
pub use scan::{scan, Scan, SETTLE};

use std::path::PathBuf;

/// The default of [`Config::pattern`]: artist and album folders, the file name kept.
pub const DEFAULT_PATTERN: &str = "{artist}/{album}/{file}";

/// What the user set (Preferences > Advanced > Database > Inbox).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Folders watched for new music.
    pub folders: Vec<PathBuf>,
    /// A move carries the file to its stage's folder under `root`.
    pub move_files: bool,
    /// The library root: each stage has a folder of its name under it.
    pub root: PathBuf,
    /// The path under the stage's folder, `/`-separated: `{artist} {album} {title} {genre} {label}` of the track and
    /// `{file}` `{name}` `{ext}` of its file (see [`rules`]).
    pub pattern: String,
}
