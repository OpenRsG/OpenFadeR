//! What happens before a job writes to a device, so that no write starts on a stick in an unknown state: a change a pulled stick or
//! a crash cut off is finished or undone ([`commit::recover`]), and the library that is there is backed up
//! (Preferences > DJ SYSTEM > DEVICE > "Back up the library before it is rewritten"). The backups taken this way are marked
//! automatic and the last [`KEEP_AUTO`] per device are kept; the user's own are never pruned.
//!
//! [`Ctx::safety`] collects what is needed on the UI thread; the [`Safety`] it returns goes to the worker.

use super::Ctx;
use openfader::{
    device::{backup, Device},
    plugin::device::{commit, commit::Recovered, Control, Feature},
    store::{device_settings, NewBackup, Store},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Automatic backups kept per device.
pub const KEEP_AUTO: usize = 5;

#[derive(Clone)]
pub struct Safety {
    store: Arc<Store>,
    /// Where backups go: the folder typed in the BACKUPS tab, else the default.
    dir: Option<PathBuf>,
    /// Preferences: back up before a write.
    backup: bool,
    computer: String,
}

impl Ctx {
    /// Read on the UI thread, used on the worker.
    pub(super) fn safety(&self) -> Safety {
        let behaviour = device_settings::Behaviour::load(&self.store);
        let typed = self
            .with_state(|s| s.get_backup_dir().trim().to_owned())
            .filter(|d| !d.is_empty());
        Safety {
            store: self.store.clone(),
            dir: typed.map(PathBuf::from).or_else(backup::default_dir),
            backup: behaviour.backup_before_write,
            computer: behaviour.computer(),
        }
    }
}

/// What the user is told about a change that was cut off on `label`.
pub fn recover_note(r: &Recovered, label: &str) -> String {
    match r {
        Recovered::Finished { .. } => tr!("An export to {} was cut off: it is now finished", label),
        Recovered::RolledBack { .. } => tr!(
            "An export to {} was cut off: the previous library is back in use",
            label
        ),
    }
}

/// Deletes the automatic backups of `device` beyond the newest `keep` (folder and index row); a folder that is gone already is
/// forgotten. Manual backups stay.
fn prune(store: &Store, device: &str, keep: usize) {
    for b in store.surplus_backups(device, keep).unwrap_or_default() {
        let path = Path::new(&b.path);
        if backup::delete(path).is_ok() || !path.exists() {
            let _ = store.delete_backup(b.id);
        }
    }
}

impl Safety {
    /// Finishes or undoes an interrupted change at `root` (called `label`); the note says what it did.
    pub fn recover(&self, root: &Path, label: &str) -> Result<Option<String>, String> {
        commit::recover(root)
            .map(|r| r.map(|r| recover_note(&r, label)))
            .map_err(|e| {
                tr!(
                    "{}: an earlier change was cut off and could not be put right: {}",
                    label,
                    e
                )
            })
    }

    /// Backs up the library of `dev` before a job rewrites it, when Preferences ask for it and the device has one. The note says
    /// where. A failed backup stops the job: nothing is written without it (switch the setting off to write anyway).
    pub fn backup_first(&self, dev: &Device, ctl: &Control) -> Result<Option<String>, String> {
        if !self.backup || !dev.can(Feature::Backup) || !dev.has_library() {
            return Ok(None);
        }
        let dir = self.dir.as_ref().ok_or(tr!("No backup folder: type one"))?;
        let made = backup::create(dev, dir, &self.computer, ctl).map_err(|e| {
            if e == "cancelled" {
                e
            } else {
                tr!(
                    "Not written: the backup of the library of {} failed ({})",
                    dev.label,
                    e
                )
            }
        })?;
        let bg = device_settings::config(&self.store, &dev.id)
            .get("background")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        self.store.add_backup(&NewBackup {
            device: &dev.id,
            taken_at: chrono::Local::now().timestamp(),
            computer: &self.computer,
            name: &dev.label,
            path: &made.dir.to_string_lossy(),
            fs: &dev.fs,
            bg,
            auto: true,
        })?;
        prune(&self.store, &dev.id, KEEP_AUTO);
        Ok(Some(tr!(
            "The library of {} was backed up first, to {}",
            dev.label,
            made.dir.display()
        )))
    }

    /// [`Self::recover`], then [`Self::backup_first`]: what every job that rewrites the library of `dev` starts with. Returns the
    /// notes to put under the job's result; the job's progress counters are reset for the work that follows.
    pub fn before_write(&self, dev: &Device, ctl: &Control) -> Result<Vec<String>, String> {
        let mut notes: Vec<String> = self.recover(&dev.mount, &dev.label)?.into_iter().collect();
        notes.extend(self.backup_first(dev, ctl)?);
        ctl.start(0);
        Ok(notes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::store::DeviceBackup;

    #[test]
    fn a_cut_off_change_is_told_as_finished_or_undone() {
        let owner = || "rekordbox-device".to_owned();
        assert_eq!(
            recover_note(&Recovered::Finished { owner: owner() }, "KEY"),
            "An export to KEY was cut off: it is now finished"
        );
        assert_eq!(
            recover_note(&Recovered::RolledBack { owner: owner() }, "KEY"),
            "An export to KEY was cut off: the previous library is back in use"
        );
    }

    #[test]
    fn a_clean_stick_says_nothing_and_a_pulled_stick_is_finished_and_told() {
        let root = std::env::temp_dir().join(format!("openfader-safety-{}", std::process::id()));
        std::fs::create_dir_all(root.join("LIB")).unwrap();
        let s = Safety {
            store: Arc::new(Store::in_memory().unwrap()),
            dir: None,
            backup: false,
            computer: "pc".into(),
        };
        assert_eq!(s.recover(&root, "KEY").unwrap(), None);
        let a = root.join("LIB/a.db");
        std::fs::write(&a, b"old").unwrap();
        let mut c = commit::Commit::new(&root, "test").unwrap();
        c.stage(&a, b"new").unwrap();
        commit::fail_at(Some(0));
        assert!(c.commit().is_err());
        commit::fail_at(None);
        assert_eq!(
            s.recover(&root, "KEY").unwrap().as_deref(),
            Some("An export to KEY was cut off: it is now finished")
        );
        assert_eq!(std::fs::read(&a).unwrap(), b"new");
        assert_eq!(s.recover(&root, "KEY").unwrap(), None, "once");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pruning_deletes_old_automatic_backups_and_keeps_the_users_own() {
        let root = std::env::temp_dir().join(format!("openfader-prune-{}", std::process::id()));
        let store = Store::in_memory().unwrap();
        let mut dirs = Vec::new();
        for (n, auto) in [(1, true), (2, false), (3, true), (4, true)] {
            let dir = root.join(format!("b{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("BACKUP.txt"), "x").unwrap();
            store
                .add_backup(&NewBackup {
                    device: "d",
                    taken_at: n,
                    computer: "pc",
                    name: "KEY",
                    path: &dir.to_string_lossy(),
                    fs: "vfat",
                    bg: 0,
                    auto,
                })
                .unwrap();
            dirs.push(dir);
        }
        prune(&store, "d", 2);
        let left: Vec<DeviceBackup> = store.backups().unwrap();
        assert_eq!(
            left.iter().map(|b| b.taken_at).collect::<Vec<_>>(),
            [4, 3, 2]
        );
        assert!(
            !dirs[0].exists(),
            "the oldest automatic one is gone with its folder"
        );
        assert!(dirs[1].exists() && dirs[2].exists() && dirs[3].exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
