//! Serato DJ library: read from a computer or a USB stick or SD card, written to a stick or card.
//!
//! | Where (below the drive root) | What |
//! |---|---|
//! | `_Serato_/database V2` | one `otrk` record per track, merged into the file ([`database`]) |
//! | `_Serato_/Subcrates/<parent>%%<child>.crate` | one crate per playlist ([`crate_file`]) |
//! | the shared audio copy (`Contents/<artist>/<album>/<file>`) | `Serato Markers2` and `Serato BeatGrid` tags, written in place when they fit ([`tag`]) |
//! | `_Serato_/Audio/<artist>/<album>/<file>` | a private copy of a track whose tags do not fit the shared copy |
//!
//! Serato keeps cues, loops, track colour and beat grid only in the audio file's tags. Other software on the same stick finds a
//! track by file name and size, so a shared copy never changes size: the tags are rewritten into the padding the file has, and
//! a track that has no room gets a private copy that is tagged freely (and is the one the database and crates point at from then on).
//!
//! # What an export guarantees
//!
//! A stick is never left empty or half written without the app having seen it before the set.
//!
//! - **Phases.** "Copying audio" (the shared copies), "Writing analysis files" (Serato's analysis is the tags: the tag edits and
//!   the private copies), "Writing the library" (`database V2` and the crates are staged), "Checking the library" (read back),
//!   then the switch. The host is told the bytes of the whole job up front (audio, private copies, tags and an estimate of the
//!   library files) and every byte written; a cancel is seen at every track and at every progress report of a copy (the SDK
//!   sends those at most every 200 ms). Once the switch has begun it cannot be cancelled.
//! - **All or nothing.** `database V2` and every crate the export changes are staged next to the files they replace
//!   (`<name>.ofnew`, flushed) in ONE [`commit`](openfader_plugin::library::commit): they are renamed over the old ones
//!   together, with a journal (`/.openfader/journal.json`) that lets the host finish an interrupted switch, so the stick holds
//!   the whole old library or the whole new one. A crate that would be written as it is already is not staged; a folder gets an
//!   empty crate only when it has none; crates the export does not name are not deleted. Audio and tags cannot sit in the
//!   commit (they are the audio files themselves), so they are written first and undone on a failure.
//! - **A cancel or an error before the switch leaves the stick byte-identical.** The run records every file and folder it made
//!   (audio copies and their folders, private copies, `_Serato_` and the host's `.openfader` folder when they were not there)
//!   and removes them again newest first. A tag edit of a file the stick held before (a shared copy rewritten in place, or a
//!   private copy refreshed) keeps the bytes the edit replaced and, on a failure, writes them back and restores the file's
//!   time; only the window of bytes that differ is kept. A pull-out between the first tag edit and the switch cannot run that
//!   undo: the stick then holds new tags next to the old database, which is harmless (the tags are read for the tracks the old
//!   database lists, the file size never changed, and the next export or check starts from there). After the switch has begun
//!   the new library is the one in use: a failure then is finished by the host's recovery and nothing is undone.
//! - **Read back.** Before the switch the staged files are read with the plugin's own parsers ([`database`], [`crate_file`],
//!   [`tag::read`]): the database lists exactly the old records (less the replaced paths) and the new ones, every written
//!   record has the title, artist and album that was written and its audio file is there, every staged crate holds its members,
//!   and the tags of each track written give back the cues and loops that were encoded (and a beat grid when one was written).
//!   A difference is an error and the stick is left as it was; success sets `ExportReport::verified`.
//! - **Dry run.** `ExportOptions::dry_run` runs the same planning and answers `ExportReport::plan`: tracks to add or update (a
//!   track whose library path the database holds is an update), the audio bytes to copy (shared copies and private copies),
//!   an estimate of the other bytes (tags, the new database, the changed crates), the files created or rewritten, and what
//!   stops it or leaves tracks out (`blocked`). The host analyses nothing for Serato, so `analyse` is 0. Nothing is created,
//!   touched or recovered; the stick is only read. A real export with `free` too small, or over an unreadable `database V2`,
//!   returns the blocked text as its error and writes nothing.
//! - **File system limits.** A track over `max_file`, or whose file or folder name is reserved on FAT (`CON`, `NUL`, `COM1` …),
//!   is left out whole with a warning, never copied in part; a track whose tagged private copy would be over `max_file` keeps its
//!   shared copy untouched and is listed without cues. Names are cleaned by the SDK's `audio_slot`.
//!
//! # The checker
//!
//! `device_check` reads `_Serato_/database V2` (readable when it starts with `vrsn` and parses), counts its tracks, the crates
//! that are playlists (a crate that only holds folders below it is not), and the cues and loops in the tags of the first
//! [`check::CUE_SAMPLE`] tracks (tags live in the audio files, one read each). It never writes and never recovers. Problems:
//! the library cannot be read (no fix: Serato keeps no backup copy of the file), audio the database lists is missing from the
//! stick (no fix: the host copies it again from the collection), crates that cannot be read or name tracks the database does
//! not hold (no fix: a crate is the user's own list), leftover `*.ofnew` files without a journal (fix `serato:clean_leftovers`,
//! which only removes them) and a pending journal of this plugin (the host finishes it when the stick shows up).
//!
//! # Inferred
//!
//! [inferred] Serato DJ accepts the library written this way: the layouts come from files Serato wrote and from Mixxx's reader,
//! and nothing was tried on a real Serato install. [inferred] The staged-then-renamed `database V2` is picked up like any other
//! write of the file. [inferred] Players and Serato itself ignore `*.ofnew` and `*.part` files next to the library and audio.
//! The cue count of the checker covers only the sampled tracks.
//!
//! Format sources: <https://github.com/Holzhaus/serato-tags> (tags, colours), Mixxx's `src/library/serato/` and
//! `src/track/serato/` (database and crates); the crate headers and text shapes are observed in files Serato wrote.
//! The reader (`reader.rs`) takes the same files back: the library of a computer or of a stick.

pub mod check;
pub mod crate_file;
pub mod database;
mod export;
mod reader;
pub mod tag;
mod tlv;
mod write;

use openfader_plugin::library::{
    ActionAnswer, Capabilities, CheckReport, DeviceInfo, ExportOptions, ExportReport, Feature,
    LibraryPlugin, Probe, Snapshot, Source,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Name of the library in [`DeviceInfo::libraries`].
pub(crate) const LIBRARY: &str = "Serato library";
/// The plugin's name: the owner of its journals.
pub(crate) const OWNER: &str = "serato";

pub struct Plugin;

impl LibraryPlugin for Plugin {
    fn probe(path: &Path) -> Option<Probe> {
        reader::probe(path)
    }

    fn open(path: &Path, _settings: &Value) -> Result<Box<dyn Source>, String> {
        reader::open(path)
    }

    fn analysis_ref(
        reference: &str,
        _settings: &Value,
    ) -> Option<openfader_plugin::library::Analysis> {
        reader::analysis_ref(reference)
    }

    fn capabilities(_settings: &Value) -> Capabilities {
        reader::capabilities()
    }

    fn device_probe(root: &Path, _settings: &Value) -> Result<DeviceInfo, String> {
        let db = std::fs::metadata(root.join(database::FILE))
            .ok()
            .filter(|m| m.is_file());
        Ok(DeviceInfo {
            libraries: if db.is_some() {
                vec![LIBRARY.into()]
            } else {
                vec![]
            },
            title: "Serato DJ".into(),
            about: "Serato DJ Pro and Serato DJ Lite on a computer; the cues travel inside the audio files".into(),
            can_write: root.is_dir(),
            features: vec![Feature::Export, Feature::Plan, Feature::Check],
            modified: db
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64),
            ..DeviceInfo::default()
        })
    }

    fn export(
        root: &Path,
        snapshot: &Snapshot,
        options: &ExportOptions,
        _settings: &Value,
    ) -> Result<ExportReport, String> {
        export::run(root, snapshot, options)
    }

    fn device_check(
        root: &Path,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<CheckReport, String> {
        check::check(root)
    }

    fn device_fix(
        root: &Path,
        fix: &str,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<ActionAnswer, String> {
        check::fix(root, fix)
    }
}
