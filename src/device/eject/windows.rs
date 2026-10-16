//! Eject on Windows: the volume (`\\.\E:`) is opened, locked (this fails while a file on it is open: the "in use" case),
//! dismounted, told that its media may be removed and ejected, the sequence of every safe-removal tool. `Device::source` is the
//! drive letter (`E:`), as the drive scan of `device` sets it.

use super::Device;
use std::{ffi::c_void, ptr::null, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    System::{
        Ioctl::{
            FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_STORAGE_EJECT_MEDIA,
            IOCTL_STORAGE_MEDIA_REMOVAL, PREVENT_MEDIA_REMOVAL,
        },
        IO::DeviceIoControl,
    },
};

/// Sends `code` (with `input`, if any) to the open volume `h`.
fn ioctl(h: *mut c_void, code: u32, input: Option<&PREVENT_MEDIA_REMOVAL>) -> bool {
    let (ptr, len) = input.map_or((null(), 0), |i| {
        (
            i as *const PREVENT_MEDIA_REMOVAL as *const c_void,
            size_of::<PREVENT_MEDIA_REMOVAL>() as u32,
        )
    });
    let mut returned = 0;
    // SAFETY: `h` is an open handle, `ptr` points at `len` readable bytes (or is null with length 0), no output buffer is asked for.
    unsafe { DeviceIoControl(h, code, ptr, len, null_mut(), 0, &mut returned, null_mut()) != 0 }
}

/// Ejects the removable drive `d` is.
pub fn eject(d: &Device) -> Result<String, String> {
    if !(d.source.len() == 2 && d.source.ends_with(':')) {
        return Err(format!(
            "{} is not a drive letter of a removable drive",
            d.label
        ));
    }
    let path: Vec<u16> = format!(r"\\.\{}", d.source)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: `path` is a NUL-terminated wide string that outlives the call.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(format!("{}: {}", d.source, std::io::Error::last_os_error()));
    }
    let done = if !ioctl(h, FSCTL_LOCK_VOLUME, None) {
        Err(format!(
            "{} is in use: close what reads it and try again",
            d.label
        ))
    } else if !ioctl(h, FSCTL_DISMOUNT_VOLUME, None) {
        Err(format!(
            "{}: cannot dismount: {}",
            d.label,
            std::io::Error::last_os_error()
        ))
    } else {
        // Media that cannot be ejected (a stick has no tray) is dismounted all the same: safe to pull out.
        ioctl(
            h,
            IOCTL_STORAGE_MEDIA_REMOVAL,
            Some(&PREVENT_MEDIA_REMOVAL {
                PreventMediaRemoval: false,
            }),
        );
        ioctl(h, IOCTL_STORAGE_EJECT_MEDIA, None);
        Ok(format!("{} dismounted: safe to remove", d.label))
    };
    // SAFETY: `h` was opened above and is closed once.
    unsafe { CloseHandle(h) };
    done
}
