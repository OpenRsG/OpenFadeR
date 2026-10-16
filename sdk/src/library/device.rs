//! Devices: what a library plugin can do with a removable device (a USB stick, an SD card) the host found mounted. The host
//! never knows the folder layout of a device: it asks every library plugin that declares `"device": true` ([`DeviceInfo`]),
//! shows what they answer, and hands the work (export, update, remove, actions, settings, backups) to the plugin.
//!
//! All paths are absolute. `settings` is the plugin's own settings (id → value) from `plugin.json`, in every request; `config`
//! is the device's settings (key → text) the host stores for the device or for all devices ([`SettingsSchema`]).
//!
//! | `op` | other fields | answer |
//! |---|---|---|
//! | `device_probe` | `root` | [`DeviceInfo`] |
//! | `export` | `root`, `snapshot`, `options` | [`ExportReport`] |
//! | `update` | `root`, `snapshot`, `baselines` | [`UpdateReport`] |
//! | `remove` | `root`, `target`, `delete_tracks`, `config` | [`RemoveReport`] |
//! | `device_action` | `root`, `action`, `confirmed`, `config` | [`ActionAnswer`] |
//! | `settings_schema` | | [`SettingsSchema`] |
//! | `settings_read` | `root` | [`SettingsRead`] |
//! | `settings_write` | `root`, `values` | [`SettingsWritten`] |
//! | `backup_tracks` | `dir` (a backup of the plugin's `backup` paths) | `{"tracks": [`[`BackupTrack`]`]}` |
//! | `analysis_ref` | `reference` | `{"analysis": `[`Analysis`]` or null}` |
//!
//! A long job (`export`, `update`, `remove`, `device_action`) runs inside the call, on a thread of the host; it reports its
//! progress and learns about a cancel through the host callbacks of [`super::host`].

use super::{Analysis, Beat, Cue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What the plugin does with devices; the host shows only what is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// `export`: write the collection (or ticked playlists) to the device.
    Export,
    /// `update`: take what was edited on the player back into the collection.
    Update,
    /// `remove`: delete playlists, histories and tracks from the device.
    Remove,
    /// The device library lists play histories (playlists whose id starts with `history:`), and `remove` can delete them.
    Histories,
    /// The device's library can be backed up and restored (`DeviceInfo::backup`, `backup_tracks`).
    Backup,
    /// `settings_schema`, `settings_read`, `settings_write`.
    Settings,
}

/// One button of a device's panel (`device_action`), e.g. converting its library to a newer format.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceAction {
    pub id: String,
    /// Button text.
    pub label: String,
    /// A line under the button.
    #[serde(default)]
    pub help: String,
    /// Whether it can run on this device now (e.g. it holds a library to convert).
    pub enabled: bool,
    /// The action writes to the device: refused on a read-only or unsupported one.
    #[serde(default)]
    pub writes: bool,
}

/// Answer of `device_probe`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceInfo {
    /// The libraries of this plugin found on the device, by name ("Device Library"); empty: it holds none.
    pub libraries: Vec<String>,
    /// The plugin can write a library to this device (it may hold none yet).
    pub can_write: bool,
    pub features: Vec<Feature>,
    pub actions: Vec<DeviceAction>,
    /// Files (relative to the root) that make the library slow to open on a stick: the host reads them once, with a progress
    /// bar, before opening the library.
    pub prefetch: Vec<String>,
    /// Folders (relative to the root) a backup copies and a restore puts back.
    pub backup: Vec<String>,
    /// Unix seconds the library was last written, when the plugin can tell.
    pub modified: Option<i64>,
}

/// A Cue Bank: the cues of tracks (by audio file) a device can load onto its pads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bank {
    pub name: String,
    pub cues: Vec<BankCue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BankCue {
    pub file: PathBuf,
    pub cue: Cue,
}

/// Most Cue Banks a device holds.
pub const MAX_BANKS: usize = 8;

/// What an `export` needs besides the snapshot.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportOptions {
    /// Free bytes on the device: the job fails up front when the tracks to copy do not fit. `None`: do not check.
    pub free: Option<u64>,
    /// The device's settings: its own values over those of all devices, as stored (key → text). A key that is not there has
    /// the default of [`SettingsSchema`].
    pub config: BTreeMap<String, String>,
    /// Cue Banks to put on the device; a list replaces the device's list of the same name.
    pub banks: Vec<Bank>,
}

/// Answer of `export`: a new library is made when the device has none, else it is extended.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportReport {
    /// Tracks written (copied audio + analysis): `added` + `updated`.
    pub tracks: usize,
    pub added: usize,
    /// Tracks the device had already: their metadata, cues and grid are refreshed, the audio is not copied again.
    pub updated: usize,
    /// Playlists and folders of the snapshot (written or refreshed).
    pub playlists: usize,
    /// Tracks left out, or analysis that could not be made, with the reason.
    pub warnings: Vec<String>,
    /// Things the user should know besides the counts ("the encrypted library was not written: set its key").
    pub notes: Vec<String>,
    /// `(`[`super::track_key`]`, signature)` of what the device holds for each track written, for the next `update`.
    pub baselines: Vec<(String, i64)>,
    /// [`SettingsWritten::fingerprint`] of the settings files written with the library.
    pub fingerprint: Option<String>,
}

/// What `update` changed on one collection track (a part that is `None` did not change).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TrackEdit {
    /// [`super::track_key`] of the collection track.
    pub key: String,
    pub rating: Option<u8>,
    pub comment: Option<String>,
    pub color: Option<u8>,
    pub cues: Option<Vec<Cue>>,
    /// `(the device's grid, the collection's grid before)`.
    pub grid: Option<(Vec<Beat>, Vec<Beat>)>,
}

/// Answer of `update` ("Update Collection"): the host applies `edits` to its store and keeps `baselines`.
///
/// A device track is a collection track when both have the same [`super::track_key`]. A track whose signature is that of
/// `baselines` was not touched on the player and is left alone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateReport {
    /// Device tracks that are collection tracks.
    pub matched: usize,
    /// Device tracks not in the collection (not updated).
    pub unmatched: usize,
    pub edits: Vec<TrackEdit>,
    /// Signatures to remember for the matched tracks that changed.
    pub baselines: Vec<(String, i64)>,
}

/// What `remove` deletes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoveTarget {
    /// A playlist or folder, by the names from the top level down (ids differ between programs, names do not).
    Playlist { path: Vec<String> },
    /// Play histories by name.
    Histories { names: Vec<String> },
    /// A track by its audio file (a path under the root).
    Track { file: PathBuf },
}

/// Answer of `remove`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoveReport {
    pub tracks: usize,
    pub playlists: usize,
    /// Histories deleted.
    pub histories: usize,
    /// Files that could not be deleted (the library no longer lists them).
    pub warnings: Vec<String>,
}

/// A question the plugin needs answered before an action goes on; the host asks it and repeats the request with
/// `confirmed: true` on "yes".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Confirm {
    pub text: String,
    /// Label of the "yes" button.
    pub yes: String,
}

/// Answer of `device_action`: done (`message`), or a [`Confirm`] first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionAnswer {
    pub message: String,
    pub confirm: Option<Confirm>,
}

/// One track a backup's library lists: what a restore needs to copy it back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackupTrack {
    pub title: String,
    /// Audio file name and size: the host finds it in the collection by these.
    pub name: String,
    pub size: u64,
    /// Where it lives on the device, relative to the root.
    pub file: String,
}

/// Where a setting applies: to every device (the Preferences window), to one device, or to both (a device without an own value
/// follows the defaults).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Both,
    Global,
    Device,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// What is stored.
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    /// One of `choices` (a drop-down, or chips when there are few).
    Choice {
        choices: Vec<Choice>,
        /// The choices are `none` followed by the active entries of the [`FieldKind::List`] field with this key (a value
        /// is then an entry's value, `none` is the empty text).
        #[serde(default)]
        from_list: Option<String>,
        #[serde(default)]
        none: Option<String>,
    },
    Text,
    /// Stored as `1` / `0`.
    Bool,
    /// Entries move between an inactive and an active list and are ordered in the active one; stored as the active values
    /// joined by `,`. `items` is every entry in its table order.
    List {
        inactive: String,
        active: String,
        items: Vec<Choice>,
    },
    /// A file the user imports (a picture): stored as the path of the host's copy; empty: none. `sets` are other keys the host
    /// writes with the import (key → value), e.g. the switch that makes the device use the picture.
    File {
        extensions: Vec<String>,
        #[serde(default)]
        sets: BTreeMap<String, String>,
    },
    /// A line of text, not a setting.
    Info,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub hint: String,
    /// The value without any stored one, as stored.
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub scope: Scope,
    pub kind: FieldKind,
}

/// A header and the fields under it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Empty: no header.
    pub label: String,
    pub fields: Vec<Field>,
}

/// One page of the settings window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tab {
    pub label: String,
    pub scope: Scope,
    pub groups: Vec<Group>,
    /// Button of the Preferences page: writes the defaults of this tab to every connected device (their own values of its keys
    /// are dropped).
    pub apply_label: Option<String>,
    /// Button of a device's page: `settings_read` takes what was changed on the player over as the device's own values of
    /// this tab.
    pub read_label: Option<String>,
    /// A [`FieldKind::Bool`] key of this tab: when on for a device, an edit of any key of this tab is written to the connected
    /// device at once.
    pub auto_key: Option<String>,
}

/// Answer of `settings_schema`: the settings pages the host draws for the devices of this plugin (labels and options are the
/// plugin's).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsSchema {
    pub tabs: Vec<Tab>,
}

/// Answer of `settings_read`: the values the device holds now (only the keys it has files for), and a fingerprint of them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettingsRead {
    pub values: BTreeMap<String, String>,
    /// Equal for equal device contents; the host compares it with [`SettingsWritten::fingerprint`] to see that the player changed
    /// them.
    pub fingerprint: String,
}

/// Answer of `settings_write`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettingsWritten {
    /// What `settings_read` answers for the device right after this write.
    pub fingerprint: String,
}

/// An `analysis_ref` answer, shared by the host's callback of the same name: the analysis of a track.
#[derive(Serialize, Deserialize, Default)]
pub struct Analysed {
    pub analysis: Option<Analysis>,
}

/// `<plugin>:<opaque>`: how the host stores an `analysis_ref` of a plugin's track so it asks that plugin again.
pub fn owned_ref(plugin: &str, opaque: &str) -> String {
    format!("{plugin}:{opaque}")
}

/// The plugin and the opaque part of a ref made by [`owned_ref`]; `None` for a bare one (a path, even a Windows `C:\…` one).
pub fn split_ref(r: &str) -> Option<(&str, &str)> {
    r.split_once(':')
        .filter(|(p, _)| p.len() > 1 && !p.contains(['/', '\\']))
}

/// Stable identity of a track in the host's store: audio file name and size (`name|size`; a file that cannot be read counts as
/// size 0), else title and artist (`meta|title|artist`). Plugins use it as the key of [`TrackEdit`] and of baselines.
pub fn track_key(t: &super::Track) -> String {
    match &t.file {
        Some(f) => format!(
            "{}|{}",
            f.file_name()
                .map_or_else(Default::default, |n| n.to_string_lossy()),
            std::fs::metadata(f).map_or(0, |m| m.len())
        ),
        None => format!("meta|{}|{}", t.title, t.artist),
    }
}

/// A file or folder name on a device: characters FAT file systems and players reject become `_`, at most 120 characters, no
/// surrounding spaces or trailing dots; empty → `fallback`. Every device plugin names the audio it copies with it.
pub fn clean_name(s: &str, fallback: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let s = s.trim().trim_end_matches('.');
    if s.is_empty() {
        fallback.into()
    } else {
        s.into()
    }
}

/// Where the audio of `t` lives on the device mounted at `root`, the same for every format written there so a device holds one
/// copy: `t.file` itself when it is already below `root` (the host points a track the device holds at that copy); else
/// `Contents/<artist>/<album>/<name>` or its ` (2)`, ` (3)` … variant that holds a file of the same size; else the first of
/// those that is free. Returns the path relative to `root` with `/` separators and whether the file is there already. `Err`:
/// `t` has no readable audio file.
pub fn audio_slot(root: &Path, t: &super::Track) -> Result<(String, bool), String> {
    let file = t
        .file
        .as_deref()
        .ok_or_else(|| format!("{}: no audio file", t.title))?;
    if let Ok(rel) = file.strip_prefix(root) {
        let rel: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
        return Ok((rel.join("/"), file.is_file()));
    }
    let size = std::fs::metadata(file)
        .map_err(|e| format!("{}: {e}", file.display()))?
        .len();
    let name = clean_name(
        &file
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        "track",
    );
    let dir = format!(
        "Contents/{}/{}",
        clean_name(&t.artist, "UnknownArtist"),
        clean_name(&t.album, "UnknownAlbum")
    );
    let (stem, ext) = name
        .rsplit_once('.')
        .map_or((name.as_str(), String::new()), |(s, e)| {
            (s, format!(".{e}"))
        });
    for n in 1.. {
        let rel = if n == 1 {
            format!("{dir}/{name}")
        } else {
            format!("{dir}/{stem} ({n}){ext}")
        };
        match std::fs::metadata(root.join(&rel)) {
            Ok(m) if m.len() == size => return Ok((rel, true)),
            Ok(_) => {}
            Err(_) => return Ok((rel, false)),
        }
    }
    unreachable!()
}

/// [`audio_slot`], copying the audio there when the device does not hold it yet (through a temporary file, so a cut copy never
/// looks whole). Returns the relative path and whether it was copied.
pub fn place_audio(root: &Path, t: &super::Track) -> Result<(String, bool), String> {
    let (rel, there) = audio_slot(root, t)?;
    if there {
        return Ok((rel, false));
    }
    let (src, dest) = (t.file.as_deref().unwrap_or(Path::new("")), root.join(&rel));
    let tmp: PathBuf = dest.with_extension("part");
    let err = |e: std::io::Error| format!("{}: {e}", dest.display());
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    std::fs::copy(src, &tmp)
        .and_then(|_| std::fs::rename(&tmp, &dest))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            err(e)
        })?;
    Ok((rel, true))
}
