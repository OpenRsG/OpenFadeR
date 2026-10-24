//! Removable devices (USB sticks, SD cards) a library plugin can work with, and what is done with them (base): finding the mounted
//! ones with their file system, free space and the plugin that handles them ([`scan`]), opening the library on one read-only
//! ([`open_library`]).
//!
//! Detection is the OS's own, one child each (`os`): Linux (`linux`) reads `/proc/self/mountinfo` (a mount counts as a device when
//! it is under `/run/media` or `/media` or its disk is removable or on USB), Windows (`windows`) lists the removable drive
//! letters, macOS (`macos`) the local volumes below `/Volumes`. `OPENFADER_DEVICE_ROOTS=<dir>[:<dir>…]` adds folders as pretend
//! FAT32 sticks (testing). Nothing is spawned for the free space (`statvfs` / `GetDiskFreeSpaceExW`), which is cached for a few
//! seconds. What a device holds is not known here: the device plugins answer ([`Device::info`]).
//!
//! Support (players read FAT16/FAT32; exFAT only on some models and HFS+ only from USB, both flagged [`Support::Warn`]; NTFS and
//! anything else is [`Support::Unsupported`]: use FAT32, or HFS+ on a Mac). SD cards: FAT16/FAT32 only.
//!
//! Children: [`eject`] (unmount + power off), [`backup`] (copy the plugin's backup folders, restore them), [`export`] (several
//! library formats on one device), [`sync`] (Sync Manager), [`update`] (Update Collection: apply what the plugin read back),
//! [`link`] (cue sync between the collection and the device copy of a track), [`history`] (import the players' play
//! histories), [`plug`] (mount a stick nobody mounted, read its library files with progress).

pub mod backup;
pub mod check;
pub mod eject;
pub mod export;
pub mod history;
pub mod link;
pub mod plan;
pub mod plug;
pub mod sync;
pub mod transfer;
pub mod update;

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod os;

use crate::library::{self, Library};
use crate::plugin::{self, device::DeviceInfo};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// How the device is attached, from its disk name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Usb,
    Sd,
}

/// Whether players can use the file system ([`Device::support`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Ok,
    /// Works on some players only; the text says which.
    Warn(&'static str),
    /// Players cannot read it; exporting here is refused.
    Unsupported(&'static str),
}

impl Support {
    fn of(fs: &str, kind: Kind) -> Self {
        match (fs, kind) {
            ("vfat" | "msdos" | "fat" | "fat32", _) => Self::Ok,
            ("exfat", Kind::Sd) => {
                Self::Unsupported("exFAT on an SD card: players read FAT16 / FAT32 cards only")
            }
            ("exfat", _) => Self::Warn("exFAT: only some players read it; FAT32 works on all"),
            ("hfsplus" | "hfs", Kind::Usb) => {
                Self::Warn("HFS+: USB storage only, and not every player reads it")
            }
            ("hfsplus" | "hfs", Kind::Sd) => {
                Self::Unsupported("HFS+ on an SD card: players read FAT16 / FAT32 cards only")
            }
            ("ntfs" | "ntfs3" | "fuseblk", _) => Self::Unsupported(
                "NTFS is not supported by DJ players: use FAT32 (or HFS+ on a Mac)",
            ),
            _ => Self::Unsupported(
                "this file system is not supported by DJ players: use FAT32 (or HFS+ on a Mac)",
            ),
        }
    }

    /// The warning to show, `None` when the file system is fine.
    pub fn message(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::Warn(m) | Self::Unsupported(m) => Some(m),
        }
    }
}

/// One mounted removable device.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// Stable between mounts: the file system UUID, else the mount path.
    pub id: String,
    pub mount: PathBuf,
    /// Block device, e.g. `/dev/sdb1`.
    pub source: String,
    /// Volume label as udisks names the mount folder, else the folder name.
    pub label: String,
    /// Kernel file system name: `vfat`, `exfat`, `ntfs3`, …
    pub fs: String,
    /// Partition table of the disk it is on: `dos` (MBR), `gpt`, empty while unknown.
    pub table: String,
    pub kind: Kind,
    pub support: Support,
    pub read_only: bool,
    /// Bytes; 0 while unknown.
    pub total: u64,
    pub free: u64,
    /// Name of the library plugin that handles the device ([`plugin::device::find`]); `None`: no plugin can work with it.
    pub provider: Option<String>,
    /// What that plugin found on it; the default when there is no provider.
    pub info: DeviceInfo,
    /// Every library format a plugin can write here (or holds here), the provider first: `(plugin name, its answer)`
    /// ([`plugin::device::formats`]). An export writes those the user picked ([`Device::default_formats`] until then).
    pub formats: Vec<(String, DeviceInfo)>,
}

impl Device {
    /// A writable device at `mount` with no source or space known yet and no plugin asked ([`scan`] does that); the label is the
    /// folder name.
    fn new(id: String, mount: PathBuf, fs: String, kind: Kind, support: Support) -> Self {
        Self {
            id,
            label: mount.file_name().map_or_else(
                || mount.to_string_lossy().into_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            source: String::new(),
            fs,
            table: String::new(),
            kind,
            support,
            read_only: false,
            total: 0,
            free: 0,
            provider: None,
            info: DeviceInfo::default(),
            formats: Vec::new(),
            mount,
        }
    }

    /// The plugin that handles this device.
    pub fn provider(&self) -> Option<plugin::device::Provider> {
        plugin::device::provider(self.provider.as_deref()?)
    }

    /// Whether a library of the plugin is on the device.
    pub fn has_library(&self) -> bool {
        !self.info.libraries.is_empty()
    }

    /// Whether the plugin offers `f` for this device.
    pub fn can(&self, f: plugin::device::Feature) -> bool {
        self.info.features.contains(&f)
    }

    /// The largest file its file system holds: FAT (FAT16 / FAT32) stops at 4 GiB - 1.
    pub fn max_file(&self) -> Option<u64> {
        matches!(self.fs.as_str(), "vfat" | "msdos" | "fat" | "fat32")
            .then_some(u64::from(u32::MAX))
    }

    /// The formats an export writes when the user picked none for this device: those whose library is on it, else all (a blank
    /// device gets every format: it then works with any software).
    pub fn default_formats(&self) -> Vec<String> {
        let held: Vec<String> = self
            .formats
            .iter()
            .filter(|(_, i)| !i.libraries.is_empty())
            .map(|(n, _)| n.clone())
            .collect();
        if held.is_empty() {
            self.formats.iter().map(|(n, _)| n.clone()).collect()
        } else {
            held
        }
    }

    /// The name shown for the format of `plugin` here: what its plugin calls it, else the plugin name in capitals
    /// ("my-format" → "MY FORMAT").
    pub fn format_title(&self, plugin: &str) -> String {
        match self.formats.iter().find(|(n, _)| n == plugin) {
            Some((_, i)) if !i.title.is_empty() => i.title.clone(),
            _ => plugin.replace(['-', '_'], " ").to_uppercase(),
        }
    }

    /// The formats whose library is on the device, with the libraries when a format holds several:
    /// "My Format (Library A + Library B) · Other Format"; empty: none.
    pub fn held(&self) -> String {
        self.formats
            .iter()
            .filter(|(_, i)| !i.libraries.is_empty())
            .map(|(n, i)| match i.libraries.len() {
                1 => self.format_title(n),
                _ => format!("{} ({})", self.format_title(n), i.libraries.join(" + ")),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Whether an export may write here.
    pub fn writable(&self) -> bool {
        !self.read_only && !matches!(self.support, Support::Unsupported(_))
    }

    /// "FAT32 · 3.2 GB free of 14.9 GB".
    pub fn summary(&self) -> String {
        let fs = match self.fs.as_str() {
            "vfat" => "FAT32".to_owned(),
            "msdos" => "FAT".into(),
            "exfat" => "exFAT".into(),
            "hfsplus" => "HFS+".into(),
            "ntfs" | "ntfs3" | "fuseblk" => "NTFS".into(),
            other => other.to_uppercase(),
        };
        if self.total == 0 {
            fs
        } else {
            format!("{fs} · {} free of {}", size(self.free), size(self.total))
        }
    }
}

/// "3.2 GB" (decimal units, as file managers show them).
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    match bytes {
        0..1_000_000 => format!("{:.0} kB", b / 1e3),
        1_000_000..1_000_000_000 => format!("{:.0} MB", b / 1e6),
        _ => format!("{:.1} GB", b / 1e9),
    }
}

/// Space of one mount, looked up at most every [`SPACE_TTL`].
static SPACE: LazyLock<Mutex<HashMap<PathBuf, (Instant, u64, u64)>>> =
    LazyLock::new(Mutex::default);
const SPACE_TTL: Duration = Duration::from_secs(5);

/// `(total, free to a user)` bytes of the file system at `path` (`statvfs`: fragment size, blocks, blocks free to a user).
#[cfg(unix)]
fn fs_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is NUL-terminated and `s` is a live out-buffer; it is read only when the call says it was filled.
    let s = unsafe { (libc::statvfs(c.as_ptr(), s.as_mut_ptr()) == 0).then(|| s.assume_init())? };
    let unit = s.f_frsize as u64;
    Some((unit * s.f_blocks as u64, unit * s.f_bavail as u64))
}

/// `(total, free to the user)` bytes of the volume at `path` (`GetDiskFreeSpaceExW`).
#[cfg(windows)]
fn fs_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `w` is NUL-terminated and the three out-pointers are live locals.
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((total, avail))
}

/// `(total, free)` bytes of the file system at `path`; `None` if the path is gone. Cached; [`forget`] after writing.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    if let Some(&(_, total, free)) = SPACE.lock().get(path).filter(|e| e.0.elapsed() < SPACE_TTL) {
        return Some((total, free));
    }
    let (total, free) = fs_space(path)?;
    SPACE
        .lock()
        .insert(path.to_owned(), (Instant::now(), total, free));
    Some((total, free))
}

/// What the plugins said about one mount, asked at most every [`SPACE_TTL`] (they read the device).
static PROBED: LazyLock<Mutex<HashMap<PathBuf, (Instant, Vec<(String, DeviceInfo)>)>>> =
    LazyLock::new(Mutex::default);

fn probe(mount: &Path) -> Vec<(String, DeviceInfo)> {
    if let Some((_, f)) = PROBED
        .lock()
        .get(mount)
        .filter(|e| e.0.elapsed() < SPACE_TTL)
    {
        return f.clone();
    }
    let f: Vec<_> = plugin::device::formats(mount)
        .into_iter()
        .map(|(p, i)| (p.name().to_owned(), i))
        .collect();
    PROBED
        .lock()
        .insert(mount.to_owned(), (Instant::now(), f.clone()));
    f
}

/// Drops the cached space and plugin answer of `path` so the next [`scan`] asks again (after a job wrote to it).
pub fn forget(path: &Path) {
    SPACE.lock().remove(path);
    PROBED.lock().remove(path);
}

/// The devices mounted right now, by mount path, with file system, space, the plugin that handles them and what it found.
// ponytail: the app polls this every second and compares ids; udev / udisks D-Bus signals would avoid the poll, but need a bus crate.
pub fn scan() -> Vec<Device> {
    let mut v = os::mounted();
    v.extend(pretend_devices(OsStr::new(
        &std::env::var("OPENFADER_DEVICE_ROOTS").unwrap_or_default(),
    )));
    for d in &mut v {
        if let Some((total, free)) = space(&d.mount) {
            (d.total, d.free) = (total, free);
        }
        d.formats = probe(&d.mount);
        (d.provider, d.info) = d
            .formats
            .first()
            .map_or((None, DeviceInfo::default()), |(p, i)| {
                (Some(p.clone()), i.clone())
            });
    }
    v
}

/// `OPENFADER_DEVICE_ROOTS=<dir>[:<dir>…]` (`;` on Windows): every existing folder listed counts as a mounted FAT32 USB stick
/// (for tests and demos on a machine without removable media). Never set in normal use; ejecting one runs the real eject
/// commands on the folder.
fn pretend_devices(roots: &OsStr) -> Vec<Device> {
    std::env::split_paths(roots)
        .filter(|r| r.is_dir())
        .map(|r| {
            Device::new(
                r.to_string_lossy().into_owned(),
                r,
                "vfat".into(),
                Kind::Usb,
                Support::Ok,
            )
        })
        .collect()
}

/// The library on the device, read-only, opened by the plugin that found it.
pub fn open_library(d: &Device) -> Result<Box<dyn Library + Send>, String> {
    let plugin = d.provider.as_deref().filter(|_| d.has_library());
    let plugin = plugin.ok_or_else(|| format!("{} holds no library", d.label))?;
    Ok(Box::new(plugin::library::open_with(plugin, &d.mount)?))
}

/// The names from the top level down to playlist `id` of `playlists` (a [`Library::playlists`] list in tree order), `None` for an
/// unknown id: how a playlist is found again in a library written by another program, whose ids differ.
pub fn playlist_path(playlists: &[library::Playlist], id: &str) -> Option<Vec<String>> {
    let at = playlists.iter().position(|p| p.id == id)?;
    let mut path = vec![playlists[at].name.clone()];
    let mut depth = playlists[at].depth;
    for p in playlists[..at].iter().rev() {
        if p.depth < depth {
            path.insert(0, p.name.clone());
            depth = p.depth;
        }
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_systems_are_judged_by_what_the_players_read() {
        assert_eq!(
            Support::of("exfat", Kind::Sd),
            Support::Unsupported("exFAT on an SD card: players read FAT16 / FAT32 cards only")
        );
        assert_eq!(
            Support::of("exfat", Kind::Usb),
            Support::Warn("exFAT: only some players read it; FAT32 works on all")
        );
        for fat in ["vfat", "msdos", "fat32"] {
            assert!(Support::of(fat, Kind::Sd).message().is_none(), "{fat}");
        }
        assert!(matches!(
            Support::of("ntfs", Kind::Usb),
            Support::Unsupported(_)
        ));
    }

    #[test]
    fn device_roots_make_folders_into_fat32_sticks() {
        let dir = std::env::temp_dir();
        let spec = std::env::join_paths([dir.clone(), "/no/such/folder".into()]).unwrap();
        let v = pretend_devices(&spec);
        assert_eq!(v.len(), 1);
        assert!(v[0].writable() && v[0].fs == "vfat" && v[0].mount == dir);
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(
            (size(999_000), size(32_000_000), size(15_300_000_000)),
            ("999 kB".into(), "32 MB".into(), "15.3 GB".into())
        );
    }

    #[test]
    fn the_space_of_a_folder_is_its_file_systems() {
        let (total, free) =
            space(&std::env::temp_dir()).expect("the temp folder has a file system");
        assert!(total > 0 && free <= total, "{free} of {total}");
    }
}
