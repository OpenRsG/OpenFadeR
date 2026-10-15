//! Device library backup and restore. The backup holds the device's library, not its music: the folders the device plugin lists
//! ([`DeviceInfo::backup`](crate::plugin::device::DeviceInfo::backup): databases, analysis files, artwork, settings) plus a
//! `BACKUP.txt` naming the device. Restoring is "(1/2) Restoring library, (2/2) Restoring tracks": the library is put back and the
//! tracks of it that are missing on the device ([`Provider::backup_tracks`]) are copied from the collection's files; the ones the
//! collection does not have are listed. The backup goes to a local folder.
//!
//! The backup index (date, computer, device, destination, file system, background colour) is in the store
//! ([`crate::store::DeviceBackup`]).

use super::Device;
use crate::paths::{at, on_device};
use crate::plugin::device::{Control, Feature, Provider};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

/// The marker file of a backup folder; [`delete`] refuses folders without it.
const MARKER: &str = "BACKUP.txt";

/// What [`create`] wrote.
#[derive(Debug)]
pub struct Made {
    pub dir: PathBuf,
    pub files: usize,
}

/// What [`restore`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Restore {
    /// Nothing was written: these tracks (titles) are on neither the device nor in the collection; call again with `go_on`.
    Missing(Vec<String>),
    Done {
        tracks: usize,
        missing: Vec<String>,
    },
}

/// Where backups go unless the user chose a folder: `<data dir>/device-backups`.
pub fn default_dir() -> Option<PathBuf> {
    crate::paths::data_dir().map(|d| d.join("device-backups"))
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| {
        d.flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() { count_files(&p) } else { 1 }
            })
            .sum()
    })
}

/// Copies `src` to `dst` (created), counting files in `ctl`; stops with `Err("cancelled")`.
fn copy_tree(src: &Path, dst: &Path, ctl: &Control) -> Result<(), String> {
    at(std::fs::create_dir_all(dst), dst)?;
    for e in at(std::fs::read_dir(src), src)?.flatten() {
        if ctl.cancelled() {
            return Err("cancelled".into());
        }
        let (from, to) = (e.path(), dst.join(e.file_name()));
        if from.is_dir() {
            copy_tree(&from, &to, ctl)?;
        } else {
            at(std::fs::copy(&from, &to), &to)?;
            ctl.done.fetch_add(1, Relaxed);
        }
    }
    Ok(())
}

/// Backs the library of `dev` up into a new folder `<into>/<label>-<date>-<time>`; a failed or cancelled backup leaves nothing.
pub fn create(dev: &Device, into: &Path, computer: &str, ctl: &Control) -> Result<Made, String> {
    let folders: Vec<&String> = dev
        .info
        .backup
        .iter()
        .filter(|f| dev.mount.join(f).is_dir())
        .collect();
    if !dev.can(Feature::Backup) || folders.is_empty() {
        return Err(format!("{} holds no library to back up", dev.label));
    }
    let name = dev.label.replace(['/', '\\'], "_");
    let dir = into.join(format!(
        "{}-{}",
        if name.is_empty() { "device" } else { &name },
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    ctl.set_phase("Backing up the library");
    let files = folders
        .iter()
        .map(|f| count_files(&dev.mount.join(f)))
        .sum();
    ctl.start(files);
    let made = folders
        .iter()
        .try_for_each(|f| copy_tree(&dev.mount.join(f), &dir.join(f), ctl))
        .and_then(|()| {
            let note = format!(
                "device: {}\ncomputer: {computer}\nfile system: {}\ntaken: {}\n",
                dev.label,
                dev.fs,
                chrono::Local::now().to_rfc3339()
            );
            at(std::fs::write(dir.join(MARKER), note), &dir)
        });
    match made {
        Ok(()) => Ok(Made { dir, files }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// Deletes a backup folder made by [`create`].
pub fn delete(dir: &Path) -> Result<(), String> {
    if !dir.join(MARKER).is_file() {
        return Err(format!("{} is not a backup folder", dir.display()));
    }
    at(std::fs::remove_dir_all(dir), dir)
}

/// Puts the backup `dir` onto the device at `dest`: the library first (each backed-up folder replaces the device's; a cancel before
/// the end leaves them alone), then the tracks the restored library lists (`provider`) and the device lacks, which
/// `finder(file name, size)` locates in the collection. Tracks `finder` cannot find stop the restore with [`Restore::Missing`]
/// unless `go_on`.
pub fn restore(
    provider: &Provider,
    dir: &Path,
    dest: &Path,
    finder: &dyn Fn(&str, u64) -> Option<PathBuf>,
    go_on: bool,
    ctl: &Control,
) -> Result<Restore, String> {
    let folders: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if !dir.join(MARKER).is_file() || folders.is_empty() {
        return Err(format!("{} is not a device library backup", dir.display()));
    }
    let (mut copy, mut missing) = (Vec::new(), Vec::new());
    for t in provider.backup_tracks(dir)? {
        let to = on_device(dest, &t.file);
        if to.is_file() {
            continue;
        }
        match finder(&t.name, t.size) {
            Some(from) => copy.push((from, to)),
            None => missing.push(t.title),
        }
    }
    if !missing.is_empty() && !go_on {
        return Ok(Restore::Missing(missing));
    }

    ctl.set_phase("(1/2) Restoring library");
    ctl.start(folders.iter().map(|f| count_files(&dir.join(f))).sum());
    let tmp = |f: &str| dest.join(format!("{f}.restoring"));
    let clean = || {
        folders
            .iter()
            .for_each(|f| drop(std::fs::remove_dir_all(tmp(f))))
    };
    clean();
    folders
        .iter()
        .try_for_each(|f| copy_tree(&dir.join(f), &tmp(f), ctl))
        .inspect_err(|_| clean())?;
    for f in &folders {
        let live = dest.join(f);
        if live.exists() {
            at(std::fs::remove_dir_all(&live), &live)?;
        }
        at(std::fs::rename(tmp(f), &live), &live)?;
    }

    ctl.set_phase("(2/2) Restoring tracks");
    ctl.start(copy.len());
    for (from, to) in &copy {
        if ctl.cancelled() {
            return Err("cancelled (the library is restored, some tracks are not)".into());
        }
        if let Some(d) = to.parent() {
            at(std::fs::create_dir_all(d), d)?;
        }
        at(std::fs::copy(from, to), to)?;
        ctl.done.fetch_add(1, Relaxed);
    }
    Ok(Restore::Done {
        tracks: copy.len(),
        missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Kind, Support};
    use crate::plugin::device::DeviceInfo;

    #[test]
    fn backup_copies_the_listed_folders_and_delete_needs_the_marker() {
        let root = std::env::temp_dir().join(format!("openfader-backup-{}", std::process::id()));
        let (stick, into) = (root.join("KEY"), root.join("out"));
        std::fs::create_dir_all(stick.join("LIB/sub")).unwrap();
        std::fs::write(stick.join("LIB/a.db"), "a").unwrap();
        std::fs::write(stick.join("LIB/sub/b.bin"), "b").unwrap();
        let mut dev = Device::new("id".into(), stick, "vfat".into(), Kind::Usb, Support::Ok);
        let ctl = Control::default();
        assert!(
            create(&dev, &into, "pc", &ctl).is_err(),
            "nothing listed: nothing to back up"
        );
        dev.info = DeviceInfo {
            backup: vec!["LIB".into(), "GONE".into()],
            features: vec![Feature::Backup],
            ..DeviceInfo::default()
        };
        let made = create(&dev, &into, "pc", &ctl).unwrap();
        assert_eq!(made.files, 2);
        assert!(made.dir.join("LIB/sub/b.bin").is_file() && made.dir.join(MARKER).is_file());
        assert!(delete(&root).is_err());
        delete(&made.dir).unwrap();
        assert!(!made.dir.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
