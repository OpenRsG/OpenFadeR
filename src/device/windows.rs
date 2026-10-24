//! Finding the removable drives on Windows: every drive letter of `GetLogicalDrives` whose `GetDriveTypeW` is `DRIVE_REMOVABLE`
//! (a USB stick, an SD card in a reader) and that holds a medium. `GetVolumeInformationW` gives the label, the file system
//! (`FAT32`, `exFAT`, `NTFS`, lower-cased here, which is how [`Support::of`] reads them), the read-only flag and the volume serial
//! that [`Device::id`] uses (the `XXXX-XXXX` Explorer shows). [`Device::source`] is the drive letter (`E:`), which the
//! eject child opens as `\\.\E:`.
// ponytail: a USB hard disk or SSD reports DRIVE_FIXED, so it is not listed; asking the storage bus type (IOCTL_STORAGE_QUERY_PROPERTY)
// would add it.

use super::{format::parse, Device, Kind, Support};
use std::ptr::{null, null_mut};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{Ioctl::IOCTL_DISK_GET_PARTITION_INFO_EX, IO::DeviceIoControl},
};

/// `GetDriveTypeW`'s answer for removable media.
const DRIVE_REMOVABLE: u32 = 2;
/// `GetVolumeInformationW` flag: the volume is read-only.
const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Opens volume `drive` (`E:`) as `\\.\E:` with `access` (0 asks nothing but the right to query it); `None` when it cannot be.
pub(super) fn open_volume(drive: &str, access: u32) -> Option<HANDLE> {
    // SAFETY: the wide string is NUL-terminated and lives through the call; the other arguments are plain flags and nulls.
    let h = unsafe {
        CreateFileW(
            wide(&format!(r"\\.\{drive}")).as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE).then_some(h)
}

/// Asks open handle `h` for `code`; the answer fills `out`, the bytes written come back (`None` when the ioctl failed).
pub(super) fn ioctl_out(h: HANDLE, code: u32, out: &mut [u8]) -> Option<usize> {
    let mut returned = 0;
    // SAFETY: `h` is an open handle, `out` is writable for its whole length, nothing is sent in.
    let ok = unsafe {
        DeviceIoControl(
            h,
            code,
            null(),
            0,
            out.as_mut_ptr().cast(),
            out.len() as u32,
            &mut returned,
            null_mut(),
        )
    };
    (ok != 0).then_some(returned as usize)
}

/// `dos` / `gpt` of the disk volume `drive` is on: the partition style its own partition reports (an ioctl, no process);
/// empty while unknown.
fn table(drive: &str) -> String {
    let Some(h) = open_volume(drive, 0) else {
        return String::new();
    };
    let mut buf = [0u8; 256];
    let got = ioctl_out(h, IOCTL_DISK_GET_PARTITION_INFO_EX, &mut buf);
    // SAFETY: `h` was opened above and is closed once.
    unsafe { CloseHandle(h) };
    // PARTITION_INFORMATION_EX starts with the PARTITION_STYLE (0 MBR, 1 GPT, 2 raw).
    got.map_or_else(String::new, |_| {
        parse::table_from_style(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
    })
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
                table: table(&format!("{letter}:")),
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
