//! rekordbox Device Libraries on USB sticks and SD cards as a library plugin: reads the libraries a device holds (the
//! `export.pdb` database and the encrypted `exportLibrary.db`), answers for the analysis files of their tracks, and
//! writes, updates and edits them (the work is in `compat-rekordbox-export`).

mod read;

use compat_rekordbox_export as ops;
use openfader_plugin::library::{
    ActionAnswer, Analysis, BackupTrack, CheckReport, DeviceInfo, ExportOptions, ExportReport,
    LibraryPlugin, Probe, RemoveReport, RemoveTarget, SettingsRead, SettingsSchema,
    SettingsWritten, Snapshot, Source, UpdateReport,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub struct Device;

impl LibraryPlugin for Device {
    fn probe(path: &Path) -> Option<Probe> {
        read::probe(path)
    }

    fn device_check(
        root: &Path,
        config: &BTreeMap<String, String>,
        settings: &Value,
    ) -> Result<CheckReport, String> {
        ops::device_check(root, config, settings)
    }

    fn device_fix(
        root: &Path,
        fix: &str,
        config: &BTreeMap<String, String>,
        settings: &Value,
    ) -> Result<ActionAnswer, String> {
        ops::device_fix(root, fix, config, settings)
    }

    fn open(path: &Path, settings: &Value) -> Result<Box<dyn Source>, String> {
        read::open(path, settings)
    }

    fn analysis_ref(reference: &str, _settings: &Value) -> Option<Analysis> {
        read::analysis_ref(reference)
    }

    fn device_probe(root: &Path, settings: &Value) -> Result<DeviceInfo, String> {
        ops::device_probe(root, settings)
    }

    fn export(
        root: &Path,
        snapshot: &Snapshot,
        options: &ExportOptions,
        settings: &Value,
    ) -> Result<ExportReport, String> {
        ops::export(root, snapshot, options, settings)
    }

    fn update(
        root: &Path,
        snapshot: &Snapshot,
        baselines: &BTreeMap<String, i64>,
        settings: &Value,
    ) -> Result<UpdateReport, String> {
        ops::update(root, snapshot, baselines, settings)
    }

    fn remove(
        root: &Path,
        target: &RemoveTarget,
        delete_tracks: bool,
        config: &BTreeMap<String, String>,
        settings: &Value,
    ) -> Result<RemoveReport, String> {
        ops::remove(root, target, delete_tracks, config, settings)
    }

    fn device_action(
        root: &Path,
        action: &str,
        confirmed: bool,
        config: &BTreeMap<String, String>,
        settings: &Value,
    ) -> Result<ActionAnswer, String> {
        ops::device_action(root, action, confirmed, config, settings)
    }

    fn settings_schema(settings: &Value) -> Result<SettingsSchema, String> {
        ops::settings_schema(settings)
    }

    fn settings_read(root: &Path, settings: &Value) -> Result<SettingsRead, String> {
        ops::settings_read(root, settings)
    }

    fn settings_write(
        root: &Path,
        values: &BTreeMap<String, String>,
        settings: &Value,
    ) -> Result<SettingsWritten, String> {
        ops::settings_write(root, values, settings)
    }

    fn backup_tracks(dir: &Path, settings: &Value) -> Result<Vec<BackupTrack>, String> {
        ops::backup_tracks(dir, settings)
    }
}
