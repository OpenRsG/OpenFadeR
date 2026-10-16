//! Eject on macOS: `diskutil eject <mount point>` unmounts every volume of the disk and powers it down; its own message
//! is returned when it fails (e.g. "at least one volume could not be unmounted" while something still reads it).

use super::Device;
use std::process::Command;

/// Ejects the disk `d` is on.
pub fn eject(d: &Device) -> Result<String, String> {
    let out = Command::new("diskutil")
        .arg("eject")
        .arg(&d.mount)
        .output()
        .map_err(|e| format!("diskutil: {e}"))?;
    if out.status.success() {
        return Ok(format!("{} ejected: safe to remove", d.label));
    }
    let said = [&out.stderr, &out.stdout]
        .map(|b| String::from_utf8_lossy(b).trim().to_owned())
        .join("\n");
    Err(said.trim().to_owned())
}
