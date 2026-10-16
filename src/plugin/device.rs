//! Devices (removable media) through library plugins: a plugin that declares `"library": {"device": true}` knows the layout of
//! the libraries on a USB stick or SD card and answers the device ops of `openfader_plugin::library` (probe, export, update,
//! remove, actions, settings, backups). The host never knows that layout: it asks each [`Provider`], shows what they answer
//! and hands them the work.
//!
//! Every op blocks and runs on the caller's thread (the app calls them from a worker thread). A long job passes its [`Control`]:
//! the plugin's progress calls update it and its cancel flag goes back to the plugin, which then ends the job with
//! `Err("cancelled")`.

use super::library::{Loaded, enabled, with_job};
use crate::library::snapshot::Snapshot;
use openfader_plugin::library::Request;
pub use openfader_plugin::library::{
    ActionAnswer, BackupTrack, Bank, BankCue, Confirm, DeviceAction, DeviceInfo, ExportOptions,
    ExportReport, Feature, MAX_BANKS, RemoveReport, RemoveTarget, SettingsRead, SettingsSchema,
    SettingsWritten, TrackEdit, UpdateReport,
};
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};

/// Progress and cancel of one job, shared between the worker and whoever shows it: the plugin reports `done` of `total` in a
/// named phase and stops at the next safe point once [`Control::cancel`] was called; a cancelled job returns `Err("cancelled")`.
#[derive(Default)]
pub struct Control {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    cancel: AtomicBool,
    phase: Mutex<String>,
}

impl Control {
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    /// What the job is doing, as the plugin named it ("Copying tracks").
    pub fn phase(&self) -> String {
        self.phase.lock().clone()
    }

    pub fn set_phase(&self, phase: &str) {
        phase.clone_into(&mut self.phase.lock());
    }

    /// Starts counting: `total` tracks (or files) to do, none done yet.
    pub fn start(&self, total: usize) {
        self.total.store(total, Relaxed);
        self.done.store(0, Relaxed);
    }
}

/// A loaded plugin with `"device": true`.
#[derive(Clone)]
pub struct Provider(&'static Loaded);

/// The enabled device plugins, in registry order.
pub fn providers() -> Vec<Provider> {
    enabled()
        .filter(|l| l.decl().device)
        .map(Provider)
        .collect()
}

/// The device plugin named `name`.
pub fn provider(name: &str) -> Option<Provider> {
    providers().into_iter().find(|p| p.name() == name)
}

/// The device plugins that hold a library at `root` or can write one, each with its answer; the first is the one that handles
/// the device (opens its library, updates from it): one that holds a library, the one offering the most there.
pub fn formats(root: &Path) -> Vec<(Provider, DeviceInfo)> {
    let mut all: Vec<_> = providers()
        .into_iter()
        .filter_map(|p| p.info(root).ok().map(|i| (p, i)))
        .filter(|(_, i)| i.can_write || !i.libraries.is_empty())
        .collect();
    if let Some(best) = (0..all.len()).max_by_key(|&i| {
        let info = &all[i].1;
        (
            !info.libraries.is_empty(),
            info.features.len(),
            usize::MAX - i,
        )
    }) {
        let first = all.remove(best);
        all.insert(0, first);
    }
    all
}

/// The plugin that handles the device mounted at `root` (the first of [`formats`]).
pub fn find(root: &Path) -> Option<(Provider, DeviceInfo)> {
    formats(root).into_iter().next()
}

/// The schemas answered so far, with the plugin settings they were asked under.
static SCHEMAS: LazyLock<Mutex<HashMap<String, (String, SettingsSchema)>>> =
    LazyLock::new(Mutex::default);

impl Provider {
    pub fn name(&self) -> &str {
        self.0.name()
    }

    fn call<T: serde::de::DeserializeOwned>(
        &self,
        req: impl FnOnce(serde_json::Value) -> Request,
        ctl: Option<&Arc<Control>>,
    ) -> Result<T, String> {
        let req = req(self.0.settings());
        match ctl {
            Some(c) => with_job(c, || self.0.call(&req)),
            None => self.0.call(&req),
        }
    }

    /// What the plugin finds on the device mounted at `root`.
    pub fn info(&self, root: &Path) -> Result<DeviceInfo, String> {
        self.call(
            |settings| Request::DeviceProbe {
                root: root.to_owned(),
                settings,
            },
            None,
        )
    }

    /// Writes `snap` to the device: a new library when it has none, else the existing one is extended.
    pub fn export(
        &self,
        root: &Path,
        snap: &Snapshot,
        o: &ExportOptions,
        ctl: &Arc<Control>,
    ) -> Result<ExportReport, String> {
        self.call(
            |settings| Request::Export {
                root: root.to_owned(),
                snapshot: snap.to_wire(),
                options: o.clone(),
                settings,
            },
            Some(ctl),
        )
    }

    /// "Update Collection": what the players changed since `baselines`.
    pub fn update(
        &self,
        root: &Path,
        snap: &Snapshot,
        baselines: &BTreeMap<String, i64>,
        ctl: &Arc<Control>,
    ) -> Result<UpdateReport, String> {
        self.call(
            |settings| Request::Update {
                root: root.to_owned(),
                snapshot: snap.to_wire(),
                baselines: baselines.clone(),
                settings,
            },
            Some(ctl),
        )
    }

    /// Deletes a playlist, histories or a track from the device.
    pub fn remove(
        &self,
        root: &Path,
        t: &RemoveTarget,
        delete_tracks: bool,
        config: &BTreeMap<String, String>,
        ctl: &Arc<Control>,
    ) -> Result<RemoveReport, String> {
        self.call(
            |settings| Request::Remove {
                root: root.to_owned(),
                target: t.clone(),
                delete_tracks,
                config: config.clone(),
                settings,
            },
            Some(ctl),
        )
    }

    /// Runs the device action `id` (one of [`DeviceInfo::actions`]); a [`Confirm`] answer wants the question asked and the action
    /// repeated with `confirmed`.
    pub fn action(
        &self,
        root: &Path,
        id: &str,
        confirmed: bool,
        config: &BTreeMap<String, String>,
        ctl: &Arc<Control>,
    ) -> Result<ActionAnswer, String> {
        self.call(
            |settings| Request::DeviceAction {
                root: root.to_owned(),
                action: id.to_owned(),
                confirmed,
                config: config.clone(),
                settings,
            },
            Some(ctl),
        )
    }

    /// The device settings pages of this plugin; asked once per plugin settings.
    pub fn schema(&self) -> Result<SettingsSchema, String> {
        let settings = self.0.settings();
        let key = settings.to_string();
        if let Some((k, s)) = SCHEMAS.lock().get(self.name())
            && *k == key
        {
            return Ok(s.clone());
        }
        let s: SettingsSchema = self.call(|settings| Request::SettingsSchema { settings }, None)?;
        SCHEMAS
            .lock()
            .insert(self.name().to_owned(), (key, s.clone()));
        Ok(s)
    }

    /// The settings values the device holds now, with a fingerprint of them.
    pub fn settings_read(&self, root: &Path) -> Result<SettingsRead, String> {
        self.call(
            |settings| Request::SettingsRead {
                root: root.to_owned(),
                settings,
            },
            None,
        )
    }

    /// Writes `values` (key → text) as the device's settings.
    pub fn settings_write(
        &self,
        root: &Path,
        values: &BTreeMap<String, String>,
    ) -> Result<SettingsWritten, String> {
        self.call(
            |settings| Request::SettingsWrite {
                root: root.to_owned(),
                values: values.clone(),
                settings,
            },
            None,
        )
    }

    /// The tracks the library inside the backup folder `dir` lists.
    pub fn backup_tracks(&self, dir: &Path) -> Result<Vec<BackupTrack>, String> {
        #[derive(serde::Deserialize)]
        struct Tracks {
            tracks: Vec<BackupTrack>,
        }
        self.call::<Tracks>(
            |settings| Request::BackupTracks {
                dir: dir.to_owned(),
                settings,
            },
            None,
        )
        .map(|t| t.tracks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_counts_and_cancels() {
        let c = Control::default();
        c.start(4);
        c.done.fetch_add(1, Relaxed);
        c.set_phase("Copying tracks");
        assert_eq!(
            (c.total.load(Relaxed), c.done.load(Relaxed), c.phase()),
            (4, 1, "Copying tracks".to_owned())
        );
        assert!(!c.cancelled());
        c.cancel();
        assert!(c.cancelled());
        c.start(2);
        assert_eq!(c.done.load(Relaxed), 0);
    }
}
