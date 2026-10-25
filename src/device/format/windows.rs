//! Formatting on Windows. Erasing a disk and writing to a raw volume need an administrator, so the app relaunches itself elevated
//! once (`ShellExecuteExW` with `runas`: one UAC prompt) as `openfader --format-disk <n> <fs> <letter> <result file> <label>`
//! ([`elevated_main`]) and waits for it. That process runs a diskpart script (select the disk, `clean`, `convert mbr`, one
//! primary partition of type `0x0c` / `0x07`, drive letter), then makes the file system: exFAT with `format X: /FS:exFAT /Q`,
//! FAT32 with the `fatfs` crate through `\\.\X:` (Windows' own tools refuse FAT32 above 32 GB; this one makes it on any size).
//! The disk and its volumes are described with one hidden PowerShell call (`Get-Disk` / `Get-Partition` / `Get-Volume`).
//!
//! Not run on Windows in this build (CI compiles it; the parsers, the script and the command line are unit-tested everywhere).

use super::{parse, Disk, Done, Error, Fs, Step};
use crate::device::{
    os::{ioctl_out, open_volume, wide},
    Device,
};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    mem::zeroed,
    os::windows::{io::FromRawHandle, process::CommandExt},
    path::Path,
    process::Command,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE},
    Storage::FileSystem::GetDriveTypeW,
    System::{
        Ioctl::{
            FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME,
            IOCTL_DISK_GET_DRIVE_GEOMETRY, IOCTL_DISK_GET_LENGTH_INFO,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
        },
        Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
        IO::DeviceIoControl,
    },
    UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
};

const DRIVE_REMOVABLE: u32 = 2;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ERROR_CANCELLED: u32 = 1223;
/// Exit codes of the elevated process.
const EXIT_ARGS: i32 = 1;
const EXIT_DISKPART: i32 = 2;
const EXIT_VOLUME: i32 = 3;
const EXIT_FORMAT: i32 = 4;

/// The upper-case drive letter of `X:\…` (Windows paths compare drive letters without case, `Path` does not).
fn upper_drive(p: &Path) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    match s.as_bytes() {
        [d, b':', ..] if d.is_ascii_lowercase() => {
            format!("{}{}", d.to_ascii_uppercase() as char, &s[1..]).into()
        }
        _ => p.to_path_buf(),
    }
}

/// Runs a hidden PowerShell command: its output, or why not.
fn powershell(script: &str) -> Result<String, Error> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|_| Error::Missing("PowerShell"))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(Error::Failed {
            step: Step::Read,
            detail,
            erased: false,
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The number of the physical disk volume `drive` (`E:`) is on.
fn disk_number(drive: &str) -> Option<u32> {
    let h = open_volume(drive, 0)?;
    let mut buf = [0u8; 12]; // STORAGE_DEVICE_NUMBER { DeviceType, DeviceNumber, PartitionNumber }
    let got = ioctl_out(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, &mut buf);
    // SAFETY: `h` was opened above and is closed once.
    unsafe { CloseHandle(h) };
    got.map(|_| u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]))
}

/// The disk `dev` is on with its volumes.
pub(super) fn describe(dev: &Device) -> Result<Disk, Error> {
    let drive = dev.source.as_str();
    if !(drive.len() == 2 && drive.ends_with(':')) {
        return Err(Error::NoDisk);
    }
    // SAFETY: the wide string is NUL-terminated and lives through the call.
    let removable =
        unsafe { GetDriveTypeW(wide(&format!("{drive}\\")).as_ptr()) } == DRIVE_REMOVABLE;
    let n = disk_number(drive).ok_or(Error::NoDisk)?;
    let json = powershell(&format!(
        "$d = Get-Disk -Number {n}; \
         $p = @(Get-Partition -DiskNumber {n} | ForEach-Object {{ $v = $_ | Get-Volume -ErrorAction SilentlyContinue; \
         [pscustomobject]@{{ Letter = [string]$_.DriveLetter; Number = $_.PartitionNumber; Size = $_.Size; \
         Label = $v.FileSystemLabel; Fs = $v.FileSystem }} }}); \
         [pscustomobject]@{{ Model = $d.FriendlyName; Size = $d.Size; Parts = $p }} | ConvertTo-Json -Compress -Depth 4"
    ))?;
    let mut disk = parse::disk_from_powershell(n, &json).ok_or_else(|| Error::Failed {
        step: Step::Read,
        detail: "PowerShell answered something unreadable".into(),
        erased: false,
    })?;
    disk.removable = removable;
    let mounts: Vec<_> = disk
        .volumes
        .iter()
        .filter_map(|v| v.mount.as_deref())
        .map(upper_drive)
        .collect();
    let mut protected: Vec<_> = super::protected().iter().map(|p| upper_drive(p)).collect();
    protected.extend(std::env::var_os("SystemRoot").map(|r| upper_drive(Path::new(&r))));
    let mounts: Vec<&Path> = mounts.iter().map(|m| m.as_path()).collect();
    disk.system = super::holds_system(&mounts, &protected);
    Ok(disk)
}

fn ioctl(h: *mut std::ffi::c_void, code: u32) -> bool {
    let mut returned = 0;
    // SAFETY: `h` is an open handle; no buffers are passed.
    unsafe {
        DeviceIoControl(
            h,
            code,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        ) != 0
    }
}

/// Whether volume `drive` can be locked now (nothing has a file open on it); it is unlocked again at once.
fn lockable(drive: &str) -> bool {
    let Some(h) = open_volume(drive, GENERIC_READ | GENERIC_WRITE) else {
        return true; // not openable here: the elevated process decides
    };
    let ok = ioctl(h, FSCTL_LOCK_VOLUME);
    if ok {
        ioctl(h, FSCTL_UNLOCK_VOLUME);
    }
    // SAFETY: `h` was opened above and is closed once.
    unsafe { CloseHandle(h) };
    ok
}

/// Runs `exe params` elevated (a UAC prompt) and waits; its exit code.
fn elevated(exe: &Path, params: &str) -> Result<u32, Error> {
    let (verb, file, params) = (wide("runas"), wide(&exe.to_string_lossy()), wide(params));
    // SAFETY: an all-zero SHELLEXECUTEINFOW is a valid empty request; the fields set below are filled before the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0; // SW_HIDE
                    // SAFETY: the strings it points at outlive the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: no arguments.
        return Err(match unsafe { GetLastError() } {
            ERROR_CANCELLED => Error::Denied { erased: false },
            e => Error::Failed {
                step: Step::Table,
                detail: format!("cannot start the elevated formatter (error {e})"),
                erased: false,
            },
        });
    }
    let mut code = 1;
    // SAFETY: `hProcess` is the process handle ShellExecuteExW returned (SEE_MASK_NOCLOSEPROCESS); it is closed once.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code)
}

pub(super) fn format(
    dev: &Device,
    disk: &Disk,
    fs: Fs,
    label: &str,
    step: &dyn Fn(Step),
) -> Result<Done, Error> {
    step(Step::Unmount);
    for v in disk.mounted() {
        if !lockable(&v.name) {
            return Err(Error::InUse(v.name.clone()));
        }
    }
    step(Step::Table);
    let letter = dev.source.chars().next().ok_or(Error::NoDisk)?;
    let number: u32 = disk.id.parse().map_err(|_| Error::NoDisk)?;
    let result = std::env::temp_dir().join(format!("openfader-format-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&result);
    let exe = std::env::current_exe().map_err(|e| Error::Failed {
        step: Step::Table,
        detail: e.to_string(),
        erased: false,
    })?;
    let code = elevated(
        &exe,
        &parse::elevated_args(number, fs, letter, &result.to_string_lossy(), label),
    )?;
    let detail = std::fs::read_to_string(&result)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let _ = std::fs::remove_file(&result);
    match code as i32 {
        0 => Ok(Done {
            source: format!("{letter}:"),
            fs,
            label: label.to_owned(),
            short: None,
        }),
        EXIT_VOLUME => Err(Error::Failed {
            step: Step::Mount,
            detail,
            erased: true,
        }),
        c => Err(Error::Failed {
            step: if c == EXIT_FORMAT {
                Step::Partition
            } else {
                Step::Table
            },
            detail,
            erased: c != EXIT_ARGS,
        }),
    }
}

// ---- the elevated process ----

/// A volume as a plain byte stream for `fatfs`: Windows wants every read and write on a raw volume to start on a sector and
/// span whole sectors, which `fatfs` does not do (the boot sector is written field by field), so a partial sector is read,
/// patched and written back. `len` is the volume's size: `fatfs` finds it with a seek to the end.
struct Raw {
    file: File,
    len: u64,
    pos: u64,
    sector: u64,
}

impl Raw {
    /// The sectors around `pos..pos + n`: where they start and their bytes.
    fn span(&mut self, n: u64) -> io::Result<(u64, Vec<u8>)> {
        let start = self.pos / self.sector * self.sector;
        let end = (self.pos + n).div_ceil(self.sector) * self.sector;
        let mut bytes = vec![0; (end - start) as usize];
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut bytes)?;
        Ok((start, bytes))
    }
}

impl Read for Raw {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos));
        if n == 0 {
            return Ok(0);
        }
        let (start, bytes) = self.span(n)?;
        let off = (self.pos - start) as usize;
        buf[..n as usize].copy_from_slice(&bytes[off..off + n as usize]);
        self.pos += n;
        Ok(n as usize)
    }
}

impl Write for Raw {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos));
        if n == 0 {
            return Ok(0);
        }
        if self.pos % self.sector == 0 && n % self.sector == 0 {
            self.file.seek(SeekFrom::Start(self.pos))?;
            self.file.write_all(&buf[..n as usize])?;
        } else {
            let (start, mut bytes) = self.span(n)?;
            let off = (self.pos - start) as usize;
            bytes[off..off + n as usize].copy_from_slice(&buf[..n as usize]);
            self.file.seek(SeekFrom::Start(start))?;
            self.file.write_all(&bytes)?;
        }
        self.pos += n;
        Ok(n as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for Raw {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let at = match to {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(d) => self.len as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        self.pos = u64::try_from(at)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.pos)
    }
}

/// Makes a FAT32 file system on volume `drive` (`E:`) whatever its size.
fn fat32(drive: &str, label: &str) -> Result<(), String> {
    let h = open_volume(drive, GENERIC_READ | GENERIC_WRITE)
        .ok_or_else(|| format!("cannot open {drive}: {}", io::Error::last_os_error()))?;
    // The file closes the handle when it drops.
    // SAFETY: `h` is an open handle nothing else owns.
    let file = unsafe { File::from_raw_handle(h) };
    let mut geometry = [0u8; 32]; // DISK_GEOMETRY: BytesPerSector is the u32 at 20
    let mut length = [0u8; 8]; // GET_LENGTH_INFORMATION
    ioctl_out(h, IOCTL_DISK_GET_DRIVE_GEOMETRY, &mut geometry)
        .ok_or("cannot read the sector size")?;
    ioctl_out(h, IOCTL_DISK_GET_LENGTH_INFO, &mut length).ok_or("cannot read the volume size")?;
    let sector = u64::from(u32::from_le_bytes([
        geometry[20],
        geometry[21],
        geometry[22],
        geometry[23],
    ]))
    .max(512);
    let len = i64::from_le_bytes(length) as u64 / sector * sector;
    if !ioctl(h, FSCTL_LOCK_VOLUME) {
        return Err("the new volume is in use".into());
    }
    let mut name = [b' '; 11];
    name[..label.len()].copy_from_slice(label.as_bytes());
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x1234_5678, |d| d.as_secs() as u32);
    let options = fatfs::FormatVolumeOptions::new()
        .fat_type(fatfs::FatType::Fat32)
        .bytes_per_sector(sector as u16)
        .total_sectors(u32::try_from(len / sector).map_err(|_| "the volume is too large")?)
        .volume_id(id)
        .volume_label(name);
    let mut raw = Raw {
        file,
        len,
        pos: 0,
        sector,
    };
    fatfs::format_volume(&mut raw, options).map_err(|e| e.to_string())?;
    raw.flush().map_err(|e| e.to_string())?;
    ioctl(h, FSCTL_DISMOUNT_VOLUME); // Windows reads the new file system on the next access
    Ok(())
}

/// Runs a hidden console tool: its output on failure.
fn tool(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(said)
    } else {
        Err(said.trim().to_owned())
    }
}

/// What the elevated process does: `(exit code, failure text)` on failure.
fn run(disk: u32, fs: Fs, letter: char, label: &str) -> Result<(), (i32, String)> {
    // Never trust the command line alone: the drive letter must still be a removable drive on that very disk.
    let drive = format!("{letter}:");
    // SAFETY: the wide string is NUL-terminated and lives through the call.
    let removable =
        unsafe { GetDriveTypeW(wide(&format!("{drive}\\")).as_ptr()) } == DRIVE_REMOVABLE;
    if !removable || disk_number(&drive) != Some(disk) {
        return Err((
            EXIT_ARGS,
            format!("{drive} is not a removable drive on disk {disk}"),
        ));
    }
    let script =
        std::env::temp_dir().join(format!("openfader-diskpart-{}.txt", std::process::id()));
    std::fs::write(&script, parse::diskpart_script(disk, fs, letter))
        .map_err(|e| (EXIT_DISKPART, e.to_string()))?;
    let said = tool("diskpart", &["/s", &script.to_string_lossy()]);
    let _ = std::fs::remove_file(&script);
    // diskpart says "DiskPart has encountered an error" and still exits 0 for some failures.
    match said {
        Ok(s) if s.contains("has encountered an error") => return Err((EXIT_DISKPART, s)),
        Err(s) => return Err((EXIT_DISKPART, s)),
        Ok(_) => {}
    }
    let drive = format!("{letter}:");
    // The new volume shows up a moment after diskpart is done.
    let ready = (0..40).any(|i| {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(250));
        }
        open_volume(&drive, GENERIC_READ | GENERIC_WRITE).is_some_and(|h| {
            // SAFETY: `h` was opened just now and is closed once.
            unsafe { CloseHandle(h) };
            true
        })
    });
    if !ready {
        return Err((EXIT_VOLUME, format!("{drive} did not appear")));
    }
    match fs {
        Fs::Fat32 => fat32(&drive, label).map_err(|e| (EXIT_FORMAT, e)),
        Fs::ExFat => tool(
            "format",
            &[&drive, "/FS:exFAT", "/Q", &format!("/V:{label}"), "/Y"],
        )
        .map(drop)
        .map_err(|e| (EXIT_FORMAT, e)),
    }
}

/// `openfader --format-disk <n> <fs> <letter> <result file> <label>` (the arguments after the flag): formats, exits with 0 or a
/// code, and leaves the failure text in the result file. Run elevated by [`format`], never by the user.
pub fn elevated_main(args: &[OsString]) -> i32 {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let Some((disk, fs, letter, result, label)) = parse::parse_elevated(&args) else {
        return EXIT_ARGS;
    };
    match run(disk, fs, letter, &label) {
        Ok(()) => 0,
        Err((code, detail)) => {
            let _ = std::fs::write(result, detail);
            code
        }
    }
}
