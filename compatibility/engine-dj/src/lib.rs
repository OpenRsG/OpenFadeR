//! Engine DJ library: `Engine Library/Database2/m.db`, the database Denon DJ (Prime, SC6000, SC Live), Numark and RANE standalone
//! players and Engine DJ desktop read. `probe` and `open` read it (desktop `~/Music/Engine Library` or a device, [`reader`]);
//! `export` merges the snapshot into the library of the device and makes one when there is none.
//!
//! # What is written where
//!
//! | Where | What |
//! |---|---|
//! | `Contents/<artist>/<album>/<file>` | the audio, one copy shared with every other format on the stick (`place_audio`) |
//! | `Engine Library/Database2/m.db`, `Track` | one row per track: metadata, key, rating, `path` = `../<audio path>` |
//! | `m.db`, `PerformanceData` | per track: `trackData`, `overviewWaveFormData`, `beatData`, `quickCues`, `loops` ([`blob`]); a table in 3.x, a view over `Track` in 2.x |
//! | `m.db`, `Playlist`, `PlaylistEntity` | the folder and list tree ([`playlists`]) |
//!
//! A track is identified by its `path`: a library track found there is refreshed (metadata, cues, loops, grid, waveform) and
//! never deleted, any other is added. A list that exists by name path gets the snapshot's tracks, other lists stay.
//!
//! # What an export guarantees
//!
//! * **All or nothing.** The new `m.db` is built at `m.db.ofnew` beside the old one (`Commit::staged_path`; a copy made with
//!   `VACUUM INTO` or a new file, journal mode DELETE, so it needs no `-wal`/`-shm`) and switched in by one `Commit` of the SDK,
//!   journal `.openfader/journal.json`. A pulled stick leaves the whole old library or, once the host has recovered the
//!   journal, the whole new one. The old file's `-wal`/`-shm` are removed after the switch; a crash between the two leaves them
//!   and `device_check` reports them (they are older than `m.db`).
//! * **Cancel and errors** (`Err("cancelled")` included) before the commit leave the device byte-identical: the audio copied,
//!   the folders made, the staged file and the host's `.openfader` folder are removed again (`export::Writer`). An existing
//!   library is never modified in place.
//! * **Read-back.** Before the switch the staged file passes `PRAGMA integrity_check` and is opened with this plugin's own
//!   [`reader`]: the track count, the tracks of every list written (in order) and the cues, hot cues and loops of every track
//!   that got performance data must be what was written, else the export fails and the old library stays.
//!   `ExportReport::verified` is then `Some(true)`.
//! * **Dry run** (`ExportOptions::dry_run`, `Feature::Plan`): the same planning code reads the device and the snapshot and
//!   answers an `ExportPlan` (tracks to add or update by `Track.path`, audio bytes, an estimate of the library bytes, files,
//!   what blocks the export); it writes nothing, not a folder. The existing `m.db` is opened read-only and `immutable` so
//!   SQLite makes no `-wal`, `-shm` or journal and touches no time stamp; only a `-wal` a player left behind (changes
//!   `immutable` would ignore) makes it read a copy in the system's temp folder.
//! * **Room and size.** An export whose audio and library do not fit `ExportOptions::free` stops before anything is written,
//!   with the sentence a dry run lists in `blocked`. A track over `ExportOptions::max_file`, or with a name FAT cannot hold
//!   (`CON.wav`), is left out whole with a warning and a `blocked` line.
//! * **Progress.** Phases "Copying audio", "Writing the library", "Checking the library" (this format has no analysis files: the
//!   analysis lives in the database), bytes counted for the audio and the library file.
//!
//! # Checking a stick
//!
//! `device_check` (read-only, `immutable`, nothing recovered) answers the library `Engine Library` (present, readable, tracks,
//! lists that are not folders, cues = hot cues, memory cues and loops read by [`reader`]) and these problems: unreadable
//! (not a database, not an Engine library, unknown schema, the old two-file layout; severity Unreadable), `PRAGMA quick_check`
//! findings (Error), audio files the library lists that the stick lacks (Error, `files`; no fix here, the host copies them again
//! from the collection), `m.db.ofnew` without a journal (Info, fix `engine-dj:clean_leftovers`), `-wal`/`-shm` files older than
//! `m.db` (Warning, fix `engine-dj:remove_sidecars`), a journal of this plugin (Warning, fix `engine-dj:finish_change` runs the
//! recovery). The fixes only delete those files or run the recovery; none rewrites the library. [inferred: a `-wal` older than
//! the database it sits next to is stale; SQLite itself ignores one whose salt does not match, the check is the cautious side.]
//!
//! # Format version: schema 3.0.0, or 2.18.0 for older Engine
//!
//! libdjinterop (`to_application_version_string`) names the Engine DJ Desktop/OS releases that write each schema version of
//! `Database2/m.db`; the last column is what this plugin does with it:
//!
//! | Schema | Written by Engine DJ Desktop and OS | Here |
//! |---|---|---|
//! | 2.18.0 | 2.0.x to 2.1.x | new library with the option `engine_dj.schema` = `2.18.0`; extended |
//! | 2.20.1 to 2.20.3 | 2.2.x to 3.0.1 | extended |
//! | 2.21.0 to 2.21.2 | 3.1.0 to 4.0.1 | extended |
//! | 3.0.0 | 4.1.0 to 4.2.1 | new library by default (`engine_dj.schema` = `3.0.0`); extended |
//! | 3.0.1 | 4.3.x | extended |
//! | 3.0.2 | 4.5.0, 5.0.x | extended |
//!
//! An existing `m.db` of one of these versions is extended in place and keeps its version: the rows written are valid for each
//! (3.0.1 only changes triggers, 3.0.2 adds the nullable `Track.albumArtSourceHash` which stays NULL, 2.20.3 and later have
//! `Track.lastEditTime`, 2.18.0 to 2.20.2 do not). The option only decides the schema of a new library and converts nothing: on
//! a stick that holds one, a report note says that the version found was kept. Any other version (1.x, a release newer than 3.0.2)
//! is refused with the version found and nothing is changed, as is a library in the old two-file layout (`Engine Library/m.db`).
//!
//! A new library is 3.0.0, the oldest schema of the 3.x line: the later releases (4.3, 4.5, 5.x) still carry it, read it and
//! upgrade it [inferred]. 3.0.0 has the separate `PerformanceData` table, so one `UPDATE` of one row carries every analysis blob.
//! For Engine DJ and players older than 4.1 (they refuse 3.x) the option gives 2.18.0, the oldest `Database2` schema (Engine
//! 2.0.x); it is the schema that reaches back furthest, and the releases up to 4.0.1 are expected to read an older schema and
//! upgrade it [inferred: libdjinterop reads and writes each version but says nothing about upgrades, and no library of
//! this version was tried on a player]. 2.x keeps the analysis blobs in `Track`; `PerformanceData` is a view over `Track` with an
//! `INSTEAD OF UPDATE` trigger per column, so the `UPDATE PerformanceData` used for 3.x reaches the same columns
//! ([`schema_2`] has the DDL). The blob layouts are the same in both lines (libdjinterop shares `engine/v2/*_blob.cpp`). Only `m.db`
//! is made: libdjinterop writes no other file for these schemas (the history and streaming databases are the players' own)
//! [inferred from libdjinterop, not tried on hardware]. Schema 1.x (Engine Prime and Engine OS 1.x, the two-file layout) is not
//! written.
//!
//! # Conventions
//!
//! * `Track.path` is relative to the `Engine Library` folder: `../Contents/...` for audio at the root of the stick
//!   (libdjinterop `track_table.hpp`: "relative path from the location of the Engine library to the music file").
//! * Positions are samples (`f64`): `time_ms * rate / 1000`, the rate from the track or the host.
//! * Rating is stars x 20; the key is Engine's id (0 = C major = 8B, 1 = A minor = 8A, 2 = 9B, ...).
//! * The sample count is `length_s * rate`: the host knows the length in whole seconds only. It scales the overview waveform
//!   and places the last grid marker; the error is under half a second of the track.
//! * A track without a sample rate gets its row and no performance data (a warning says so).
//! * `isAnalyzed` is 1 when the track has an overview waveform and a grid (written now or by the player), else 0: a row without
//!   blobs and with `isAnalyzed = 0` is what Engine itself leaves a track it has not analysed yet [inferred: the insert trigger of
//!   the schema creates the empty `PerformanceData` row, `isAnalyzed` is the flag Engine desktop's analyser selects on].
//! * Not written: artwork (`albumArtId` 1 is the stock "none" row), memory cues beyond the main cue, cues beyond the eighth
//!   slot, MyTags and track colours (the report says how many cues were dropped).
//!
//! # Sources
//!
//! libdjinterop (<https://github.com/xsco/libdjinterop>, LGPL-3.0): `include/djinterop/engine/engine_schema.hpp` (the schema lists
//! and the Engine releases of each), `src/djinterop/engine/schema/schema.cpp` (version detection), `schema_2_18_0.cpp` and
//! `schema_3_0_0.cpp` (DDL, the `Information` row), `schema_3_0_1.cpp`, `schema_3_0_2.cpp` and the 2.x files (differences),
//! `engine/v2/*_blob.cpp` (blob layouts), `engine/v2/track_table.cpp` (`lastEditTime` from 2.20.3), `engine/v3/track_impl.cpp` and
//! `engine/v2/track_impl.cpp` (`Track` row defaults, the same in both), `engine/v2/playlist_*` and `playlist_entity_table.cpp`
//! (list and entity chains), `engine/engine.cpp` (`normalize_beatgrid`), `engine/track_utils.hpp` (overview extents). This plugin
//! shares no code with it.

pub mod blob;
pub mod check;
pub mod export;
pub mod perf;
pub mod playlists;
pub mod reader;
pub mod schema;
pub mod schema_2;
pub mod tracks;

use openfader_plugin::library::{
    ActionAnswer, CheckReport, Choice, DeviceInfo, ExportOptions, ExportReport, Feature, Field,
    FieldKind, LibraryPlugin, Probe, Scope, Snapshot, Source,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Name of the library in [`DeviceInfo::libraries`] and the folder it lives in.
const LIBRARY: &str = "Engine Library";

/// The owner of this plugin's journal on a device.
pub const OWNER: &str = "engine-dj";

/// Key of the device option that picks the schema of a new library ([`schema::NEW`], as [`schema::text`]).
pub const SCHEMA_OPTION: &str = "engine_dj.schema";

/// `Engine Library/Database2/m.db` of the device mounted at `root`.
pub fn database(root: &Path) -> PathBuf {
    root.join(LIBRARY).join("Database2").join("m.db")
}

pub struct Plugin;

impl LibraryPlugin for Plugin {
    fn probe(path: &Path) -> Option<Probe> {
        reader::probe(path)
    }

    fn open(path: &Path, _settings: &Value) -> Result<Box<dyn Source>, String> {
        reader::open(path)
    }

    /// Evidence (the export's note): eight hot cues, eight loops; a loop is a cue with an end. Memory cues are not limited here:
    /// a saved loop is a memory cue, and the writer counts what it drops itself.
    fn capabilities(_settings: &Value) -> openfader_plugin::library::Capabilities {
        openfader_plugin::library::Capabilities {
            hot_cues: Some(8),
            ..Default::default()
        }
    }

    fn analysis_ref(
        reference: &str,
        _settings: &Value,
    ) -> Option<openfader_plugin::library::Analysis> {
        reader::analysis_ref(reference)
    }

    fn device_probe(root: &Path, _settings: &Value) -> Result<DeviceInfo, String> {
        let db = database(root);
        let modified = std::fs::metadata(&db)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        Ok(DeviceInfo {
            title: "Engine DJ".into(),
            about: "Engine DJ 4.1 and newer and Denon DJ Prime/SC, Numark and RANE players on firmware 4.1 and newer; the schema option adds 2.0 to 4.0.1"
                .into(),
            options: vec![Field {
                key: SCHEMA_OPTION.into(),
                label: "Schema of a new library".into(),
                hint: "Only for a library that is made now: one that exists keeps its schema and is extended. Older Engine \
                       versions need 2.18.0 and are expected to be read by the newer ones too."
                    .into(),
                default: schema::text(schema::CURRENT),
                scope: Scope::Device,
                kind: FieldKind::Choice {
                    choices: vec![
                        Choice {
                            value: schema::text(schema::CURRENT),
                            label: "3.0.0: Engine DJ and players 4.1 and newer".into(),
                        },
                        Choice {
                            value: schema::text(schema::OLDEST),
                            label: "2.18.0: Engine DJ and players 2.0 to 4.0.1".into(),
                        },
                    ],
                    from_list: None,
                    none: None,
                },
            }],
            libraries: if db.is_file() {
                vec![LIBRARY.into()]
            } else {
                vec![]
            },
            can_write: root.is_dir(),
            features: vec![Feature::Export, Feature::Plan, Feature::Check],
            modified,
            ..Default::default()
        })
    }

    fn export(
        root: &Path,
        snapshot: &Snapshot,
        options: &ExportOptions,
        _settings: &Value,
    ) -> Result<ExportReport, String> {
        export::export(root, snapshot, options)
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
