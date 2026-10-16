//! Finding the removable drives on Windows: every drive letter of `GetLogicalDrives` whose `GetDriveTypeW` is `DRIVE_REMOVABLE`
//! (a USB stick, an SD card in a reader) and that holds a medium. `GetVolumeInformationW` gives the label, the file system
//! (`FAT32`, `exFAT`, `NTFS`, lower-cased here, which is how [`Support::of`] reads them), the read-only flag and the volume serial
//! that [`Device::id`] uses (the `XXXX-XXXX` Explorer shows). [`Device::source`] is the drive letter (`E:`), which the
//! eject child opens as `\\.\E:`.
// ponytail: a USB hard disk or SSD reports DRIVE_FIXED, so it is not listed; asking the storage bus type (IOCTL_STORAGE_QUERY_PROPERTY)
// would add it.

use super::{Device, Kind, Support};
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};

/// `GetDriveTypeW`'s answer for removable media.
const DRIVE_REMOVABLE: u32 = 2;
/// `GetVolumeInformationW` flag: the volume is read-only.
const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The text of a NUL-terminated UTF-16 buffer.
fn text(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `(label, file system, flags, serial)` of the volume at `root` (`E:\`); `None` without a medium.
fn volume(root: &str) -> Option<(String, String, u32, u32)> {
    let r = wide(root);
    let (mut label, mut fs) = ([0u16; 261], [0u16; 261]);
    let (mut serial, mut max, mut flags) = (0, 0, 0);
    // SAFETY: `r` is NUL-terminated, the buffers are as long as the sizes passed, the other out-pointers are live locals.
    let ok = unsafe {
        GetVolumeInformationW(
            r.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            &mut max,
            &mut flags,
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    (ok != 0).then(|| (text(&label), text(&fs).to_lowercase(), flags, serial))
}

/// The removable drives with a medium in them.
pub(super) fn mounted() -> Vec<Device> {
    // SAFETY: no arguments.
    let letters = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| letters >> i & 1 == 1)
        .filter_map(|i| {
            let letter = (b'A' + i) as char;
            let root = format!("{letter}:\\");
            // SAFETY: the wide string is NUL-terminated and lives through the call.
            if unsafe { GetDriveTypeW(wide(&root).as_ptr()) } != DRIVE_REMOVABLE {
                return None;
            }
            let (label, fs, flags, serial) = volume(&root)?;
            let support = Support::of(&fs, Kind::Usb);
            Some(Device {
                source: format!("{letter}:"),
                label: if label.is_empty() {
                    format!("{letter}:")
                } else {
                    label
                },
                read_only: flags & FILE_READ_ONLY_VOLUME != 0,
                ..Device::new(
                    format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF),
                    root.into(),
                    fs,
                    Kind::Usb,
                    support,
                )
            })
        })
        .collect()
}
