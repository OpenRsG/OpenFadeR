//! rekordbox's public "Export Collection in xml format" (`rekordbox.xml`) as a library plugin: reads it as a source and
//! writes the host's collection in it.
//!
//! Evidence (spec = pinned public reference):
//! - spec: Pioneer DJ, "XML file format for playlists sharing", one-page PDF
//!   <https://cdn.rekordbox.com/files/20200410160904/xml_format_list.pdf> (sha256
//!   `a1ca38cc04ffffa4e1eaac82ca85c131c1e677eaecfbe6b7dd11f8d1a4102724` as fetched 2026-10-13).
//!   It defines `DJ_PLAYLISTS/COLLECTION/TRACK` with `TrackID Name Artist Album Genre Tonality AverageBpm
//!   TotalTime (whole seconds) Location (percent-encoded `file://localhost/…` URI) Rating (0 = "0" … 5 stars =
//!   "255", steps of 51) Comments DateAdded (yyyy-mm-dd) BitRate (kbps) SampleRate (Hz)`, and
//!   `PLAYLISTS/NODE` (`Type` 0 folder / 1 playlist, a root folder NODE "ROOT", playlists hold
//!   `TRACK Key=` with `KeyType` 0 = `TrackID`, 1 = `Location`).
//! - spec: pyrekordbox 0.4.x `pyrekordbox/rbxml.py` @ 43137cb91f8297c768d2a8bdce5b27b2225f0a2d
//!   (<https://raw.githubusercontent.com/dylanljones/pyrekordbox/43137cb91f8297c768d2a8bdce5b27b2225f0a2d/pyrekordbox/rbxml.py>):
//!   the same attribute list, `RATING_MAPPING` 0/51/102/153/204/255 to 0 to 5, `file://localhost/` prefix.
//! - inferred: `Tonality` is shown as the exporting rekordbox's key notation (Camelot, Alphanumeric or
//!   classic), copied as is; a Location without the `file://` scheme is taken as a plain path; a Windows
//!   path `file://localhost/C:/Music/x.mp3` becomes `C:/Music/x.mp3` and needs the `path_map` setting (or
//!   `OPENFADER_PATH_MAP`) on another OS. Playlist entries naming an unknown key are dropped.
//! - spec: the same PDF defines `TRACK/TEMPO` (`Inizio` seconds, `Bpm`, `Metro` "n/d", `Battito` beat in bar
//!   at `Inizio`) and `TRACK/POSITION_MARK` (`Name`, `Type` 0 cue / 4 loop, `Start`/`End` seconds, `Num` -1
//!   memory or 0 to 7 hot cue, `Red Green Blue`); pyrekordbox rbxml.py @ 43137cb9 reads the same attributes.
//! - inferred: each TEMPO runs until the next one's `Inizio` (last: to `TotalTime`), beats every 60/Bpm s;
//!   cue types 1 to 3 (fade/load) are ignored. Only verified on synthetic XML, not on a real export.
//!
//! `analysis` returns the beat grid and cues, no waveforms; an XML track has no `analysis_ref`, no artwork. Tracks are
//! matched by audio `file`, so two entries with one Location share the last's data.

mod read;
mod write;

pub use read::RekordboxXml;

use openfader_plugin::library::{Capabilities, LibraryPlugin, Probe, Snapshot, Source};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

pub struct RekordboxXmlPlugin;

fn text<'a>(settings: &'a Value, id: &str) -> &'a str {
    settings.get(id).and_then(Value::as_str).unwrap_or("")
}

impl LibraryPlugin for RekordboxXmlPlugin {
    fn probe(path: &Path) -> Option<Probe> {
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
        {
            return None;
        }
        let mut head = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(64 * 1024)
            .read_to_end(&mut head)
            .ok()?;
        String::from_utf8_lossy(&head)
            .contains("<DJ_PLAYLISTS")
            .then(|| Probe {
                score: 90,
                kind: "rekordbox.xml".into(),
            })
    }

    fn open(path: &Path, settings: &Value) -> Result<Box<dyn Source>, String> {
        let map = rekordbox_common::path_map(text(settings, "path_map"))?;
        Ok(Box::new(RekordboxXml::open(path, &map)?))
    }

    fn write_collection(
        path: &Path,
        snapshot: &Snapshot,
        settings: &Value,
    ) -> Result<usize, String> {
        let grid = settings
            .get("export_grid")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        write::write(path, snapshot, grid)
    }

    /// Evidence: the spec's `POSITION_MARK Num` is -1 (memory) or 0 to 7 (hot cue), so eight hot cue slots; TEMPO elements
    /// carry tempo changes; a track's `Rating` runs 0 to 255 in steps of 51 (five stars); the file is written whole from the
    /// snapshot, so it can rewrite an existing library.
    fn capabilities(_settings: &Value) -> Capabilities {
        Capabilities {
            hot_cues: Some(8),
            write_back: true,
            ..Capabilities::default()
        }
    }
}
