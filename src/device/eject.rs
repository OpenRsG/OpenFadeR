//! Eject (base): make a device safe to pull out, by each OS's own means. Children: `linux` (udisks: flush, unmount, power off),
//! `windows` (lock, dismount and eject the volume with `DeviceIoControl`), `macos` (`diskutil eject`).

use super::Device;

#[cfg_attr(target_os = "linux", path = "eject/linux.rs")]
#[cfg_attr(target_os = "macos", path = "eject/macos.rs")]
#[cfg_attr(windows, path = "eject/windows.rs")]
mod os;

/// Flushes and unmounts `d` and powers its disk off where the OS can. Returns what was done, for the status line; the error is
/// the failing step's message. The caller must not eject while a job writes to it.
pub fn eject(d: &Device) -> Result<String, String> {
    os::eject(d)
}
