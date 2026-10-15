//! A stick that was just plugged in: mounting the file systems nobody mounted (no desktop automounter running), the way a
//! file manager does, and reading its library files with progress.
//!
//! A partition is waiting when udev calls it a file system (`ID_FS_USAGE=filesystem` in `/run/udev/data/b<maj>:<min>`), it
//! is not marked `UDISKS_IGNORE=1`, its disk is removable or on USB ([`Sys::disk`]), and no mount has it as source.
//! [`mount`] runs `udisksctl mount` (udisks2; no root: polkit lets the active session mount removable media).

use super::{Device, Linux, Sys, parse_mountinfo};
use std::{io::Read, path::PathBuf, process::Command};

/// One file system waiting to be mounted.
#[derive(Clone, Debug, PartialEq)]
pub struct Unmounted {
    /// Partition, e.g. `sda1`.
    pub part: String,
    /// Volume label, else the partition name.
    pub label: String,
}

/// Value of `E:<key>=` in a udev database file.
fn prop<'a>(udev: &'a str, key: &str) -> Option<&'a str> {
    udev.lines()
        .find_map(|l| l.strip_prefix("E:")?.strip_prefix(key)?.strip_prefix('='))
}

/// The partitions of `parts` (name, udev database text) that wait for a mount.
fn waiting(mountinfo: &str, parts: Vec<(String, String)>, sys: &dyn Sys) -> Vec<Unmounted> {
    let mounted: Vec<String> = parse_mountinfo(mountinfo)
        .into_iter()
        .map(|m| m.source)
        .collect();
    parts
        .into_iter()
        .filter(|(p, u)| {
            prop(u, "ID_FS_USAGE") == Some("filesystem")
                && prop(u, "UDISKS_IGNORE") != Some("1")
                && !mounted.iter().any(|s| s.strip_prefix("/dev/") == Some(p))
                && sys.disk(p).is_some_and(|d| d.1)
        })
        .map(|(p, u)| Unmounted {
            label: prop(&u, "ID_FS_LABEL")
                .filter(|l| !l.is_empty())
                .unwrap_or(&p)
                .to_owned(),
            part: p,
        })
        .collect()
}

/// The removable file systems plugged in but not mounted.
pub fn unmounted() -> Vec<Unmounted> {
    let parts = std::fs::read_dir("/sys/class/block")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dev = std::fs::read_to_string(e.path().join("dev")).ok()?;
            let udev = std::fs::read_to_string(format!("/run/udev/data/b{}", dev.trim())).ok()?;
            Some((e.file_name().to_string_lossy().into_owned(), udev))
        })
        .collect();
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    waiting(&mountinfo, parts, &Linux)
}

/// Whether partition `part` is still plugged in.
pub fn plugged(part: &str) -> bool {
    std::path::Path::new("/sys/class/block").join(part).exists()
}

/// Mounts `part` (`udisksctl mount --no-user-interaction -b /dev/<part>`); the folder it is mounted at, or udisks' reason.
pub fn mount(part: &str) -> Result<PathBuf, String> {
    let out = Command::new("udisksctl")
        .args(["mount", "--no-user-interaction", "-b"])
        .arg(format!("/dev/{part}"))
        .output()
        .map_err(|e| format!("udisksctl: {e} (install udisks2, or mount it in a file manager)"))?;
    // "Mounted /dev/sda1 at /run/media/dj/KEY" (older udisks end it with a '.').
    let said = String::from_utf8_lossy(&out.stdout);
    match said.split_once(" at ") {
        Some((_, at)) if out.status.success() => Ok(at.trim().trim_end_matches('.').into()),
        // "Error mounting /dev/sda1: GDBus.Error:org.freedesktop.UDisks2.Error.NotAuthorized…: Not authorized …": the last part reads.
        _ => Err(String::from_utf8_lossy(&out.stderr)
            .trim()
            .rsplit(": ")
            .next()
            .unwrap_or("udisksctl failed")
            .to_owned()),
    }
}

/// Reads the device's library files (`DeviceInfo::prefetch`) once in 1 MiB chunks so opening the library afterwards comes from
/// the page cache; `progress(done, total)` bytes after each chunk.
pub fn prefetch(d: &Device, mut progress: impl FnMut(u64, u64)) {
    let files: Vec<(PathBuf, u64)> = d
        .info
        .prefetch
        .iter()
        .map(|f| d.mount.join(f))
        .filter_map(|p| Some((p.clone(), p.metadata().ok()?.len())))
        .collect();
    let total = files.iter().map(|f| f.1).sum();
    let (mut done, mut buf) = (0, vec![0; 1 << 20]);
    for (path, _) in files {
        let Ok(mut f) = std::fs::File::open(path) else {
            continue;
        };
        while let Ok(n @ 1..) = f.read(&mut buf) {
            done += n as u64;
            progress(done, total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Sys for Fake {
        fn disk(&self, part: &str) -> Option<(String, bool)> {
            Some((
                part[..3].to_owned(),
                part.starts_with("sdb") || part.starts_with("sdc"),
            ))
        }
        fn uuid(&self, _: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn only_unmounted_removable_file_systems_wait() {
        let fs = |label: &str| {
            format!("E:ID_FS_USAGE=filesystem\nE:ID_FS_TYPE=vfat\nE:ID_FS_LABEL={label}\n")
        };
        let parts = vec![
            ("sda2".into(), fs("root")),                      // fixed disk
            ("sdb1".into(), fs("KINGSTON")),                  // waits
            ("sdb2".into(), "E:ID_FS_USAGE=crypto\n".into()), // not a file system
            ("sdc1".into(), fs("")),                          // waits, no label
            ("sdc2".into(), fs("x") + "E:UDISKS_IGNORE=1\n"), // hidden by udev rules
            ("sdc3".into(), fs("MOUNTED")),                   // already mounted
        ];
        let info = "80 24 8:35 / /run/media/dj/MOUNTED rw - vfat /dev/sdc3 rw\n";
        assert_eq!(
            waiting(info, parts, &Fake),
            [
                Unmounted {
                    part: "sdb1".into(),
                    label: "KINGSTON".into()
                },
                Unmounted {
                    part: "sdc1".into(),
                    label: "sdc1".into()
                },
            ]
        );
    }
}
