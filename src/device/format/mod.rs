//! Formatting a stick or card from inside the app (a stick a DJ player will not read is the usual reason): the WHOLE removable disk
//! the device is on is erased and gets one MBR partition table with one primary partition (type `0x0c`, FAT32: every player reads
//! it; or `0x07`, exFAT), a label, and is mounted again.
//!
//! Every safeguard lives here, not in the dialog ([`check`], run again by [`format`] on the disk as it is at that moment): only
//! removable USB sticks and SD cards (a loop image counts: it is a file of the user's), never the disk that holds `/`, the home
//! folder, the app or its data, never a read-only or write-protected one, never while a job writes to it, never with a volume the
//! user was not shown ([`Request::seen`]), only when the user typed the device's label (or `ERASE` when it has none), no exFAT on
//! an SD card (players refuse those), and nothing a plain MBR cannot hold (over 2 TB).
//!
//! The words for the user are the app's (`src/app/device/format.rs` turns an [`Error`] into a cause / consequence / action text),
//! so everything here returns data. Children: `linux` (udisks2 over D-Bus: no root, polkit asks if it must), `macos`
//! (`diskutil eraseDisk`), `windows` (diskpart; FAT32 above 32 GB with the `fatfs` crate, which needs the elevated relaunch
//! `openfader --format-disk`), [`parse`] (the parsers of the OS tools' answers and the diskpart script, compiled everywhere so the
//! tests run on every OS).

pub mod parse;

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod os;

#[cfg(windows)]
pub use os::elevated_main;

use super::{Device, Kind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file system the new partition gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fs {
    /// Plays on every player, also above 32 GB (the built-in formatter makes FAT32 of any size).
    Fat32,
    /// Older players cannot read it; never on an SD card.
    ExFat,
}

/// An MBR disk holds 2^32 sectors of 512 bytes.
const MBR_MAX: u64 = 2_199_023_255_040;

/// What the new volume is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub fs: Fs,
    /// The volume label; [`Spec::label`] checks and normalises it.
    pub label: String,
}

impl Spec {
    /// The label as it is written: trimmed, upper case on FAT32 (what FAT stores). At most 11 ASCII characters without
    /// `" * + , . / : ; < = > ? [ \ ] |`; empty is allowed.
    pub fn label(&self) -> Result<String, Error> {
        let l = self.label.trim();
        let bad =
            |c: char| !c.is_ascii() || c.is_ascii_control() || "\"*+,./:;<=>?[\\]|".contains(c);
        if l.chars().count() > 11 || l.chars().any(bad) {
            return Err(Error::Label(self.fs));
        }
        Ok(match self.fs {
            Fs::Fat32 => l.to_uppercase(),
            Fs::ExFat => l.to_owned(),
        })
    }
}

/// One volume (partition with a file system) on the disk, as shown in the confirmation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Linux `sdb1`, macOS `disk4s1`, Windows `E:` (`#2` without a drive letter).
    pub name: String,
    pub label: String,
    /// `vfat`, `exfat`, `ntfs`, … as the OS names it; empty: none known.
    pub fs: String,
    pub size: u64,
    pub mount: Option<PathBuf>,
}

/// The whole disk a device is on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    /// What the OS tool is given: `/dev/sdb`, `/dev/disk4`, the disk number on Windows.
    pub id: String,
    pub model: String,
    pub size: u64,
    pub removable: bool,
    /// It holds `/`, the home folder, the app or its data (or is swap).
    pub system: bool,
    pub volumes: Vec<Volume>,
}

impl Disk {
    /// What the user types to confirm: the label of the volume `dev` is, `ERASE` when it has none.
    pub fn word(&self, dev: &Device) -> String {
        let at = dev.source.strip_prefix("/dev/").unwrap_or(&dev.source);
        self.volumes
            .iter()
            .find(|v| v.name == at)
            .map(|v| v.label.trim())
            .filter(|l| !l.is_empty())
            .map_or_else(|| "ERASE".into(), str::to_owned)
    }

    /// The volumes that are mounted.
    pub fn mounted(&self) -> impl Iterator<Item = &Volume> {
        self.volumes.iter().filter(|v| v.mount.is_some())
    }
}

/// The user's decision, as the dialog collected it.
pub struct Request<'a> {
    pub spec: Spec,
    /// What the user typed in the confirmation.
    pub typed: &'a str,
    /// The names of the volumes the confirmation listed ([`Volume::name`]).
    pub seen: &'a [String],
    /// A job writes to the device.
    pub busy: bool,
}

/// What stopped a format, or what went wrong in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The device is not on a disk this can format (a pretend folder, a network drive).
    NoDisk,
    System,
    NotRemovable,
    ReadOnly,
    /// A job of the app writes to the device.
    JobBusy,
    /// A volume was not in the list the user confirmed (its name).
    Unseen(String),
    /// The typed word was not this one.
    Confirm(String),
    ExFatOnSd,
    TooBig,
    /// The label does not suit this file system.
    Label(Fs),
    /// The system refused (polkit said no, the password prompt was dismissed, UAC was declined); `erased`: it did so after the
    /// disk's old contents were already gone.
    Denied {
        erased: bool,
    },
    /// Another program uses a volume of the disk (its name): nothing was erased.
    InUse(String),
    /// A program the formatter needs is not there.
    Missing(&'static str),
    /// A step failed; `erased`: the disk's old contents are already gone.
    Failed {
        step: Step,
        detail: String,
        erased: bool,
    },
}

/// The steps of a format, for the progress line and the error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Reading what is on the disk.
    Read,
    Unmount,
    Table,
    Partition,
    Mount,
}

/// A finished format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    /// The new volume as [`Device::source`] names it (the app selects the device with it).
    pub source: String,
    pub fs: Fs,
    pub label: String,
    /// FAT32 was asked for and the system would not make it (a small card needs an extra authorisation for that): the FAT
    /// variant the volume got instead (`FAT16`); every player reads it.
    pub short: Option<String>,
}

/// The disk `dev` is on with its volumes (model, size, every volume with its label), for the confirmation and the guards.
pub fn describe(dev: &Device) -> Result<Disk, Error> {
    os::describe(dev)
}

/// Whether one of `mounts` (a volume's mount point) holds one of the `protected` paths, or is one.
pub fn holds_system(mounts: &[&Path], protected: &[PathBuf]) -> bool {
    mounts
        .iter()
        .any(|m| protected.iter().any(|p| p.starts_with(m)))
}

/// The places that must never be erased: the system folders, the home folder, the app, its data and settings.
pub fn protected() -> Vec<PathBuf> {
    let fixed = [
        "/",
        "/boot",
        "/boot/efi",
        "/usr",
        "/var",
        "/etc",
        "/home",
        "/opt",
        "/nix",
    ];
    let mut v: Vec<PathBuf> = fixed.iter().map(PathBuf::from).collect();
    v.extend(crate::paths::home());
    v.extend(std::env::current_exe().ok());
    v.extend(crate::paths::data_dir());
    v.extend(crate::paths::config_dir());
    v
}

/// The safeguards over a described disk; the label as it will be written.
pub fn check(dev: &Device, disk: &Disk, req: &Request) -> Result<String, Error> {
    if dev.read_only {
        return Err(Error::ReadOnly);
    }
    if disk.system {
        return Err(Error::System);
    }
    if !disk.removable {
        return Err(Error::NotRemovable);
    }
    if req.spec.fs == Fs::ExFat && dev.kind == Kind::Sd {
        return Err(Error::ExFatOnSd);
    }
    if disk.size > MBR_MAX {
        return Err(Error::TooBig);
    }
    let label = req.spec.label()?;
    if req.busy {
        return Err(Error::JobBusy);
    }
    if let Some(v) = disk.volumes.iter().find(|v| !req.seen.contains(&v.name)) {
        return Err(Error::Unseen(v.name.clone()));
    }
    let word = disk.word(dev);
    if req.typed.trim() != word {
        return Err(Error::Confirm(word));
    }
    Ok(label)
}

/// Erases the whole disk `dev` is on and makes it one MBR partition with a fresh file system, mounted again; `step` is told
/// each step as it starts. Everything [`check`] says is checked again on the disk as it is now.
pub fn format(dev: &Device, req: &Request, step: &dyn Fn(Step)) -> Result<Done, Error> {
    let disk = os::describe(dev)?;
    let label = check(dev, &disk, req)?;
    let done = os::format(dev, &disk, req.spec.fs, &label, step)?;
    super::forget(&dev.mount);
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Support;

    fn dev(source: &str, kind: Kind, ro: bool) -> Device {
        Device {
            source: source.into(),
            read_only: ro,
            ..Device::new(
                "id".into(),
                "/run/media/dj/KEY".into(),
                "vfat".into(),
                kind,
                Support::Ok,
            )
        }
    }

    fn vol(name: &str, label: &str, mounted: bool) -> Volume {
        Volume {
            name: name.into(),
            label: label.into(),
            fs: "vfat".into(),
            size: 1 << 30,
            mount: mounted.then(|| format!("/run/media/dj/{name}").into()),
        }
    }

    fn disk(volumes: Vec<Volume>) -> Disk {
        Disk {
            id: "/dev/sdb".into(),
            model: "Ultra".into(),
            size: 16_000_000_000,
            removable: true,
            system: false,
            volumes,
        }
    }

    fn ask<'a>(fs: Fs, typed: &'a str, seen: &'a [String]) -> Request<'a> {
        Request {
            spec: Spec {
                fs,
                label: "dj".into(),
            },
            typed,
            seen,
            busy: false,
        }
    }

    #[test]
    fn the_guards_refuse_before_anything_is_erased() {
        let one = vec![vol("sdb1", "MY KEY", true)];
        let seen = vec!["sdb1".to_owned()];
        let d = dev("/dev/sdb1", Kind::Usb, false);
        let ok = disk(one.clone());
        assert_eq!(
            check(&d, &ok, &ask(Fs::Fat32, "MY KEY", &seen)),
            Ok("DJ".into())
        );
        assert_eq!(
            check(&d, &ok, &ask(Fs::Fat32, " MY KEY ", &seen)),
            Ok("DJ".into()),
            "spaces around are ignored"
        );

        let table: Vec<(&str, Device, Disk, Request, Error)> = vec![
            (
                "system disk",
                d.clone(),
                Disk {
                    system: true,
                    ..ok.clone()
                },
                ask(Fs::Fat32, "MY KEY", &seen),
                Error::System,
            ),
            (
                "not removable",
                d.clone(),
                Disk {
                    removable: false,
                    ..ok.clone()
                },
                ask(Fs::Fat32, "MY KEY", &seen),
                Error::NotRemovable,
            ),
            (
                "read-only",
                dev("/dev/sdb1", Kind::Usb, true),
                ok.clone(),
                ask(Fs::Fat32, "MY KEY", &seen),
                Error::ReadOnly,
            ),
            (
                "exFAT card",
                dev("/dev/mmcblk0p1", Kind::Sd, false),
                ok.clone(),
                ask(Fs::ExFat, "MY KEY", &seen),
                Error::ExFatOnSd,
            ),
            (
                "over 2 TB",
                d.clone(),
                Disk {
                    size: 4_000_000_000_000,
                    ..ok.clone()
                },
                ask(Fs::Fat32, "MY KEY", &seen),
                Error::TooBig,
            ),
            (
                "label too long",
                d.clone(),
                ok.clone(),
                Request {
                    spec: Spec {
                        fs: Fs::Fat32,
                        label: "TWELVE CHARS".into(),
                    },
                    ..ask(Fs::Fat32, "MY KEY", &seen)
                },
                Error::Label(Fs::Fat32),
            ),
            (
                "label with a colon",
                d.clone(),
                ok.clone(),
                Request {
                    spec: Spec {
                        fs: Fs::ExFat,
                        label: "A:B".into(),
                    },
                    ..ask(Fs::Fat32, "MY KEY", &seen)
                },
                Error::Label(Fs::ExFat),
            ),
            (
                "a job writes",
                d.clone(),
                ok.clone(),
                Request {
                    busy: true,
                    ..ask(Fs::Fat32, "MY KEY", &seen)
                },
                Error::JobBusy,
            ),
            (
                "wrong word",
                d.clone(),
                ok.clone(),
                ask(Fs::Fat32, "my key", &seen),
                Error::Confirm("MY KEY".into()),
            ),
            (
                "no word",
                d.clone(),
                ok.clone(),
                ask(Fs::Fat32, "", &seen),
                Error::Confirm("MY KEY".into()),
            ),
        ];
        for (name, d, disk, req, want) in table {
            assert_eq!(check(&d, &disk, &req), Err(want), "{name}");
        }
    }

    #[test]
    fn every_volume_must_have_been_listed() {
        let d = dev("/dev/sdb1", Kind::Usb, false);
        let two = disk(vec![
            vol("sdb1", "MUSIC", true),
            vol("sdb2", "BACKUP", true),
        ]);
        let first = vec!["sdb1".to_owned()];
        assert_eq!(
            check(&d, &two, &ask(Fs::Fat32, "MUSIC", &first)),
            Err(Error::Unseen("sdb2".into()))
        );
        let both = vec!["sdb1".to_owned(), "sdb2".to_owned()];
        assert!(check(&d, &two, &ask(Fs::Fat32, "MUSIC", &both)).is_ok());
        let unmounted = disk(vec![
            vol("sdb1", "MUSIC", true),
            vol("sdb2", "BACKUP", false),
        ]);
        assert_eq!(
            check(&d, &unmounted, &ask(Fs::Fat32, "MUSIC", &first)),
            Err(Error::Unseen("sdb2".into())),
            "unmounted ones count too"
        );
    }

    #[test]
    fn a_device_without_a_label_is_confirmed_with_erase() {
        let d = dev("/dev/sdb1", Kind::Usb, false);
        let seen = vec!["sdb1".to_owned()];
        let blank = disk(vec![vol("sdb1", "", true)]);
        assert_eq!(blank.word(&d), "ERASE");
        assert!(check(&d, &blank, &ask(Fs::Fat32, "ERASE", &seen)).is_ok());
        assert_eq!(
            check(&d, &blank, &ask(Fs::Fat32, "", &seen)),
            Err(Error::Confirm("ERASE".into()))
        );
    }

    #[test]
    fn labels_are_fat_labels() {
        let l = |fs, s: &str| {
            Spec {
                fs,
                label: s.into(),
            }
            .label()
        };
        assert_eq!(l(Fs::Fat32, " dj set "), Ok("DJ SET".into()));
        assert_eq!(l(Fs::ExFat, "Dj Set"), Ok("Dj Set".into()));
        assert_eq!(l(Fs::Fat32, ""), Ok(String::new()));
        assert_eq!(l(Fs::Fat32, "ELEVEN CHARS"), Err(Error::Label(Fs::Fat32)));
        assert_eq!(l(Fs::Fat32, "CAFÉ"), Err(Error::Label(Fs::Fat32)));
        assert_eq!(l(Fs::Fat32, "a|b"), Err(Error::Label(Fs::Fat32)));
    }

    #[test]
    fn the_system_disk_is_the_one_that_holds_a_protected_place() {
        let protected: Vec<PathBuf> = ["/", "/home", "/home/dj", "/usr/bin/openfader"]
            .map(PathBuf::from)
            .into();
        let m = |s: &'static str| Path::new(s);
        assert!(holds_system(&[m("/")], &protected));
        assert!(holds_system(
            &[m("/run/media/dj/KEY"), m("/home")],
            &protected
        ));
        assert!(!holds_system(&[m("/run/media/dj/KEY")], &protected));
        let on_stick: Vec<PathBuf> = vec!["/run/media/dj/KEY/app/openfader".into()];
        assert!(
            holds_system(&[m("/run/media/dj/KEY")], &on_stick),
            "the app runs from the stick"
        );
    }
}
