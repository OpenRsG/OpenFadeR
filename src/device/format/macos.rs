//! Formatting on macOS with `diskutil`: `diskutil eraseDisk FAT32|ExFAT <NAME> MBRFormat /dev/diskN` unmounts every volume of the
//! disk, writes a new MBR with one primary partition (type `0x0c` / `0x07`), formats and mounts it, and needs no password for an
//! external disk. The disk and its volumes come from `diskutil info -plist` / `diskutil list -plist` of the whole disk.

use super::{parse, Disk, Done, Error, Fs, Step};
use crate::device::{os::forget_tables, Device};
use std::{path::Path, process::Command};

/// `diskutil args`: its stdout, or the text it said on failure; a missing `diskutil` is `Err(None)`.
fn run(args: &[&str]) -> Result<String, Option<String>> {
    let out = Command::new("diskutil")
        .args(args)
        .output()
        .map_err(|_| None)?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let said = [&out.stderr, &out.stdout].map(|b| String::from_utf8_lossy(b).trim().to_owned());
    Err(Some(
        said.into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_else(|| "diskutil failed".into()),
    ))
}

fn read_failed(e: Option<String>) -> Error {
    e.map_or(Error::Missing("diskutil"), |detail| Error::Failed {
        step: Step::Read,
        detail,
        erased: false,
    })
}

/// The disk `dev` is on with its volumes.
pub(super) fn describe(dev: &Device) -> Result<Disk, Error> {
    let node = format!(
        "/dev/{}",
        parse::mac_whole(&dev.source).ok_or(Error::NoDisk)?
    );
    let info = run(&["info", "-plist", &node]).map_err(read_failed)?;
    let list = run(&["list", "-plist", &node]).map_err(read_failed)?;
    let mut disk = parse::disk_from_diskutil(&info, &list).ok_or_else(|| Error::Failed {
        step: Step::Read,
        detail: "diskutil answered something unreadable".into(),
        erased: false,
    })?;
    let mounts: Vec<&Path> = disk
        .volumes
        .iter()
        .filter_map(|v| v.mount.as_deref())
        .collect();
    disk.system = super::holds_system(&mounts, &super::protected());
    Ok(disk)
}

pub(super) fn format(
    _dev: &Device,
    disk: &Disk,
    fs: Fs,
    label: &str,
    step: &dyn Fn(Step),
) -> Result<Done, Error> {
    step(Step::Table);
    let kind = match fs {
        Fs::Fat32 => "FAT32",
        Fs::ExFat => "ExFAT",
    };
    // diskutil wants a name; FAT32 names are upper case, 11 characters at most (checked by the caller).
    let name = if label.is_empty() { "UNTITLED" } else { label };
    let erased = run(&["eraseDisk", kind, name, "MBRFormat", &disk.id]).map_err(|e| match e {
        None => Error::Missing("diskutil"),
        Some(detail) if detail.to_lowercase().contains("unmount") => Error::InUse(
            disk.mounted()
                .next()
                .map_or_else(|| disk.id.clone(), |v| v.name.clone()),
        ),
        Some(detail) => Error::Failed {
            step: Step::Table,
            detail,
            erased: true,
        },
    });
    forget_tables();
    erased?;
    Ok(Done {
        source: format!("{}s1", disk.id),
        fs,
        label: label.to_owned(),
        short: None,
    })
}
