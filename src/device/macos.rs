//! Finding the removable volumes on macOS: every local file system mounted below `/Volumes` (`getfsstat`: where Finder shows
//! sticks, cards and disk images; the boot volume is mounted at `/`, network shares are not local). [`Device::fs`] is the
//! kernel's file-system name (`msdos` for FAT, `exfat`, `hfs`, `apfs`, `ntfs`), [`Device::source`] the BSD device
//! (`/dev/disk4s1`), [`Device::id`] the mount path (the volume UUID would need DiskArbitration). An SD card cannot be told from a
//! stick here, so every device is [`Kind::Usb`].

use super::{Device, Kind, Support};
use std::ffi::CStr;

fn text(c: &[libc::c_char]) -> String {
    // SAFETY: the kernel fills these fixed buffers with NUL-terminated strings.
    unsafe { CStr::from_ptr(c.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// The mounted file systems.
fn mounts() -> Vec<libc::statfs> {
    // SAFETY: a null buffer asks only for the count.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n <= 0 {
        return Vec::new();
    }
    let mut v: Vec<libc::statfs> = Vec::with_capacity(n as usize);
    let bytes = n as usize * size_of::<libc::statfs>();
    // SAFETY: `v` has room for `n` entries (`bytes` is their size); only the `got` entries the kernel wrote are kept.
    unsafe {
        let got = libc::getfsstat(v.as_mut_ptr(), bytes as libc::c_int, libc::MNT_NOWAIT);
        v.set_len(got.clamp(0, n) as usize);
    }
    v
}

/// The removable volumes mounted now.
pub(super) fn mounted() -> Vec<Device> {
    let mut out: Vec<Device> = mounts()
        .iter()
        .filter(|m| m.f_flags & libc::MNT_LOCAL as u32 != 0)
        .filter_map(|m| {
            let point = text(&m.f_mntonname);
            point.starts_with("/Volumes/").then(|| {
                let fs = text(&m.f_fstypename);
                let support = Support::of(&fs, Kind::Usb);
                Device {
                    source: text(&m.f_mntfromname),
                    read_only: m.f_flags & libc::MNT_RDONLY as u32 != 0,
                    ..Device::new(point.clone(), point.into(), fs, Kind::Usb, support)
                }
            })
        })
        .collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}
