//! The device side of the rekordbox plugins: writes, updates and edits the libraries on a device, its settings files and its
//! backups (the device ops of `openfader_plugin::library`). Called by `compat-rekordbox-device`.
//!
//! Device export (rekordbox EXPORT mode): writes a [`Snapshot`] of a library to a folder or removable drive as a rekordbox
//! Device Library that players and rekordbox read, or adds it to one that is already there:
//!
//! ```text
//! PIONEER/rekordbox/export.pdb        DeviceSQL: tracks, artists, albums, genres, keys, colors, playlist tree + entries, histories, columns
//! PIONEER/rekordbox/exportExt.pdb     MyTag (only when there is MyTag)
//! PIONEER/rekordbox/exportLibrary.db  OneLibrary (SQLCipher; the built-in key, or the plugin's `onelibrary_key` setting);
//!                                     left out of a new library when the device option [`ONE_OPTION`] is off
//! PIONEER/USBANLZ/P<xxx>/<8 hex>/ANLZ0000.{DAT,EXT,2EX}   grid, cues, waveforms of each track
//! PIONEER/Artwork/<00001>/{a,b}<n>{,_m}.jpg   covers, 80×80 and 240×240 JPEG (`a` for export.pdb, `b` for OneLibrary)
//! PIONEER/{MYSETTING,MYSETTING2,DJMMYSETTING,DEVSETTING}.DAT   device settings
//! PIONEER/djprofile.nxs                DJ profile (written when the device has none, see [`profile`])
//! Contents/<artist>/<album>/<file>    copies of the audio files
//! ```
//!
//! Children: [`pdb`] (DeviceSQL writer + reader), [`onelibrary`] (`exportLibrary.db`), [`anlz`] (analysis files), [`settings`],
//! `run` (the engine of `export`), `merge` (snapshot playlists and MyTag into an existing library), [`remove`] (delete tracks,
//! playlists and histories), [`convert`] (Device Library → OneLibrary), `carry` (what a rewritten OneLibrary keeps), [`update`]
//! (Update Collection: what the player changed), [`bank`] (Hot Cue Bank Lists), `progress` (host progress and cancel).
//! Format sources: `.local/re/export/notes.md`.
//! The tracks' rating, comment, cues and grid are whatever the snapshot carries; the host sends the *overlaid* library with the
//! store's colours, MyTag and user playlists already in it.
//!
//! `export` reads an existing library into [`pdb::db::Db`], adds / updates the snapshot's tracks and playlists and writes the
//! whole library again, all or nothing: audio and analysis files first (new names, fsynced), then the library files staged in
//! one [`Commit`] (journal `/.openfader/journal.json`), read back and compared with what was meant (`verify`), then switched
//! together. A cancel, an error or a failed read-back leaves the device as it was (the files already copied for new tracks are
//! removed again); a pull-out during the switch is finished or rolled back by the next write or `device_check`. With
//! `ExportOptions::dry_run` nothing is written and the report carries the `ExportPlan`. `check` is `device_check` / `device_fix`.

pub mod anlz;
pub mod bank;
mod carry;
pub mod check;
pub mod convert;
mod merge;
pub mod onelibrary;
pub mod pdb;
pub mod profile;
pub mod progress;
pub mod remove;
mod run;
pub mod settings;
pub mod update;
mod verify;

pub use settings::{read as settings_read, schema as settings_schema, write as settings_write};

use compat_rekordbox_common::at;
use openfader_plugin::library::{
    ActionAnswer, BackupTrack, CheckReport, DeviceAction, DeviceInfo, ExportOptions, ExportReport,
    Feature, Field, FieldKind, RemoveReport, RemoveTarget, Scope, Snapshot, UpdateReport,
    commit::{self, Commit},
};
use pdb::db::{Db, Tags};
use progress::Progress;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use settings::DeviceConfig;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// `PIONEER/rekordbox/export.pdb`, relative to the device root.
pub const PDB: &str = "PIONEER/rekordbox/export.pdb";
/// `PIONEER/rekordbox/exportExt.pdb` (MyTag).
pub const EXT: &str = "PIONEER/rekordbox/exportExt.pdb";
/// `PIONEER/rekordbox/exportLibrary.db` (OneLibrary).
pub const ONE: &str = "PIONEER/rekordbox/exportLibrary.db";
/// Device option: `0` leaves OneLibrary out of a device that holds none (a device that holds one keeps it up to date, so the
/// two libraries never disagree). Default `1`.
pub const ONE_OPTION: &str = "rekordbox.onelibrary";

/// Whether an export writes OneLibrary to a device that has none, from the device's `config`.
pub fn wants_onelibrary(config: &BTreeMap<String, String>) -> bool {
    config.get(ONE_OPTION).is_none_or(|v| v != "0")
}

/// Ids by name in order of first use; the empty name is id 0 (= none). Shared by the DeviceSQL and OneLibrary writers.
#[derive(Default)]
struct Names {
    ids: HashMap<String, u32>,
    rows: Vec<(u32, String)>,
}

impl Names {
    fn id(&mut self, name: &str) -> u32 {
        if name.is_empty() {
            return 0;
        }
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.rows.len() as u32 + 1;
        self.ids.insert(name.to_owned(), id);
        self.rows.push((id, name.to_owned()));
        id
    }
}

/// Opens the SQLCipher database `path` with `key`.
pub(crate) fn open_keyed(path: &Path, key: &str) -> Result<Connection, String> {
    keyed(Connection::open(path), path, key)
}

/// [`open_keyed`] without write access.
pub(crate) fn open_keyed_ro(path: &Path, key: &str) -> Result<Connection, String> {
    keyed(
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY),
        path,
        key,
    )
}

fn keyed(
    opened: rusqlite::Result<Connection>,
    path: &Path,
    key: &str,
) -> Result<Connection, String> {
    let conn = at(opened, path)?;
    at(conn.pragma_update(None, "key", key), path)?;
    Ok(conn)
}

/// The name of a `columns` / `menuItem` row: the label between the marker characters rekordbox wraps it in.
fn column_label(label: &str) -> String {
    format!("\u{fffa}{label}\u{fffb}")
}

/// Writes `bytes` to a file that is new (or may be overwritten whole, like a new track's analysis file) and flushes it to the
/// device. A library file that exists goes through a [`Commit`] instead.
fn put(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        at(std::fs::create_dir_all(dir), dir)?;
    }
    commit::write_synced(path, bytes)
}

/// The owner named in the device's journal while a [`Commit`] of this plugin is under way.
const OWNER: &str = "rekordbox-device";

/// A file or folder name: characters FAT and rekordbox reject become `_`; empty → `fallback` (the SDK's rule, shared by every
/// device format so they find each other's audio copies).
pub use openfader_plugin::library::clean_name as clean;

/// Today's local date as `YYYY-MM-DD`.
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Copies the database file `from` to the new `to`, with the write-ahead log `from` may have (a OneLibrary last written by
/// rekordbox), so the copy holds everything `from` does.
fn copy_db(from: &Path, to: &Path) -> Result<(), String> {
    let wal = |p: &Path| {
        let mut n = p.as_os_str().to_owned();
        n.push("-wal");
        std::path::PathBuf::from(n)
    };
    at(std::fs::copy(from, to), to)?;
    let _ = std::fs::remove_file(wal(to));
    if wal(from).is_file() {
        at(std::fs::copy(wal(from), wal(to)), to)?;
    }
    Ok(())
}

/// Builds a new `exportLibrary.db` from `db` and `tags` at the staged path of `one`, lets `then` add to it and stages it in `c`;
/// the staged file is removed when any step fails. The library id is kept from an existing `one`.
fn stage_onelibrary(
    c: &mut Commit,
    one: &Path,
    key: &str,
    db: &Db,
    tags: &Tags,
    then: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let id = one
        .is_file()
        .then(|| carry::library_id(one, key))
        .flatten()
        .unwrap_or(chrono::Local::now().timestamp() as u32); // any number; rekordbox's is random per library
    let tmp = Commit::staged_path(one);
    let _ = std::fs::remove_file(&tmp);
    onelibrary::write(&tmp, key, db, tags, id)
        .and_then(|()| then(&tmp))
        .and_then(|()| c.stage_built(one))
        .inspect_err(|_| drop(std::fs::remove_file(&tmp)))
}

/// Stages the library files of `dest` from `db` and `tags` in `c`: `exportLibrary.db` (encrypted with `key`; an existing one
/// hands its histories and cue rows over, see `carry`) when one is there or `new_one`, `exportExt.pdb` (when there is MyTag or
/// one existed) and, last, `export.pdb`. Nothing is on the device under its real name until `c` commits.
fn stage_libraries(
    c: &mut Commit,
    dest: &Path,
    db: &Db,
    tags: &Tags,
    key: &str,
    new_one: bool,
) -> Result<(), String> {
    let one = dest.join(ONE);
    if new_one || one.is_file() {
        stage_onelibrary(c, &one, key, db, tags, |tmp| {
            let carried = if one.is_file() {
                carry::carry(&one, tmp, key)
            } else {
                carry::histories(tmp, key, db)
            };
            carried.and_then(|()| bank::apply(tmp, key, &db.banks))
        })?;
    }
    stage_pdb(c, dest, db, tags)
}

/// Stages `exportExt.pdb` (when there is MyTag or one existed) and `export.pdb`, the Device Library.
fn stage_pdb(c: &mut Commit, dest: &Path, db: &Db, tags: &Tags) -> Result<(), String> {
    let ext = dest.join(EXT);
    if !tags.categories.is_empty() || ext.is_file() {
        c.stage(&ext, &tags.encode()?)?;
    }
    c.stage(&dest.join(PDB), &db.encode()?)
}

/// One change of the library files, all or nothing: stages them in `c` (which may hold other files already), reads them back
/// ([`verify`]) and switches them together.
fn write_libraries(
    mut c: Commit,
    dest: &Path,
    db: &Db,
    tags: &Tags,
    key: &str,
    new_one: bool,
) -> Result<(), String> {
    let mut progress = Progress::new();
    progress.phase("Writing the library", 1)?;
    stage_libraries(&mut c, dest, db, tags, key, new_one)?;
    progress.phase("Checking the library", 1)?;
    verify::staged(&c, db, tags, key)?;
    c.commit()
}

/// The OneLibrary key: the plugin's setting, else the built-in one, so both libraries are always written.
fn key(settings: &Value) -> String {
    compat_rekordbox_common::onelibrary_key(settings)
}

/// What the plugin does with a device root: its libraries, whether it can write, the convert action, the files to read ahead and
/// to back up.
pub fn device_probe(root: &Path, _settings: &Value) -> Result<DeviceInfo, String> {
    let libs = [("Device Library", PDB), ("OneLibrary", ONE)];
    let found = |f: &str| root.join(f).is_file();
    let modified = [PDB, ONE].iter().find(|f| found(f)).and_then(|f| {
        let m = std::fs::metadata(root.join(f)).ok()?.modified().ok()?;
        Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
    });
    Ok(DeviceInfo {
        title: "rekordbox".into(),
        about: "rekordbox 5, 6 and 7 and every CDJ / XDJ player read the Device Library; OneLibrary adds djay Pro, VirtualDJ, \
                CDJ-3000X, OPUS-QUAD, OMNIS-DUO and XDJ-AZ"
            .into(),
        options: vec![Field {
            key: ONE_OPTION.into(),
            label: "OneLibrary".into(),
            hint: "Also write OneLibrary (formerly Device Library Plus), which rekordbox 7 and the newest players read. A \
                   device that holds one keeps it up to date either way."
                .into(),
            default: "1".into(),
            scope: Scope::Device,
            kind: FieldKind::Bool,
        }],
        libraries: libs
            .iter()
            .filter(|(_, f)| found(f))
            .map(|(n, _)| (*n).to_owned())
            .collect(),
        can_write: root.is_dir(),
        features: vec![
            Feature::Export,
            Feature::Update,
            Feature::Remove,
            Feature::Histories,
            Feature::Backup,
            Feature::Settings,
            Feature::Plan,
            Feature::Check,
        ],
        actions: vec![DeviceAction {
            id: convert::ID.into(),
            label: "Convert from Device Library".into(),
            help: "Builds the OneLibrary from the Device Library of this device; an existing OneLibrary is replaced.".into(),
            enabled: found(PDB),
            writes: true,
        }],
        prefetch: [PDB, EXT, ONE]
            .iter()
            .filter(|f| found(f))
            .map(|f| (*f).to_owned())
            .collect(),
        backup: vec![settings::DIR.into()],
        modified,
    })
}

/// Writes `snapshot` to the device (a new library when it has none, else extends it), see the module docs.
pub fn export(
    root: &Path,
    snapshot: &Snapshot,
    options: &ExportOptions,
    settings: &Value,
) -> Result<ExportReport, String> {
    let key = key(settings);
    let config = DeviceConfig::from_values(&options.config);
    if options.dry_run {
        return run::dry_run(root, snapshot, options, &config);
    }
    let mut progress = Progress::new();
    let (mut report, ids) = run::run(root, snapshot, options, &config, &key, &mut progress)?;
    match update::baselines(root, &ids, snapshot, &mut progress) {
        Ok(b) => report.baselines = b,
        Err(e) => report.warnings.push(format!("baselines not recorded: {e}")),
    }
    report.fingerprint = settings_read(root, settings)
        .ok()
        .map(|r| r.fingerprint)
        .filter(|f| !f.is_empty());
    Ok(report)
}

/// The state of the libraries on the device and what is wrong with them (read-only), see [`check`].
pub fn device_check(
    root: &Path,
    config: &BTreeMap<String, String>,
    settings: &Value,
) -> Result<CheckReport, String> {
    check::run(root, config, &key(settings))
}

/// Runs the fix `fix` of a problem [`device_check`] reported, see [`check`].
pub fn device_fix(
    root: &Path,
    fix: &str,
    config: &BTreeMap<String, String>,
    settings: &Value,
) -> Result<ActionAnswer, String> {
    check::fix(
        root,
        fix,
        &DeviceConfig::from_values(config),
        &key(settings),
    )
}

/// What changed on the player since `baselines`.
pub fn update(
    root: &Path,
    snapshot: &Snapshot,
    baselines: &BTreeMap<String, i64>,
    _settings: &Value,
) -> Result<UpdateReport, String> {
    update::update(root, snapshot, baselines)
}

/// Deletes a playlist, histories or a track from the device.
pub fn remove(
    root: &Path,
    target: &RemoveTarget,
    delete_tracks: bool,
    config: &BTreeMap<String, String>,
    settings: &Value,
) -> Result<RemoveReport, String> {
    remove::run(
        root,
        target,
        delete_tracks,
        &DeviceConfig::from_values(config),
        &key(settings),
    )
}

/// Runs the device action `action` (`convert`).
pub fn device_action(
    root: &Path,
    action: &str,
    confirmed: bool,
    config: &BTreeMap<String, String>,
    settings: &Value,
) -> Result<ActionAnswer, String> {
    match action {
        convert::ID => convert::to_onelibrary(
            root,
            &key(settings),
            confirmed,
            &DeviceConfig::from_values(config),
        ),
        _ => Err(format!("unknown action {action}")),
    }
}

/// The tracks the library inside the backup folder `dir` lists: what a restore copies back when the device lacks them.
pub fn backup_tracks(dir: &Path, _settings: &Value) -> Result<Vec<BackupTrack>, String> {
    if !dir.join(PDB).is_file() {
        return Err(format!("{} is not a device library backup", dir.display()));
    }
    let (db, _) = pdb::read::open(dir, &DeviceConfig::default())?;
    Ok(db
        .tracks
        .into_iter()
        .map(|t| BackupTrack {
            title: t.title,
            name: t.filename,
            size: u64::from(t.file_size),
            file: t.file_path.trim_start_matches('/').to_owned(),
        })
        .collect())
}
