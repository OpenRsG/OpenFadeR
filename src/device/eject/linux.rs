//! Eject on Linux: flush, unmount, power the stick off.
//!
//! `udisksctl` is the udisks2 front end (`unmount -b <block>`, `power-off -b <disk>`; polkit lets the logged-in user do both for
//! removable media), so nothing here needs root or a bus crate. Without `udisksctl` a plain `umount <mount point>` is tried (works
//! for mounts the user may unmount). Every failing command's own message is returned, e.g. "target is busy".
//!
//! Not verified on a real device in this build (no stick may be ejected in tests): the command sequence and the error handling
//! are unit-tested against a recording runner.

use super::Device;
use std::process::Command;

/// Runs `tool args`: its stdout, or its stderr as the error; a missing tool is `Ok(None)`.
type Runner<'a> = &'a mut dyn FnMut(&str, &[&str]) -> Result<Option<String>, String>;

fn run(tool: &str, args: &[&str]) -> Result<Option<String>, String> {
    match Command::new(tool).args(args).output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{tool}: {e}")),
        Ok(o) if o.status.success() => {
            Ok(Some(String::from_utf8_lossy(&o.stdout).trim().to_owned()))
        }
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr);
            let e = e.trim();
            Err(if e.is_empty() {
                format!("{tool} {}: exit {}", args.join(" "), o.status)
            } else {
                e.to_owned()
            })
        }
    }
}

/// `/dev/sdb1` → `/dev/sdb`, `/dev/mmcblk0p1` → `/dev/mmcblk0`; `None` for a block device that is no partition (loop, whole disk).
fn disk_of(source: &str) -> Option<String> {
    let name = source.strip_prefix("/dev/")?;
    let digits = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let disk = if name.starts_with("mmcblk") || name.starts_with("nvme") {
        digits.strip_suffix('p')?
    } else {
        digits
    };
    (disk != name && !disk.is_empty() && !name.starts_with("loop")).then(|| format!("/dev/{disk}"))
}

/// Flushes and unmounts `d`, then powers its disk off (a stick or card reader; a loop image is detached instead). Returns what was
/// done, for the status line; the error is the failing command's message. The caller must not eject while a job writes to it.
pub fn eject(d: &Device) -> Result<String, String> {
    eject_via(d, &mut run)
}

fn eject_via(d: &Device, run: Runner) -> Result<String, String> {
    let mount = d.mount.to_string_lossy();
    let _ = run("sync", &["-f", &mount]); // the unmount flushes too; this makes a failing disk report before the unmount
    let unmounted = if d.source.starts_with("/dev/") {
        run("udisksctl", &["unmount", "-b", &d.source])?
    } else {
        None
    };
    if unmounted.is_none() {
        // No udisksctl (or no block device behind the mount): ask the kernel directly.
        run("umount", &[&mount])?.ok_or("neither udisksctl nor umount is installed")?;
    }
    let mut done = format!("{} unmounted", d.label);
    if d.source.starts_with("/dev/loop") {
        if let Ok(Some(_)) = run("udisksctl", &["loop-delete", "-b", &d.source]) {
            done.push_str(", loop device detached");
        }
    } else if let Some(disk) = disk_of(&d.source) {
        match run("udisksctl", &["power-off", "-b", &disk]) {
            Ok(Some(_)) => done.push_str(" and powered off: safe to remove"),
            Ok(None) => done.push_str(": safe to remove"),
            // Unmounted is what matters; some readers cannot be powered off.
            Err(e) => done.push_str(&format!(" (power-off failed: {e}); safe to remove")),
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Kind, Support};

    fn dev(source: &str, kind: Kind) -> Device {
        Device {
            source: source.into(),
            ..Device::new(
                "x".into(),
                "/run/media/dj/KEY".into(),
                "vfat".into(),
                kind,
                Support::Ok,
            )
        }
    }

    /// Records every command; `fail` makes the command whose text contains it fail with that message.
    fn recorder<'a>(
        log: &'a mut Vec<String>,
        fail: Option<(&'a str, &'a str)>,
        missing: &'a [&'a str],
    ) -> impl FnMut(&str, &[&str]) -> Result<Option<String>, String> + 'a {
        move |tool, args| {
            let line = format!("{tool} {}", args.join(" "));
            log.push(line.clone());
            if missing.contains(&tool) {
                return Ok(None);
            }
            match fail {
                Some((pat, msg)) if line.contains(pat) => Err(msg.into()),
                _ => Ok(Some(String::new())),
            }
        }
    }

    #[test]
    fn disk_names() {
        assert_eq!(disk_of("/dev/sdb1").as_deref(), Some("/dev/sdb"));
        assert_eq!(disk_of("/dev/mmcblk0p2").as_deref(), Some("/dev/mmcblk0"));
        assert_eq!(disk_of("/dev/nvme0n1p3").as_deref(), Some("/dev/nvme0n1"));
        assert_eq!(disk_of("/dev/loop3"), None);
        assert_eq!(disk_of("/dev/sdb"), None);
        assert_eq!(disk_of("tmpfs"), None);
    }

    #[test]
    fn a_stick_is_synced_unmounted_and_powered_off() {
        let mut log = Vec::new();
        let msg = eject_via(
            &dev("/dev/sdb1", Kind::Usb),
            &mut recorder(&mut log, None, &[]),
        )
        .unwrap();
        assert_eq!(
            log,
            [
                "sync -f /run/media/dj/KEY",
                "udisksctl unmount -b /dev/sdb1",
                "udisksctl power-off -b /dev/sdb"
            ]
        );
        assert_eq!(msg, "KEY unmounted and powered off: safe to remove");
    }

    #[test]
    fn a_busy_mount_surfaces_the_tools_message_and_never_powers_off() {
        let mut log = Vec::new();
        let e = eject_via(
            &dev("/dev/sdb1", Kind::Usb),
            &mut recorder(
                &mut log,
                Some(("unmount", "Error unmounting: target is busy")),
                &[],
            ),
        )
        .unwrap_err();
        assert_eq!(e, "Error unmounting: target is busy");
        assert!(!log.iter().any(|l| l.contains("power-off")), "{log:?}");
    }

    #[test]
    fn a_failing_power_off_is_a_note_not_an_error() {
        let mut log = Vec::new();
        let msg = eject_via(
            &dev("/dev/mmcblk0p1", Kind::Sd),
            &mut recorder(&mut log, Some(("power-off", "not authorized")), &[]),
        )
        .unwrap();
        assert!(
            msg.contains("power-off failed: not authorized") && msg.contains("safe to remove"),
            "{msg}"
        );
    }

    #[test]
    fn without_udisksctl_umount_is_used_and_without_both_it_is_an_error() {
        let mut log = Vec::new();
        eject_via(
            &dev("/dev/sdb1", Kind::Usb),
            &mut recorder(&mut log, None, &["udisksctl"]),
        )
        .unwrap();
        assert_eq!(
            log[..3],
            [
                "sync -f /run/media/dj/KEY",
                "udisksctl unmount -b /dev/sdb1",
                "umount /run/media/dj/KEY"
            ]
        );
        let e = eject_via(
            &dev("/dev/sdb1", Kind::Usb),
            &mut recorder(&mut Vec::new(), None, &["udisksctl", "umount"]),
        )
        .unwrap_err();
        assert!(e.contains("neither udisksctl nor umount"), "{e}");
    }

    #[test]
    fn a_loop_image_is_detached_instead_of_powered_off() {
        let mut log = Vec::new();
        let msg = eject_via(
            &dev("/dev/loop4", Kind::Usb),
            &mut recorder(&mut log, None, &[]),
        )
        .unwrap();
        assert_eq!(log.last().unwrap(), "udisksctl loop-delete -b /dev/loop4");
        assert!(msg.ends_with("loop device detached"), "{msg}");
    }
}
