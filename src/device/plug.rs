//! A stick that was just plugged in: mounting the file systems nobody mounted (Linux, no desktop automounter running: the `udisks`
//! child), and reading its library files with progress. Windows and macOS mount a plugged-in volume themselves, so nothing ever
//! waits there.

use super::Device;
use std::{io::Read, path::PathBuf};

#[cfg(target_os = "linux")]
mod udisks;

#[cfg(target_os = "linux")]
pub use udisks::{mount, plugged, unmounted};

/// One file system waiting to be mounted.
#[derive(Clone, Debug, PartialEq)]
pub struct Unmounted {
    /// Partition, e.g. `sda1`.
    pub part: String,
    /// Volume label, else the partition name.
    pub label: String,
}

/// The removable file systems plugged in but not mounted: none, the OS mounts them.
#[cfg(not(target_os = "linux"))]
pub fn unmounted() -> Vec<Unmounted> {
    Vec::new()
}

/// Whether partition `part` is still plugged in; nothing is ever waiting here, so no.
#[cfg(not(target_os = "linux"))]
pub fn plugged(_: &str) -> bool {
    false
}

/// Mounting by hand is Linux's: on Windows and macOS the OS has mounted every volume it can.
#[cfg(not(target_os = "linux"))]
pub fn mount(_: &str) -> Result<PathBuf, String> {
    Err("the system mounts volumes by itself".to_owned())
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
