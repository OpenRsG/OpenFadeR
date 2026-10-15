//! Removable devices (USB sticks, SD cards) a library plugin can work with, and what is done with them (base): finding the mounted
//! ones with their file system, free space and the plugin that handles them ([`scan`]), opening the library on one read-only
//! ([`open_library`]).
//!
//! Detection: `/proc/self/mountinfo` lists every mount; a mount counts as a device when it is under `/run/media` or `/media`
//! (where udisks and the desktops mount removable media) or when its disk is removable (`/sys/block/<disk>/removable`) or on
//! USB (its sysfs path runs through a `usb` bus: USB SSDs report not removable), so a stick mounted by hand shows up too. `OPENFADER_DEVICE_ROOTS=<dir>[:<dir>…]` adds
//! folders as pretend FAT32 sticks (testing). Nothing is spawned except `stat -f` for the free space,
//! which is cached for a few seconds. What a device holds is not known here: the device plugins answer ([`Device::info`]).
//!
//! Support (players read FAT16/FAT32; exFAT only on some models and HFS+ only from USB, both flagged [`Support::Warn`]; NTFS and
//! anything else is [`Support::Unsupported`]: use FAT32, or HFS+ on a Mac). SD cards: FAT16/FAT32 only.
//!
//! Children: [`eject`] (unmount + power off), [`backup`] (copy the plugin's backup folders, restore them), [`sync`] (Sync Manager),
//! [`update`] (Update Collection: apply what the plugin read back), [`history`] (import the players' play histories), [`plug`]
//! (mount a stick nobody mounted, read its library files with progress).

pub mod backup;
pub mod eject;
pub mod history;
pub mod plug;
pub mod sync;
pub mod update;

use crate::library::{self, Library};
use crate::plugin::{self, device::DeviceInfo};
use parking_lot::Mutex;
use std::collections::HashMap;
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
            kind,
            support,
            read_only: false,
            total: 0,
            free: 0,
            provider: None,
            info: DeviceInfo::default(),
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

    /// Whether an export may write here.
    pub fn writable(&self) -> bool {
        !self.read_only && !matches!(self.support, Support::Unsupported(_))
    }

    /// "FAT32 · 3.2 GB free of 14.9 GB".
    pub fn summary(&self) -> String {
        let fs = match self.fs.as_str() {
            "vfat" => "FAT32".to_owned(),
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

/// One line of `/proc/self/mountinfo` that matters here.
struct Mount {
    point: String,
    fs: String,
    source: String,
    ro: bool,
}

/// `\040` and friends: the kernel escapes space, tab, newline and backslash in mount paths.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (
            b[i],
            s.get(i + 1..i + 4)
                .and_then(|o| u8::from_str_radix(o, 8).ok()),
        ) {
            (b'\\', Some(v)) => {
                out.push(v);
                i += 4;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Spec: proc(5) `mountinfo`: `id parent maj:min root point options [optional…] - fstype source super-options`.
fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let f: Vec<&str> = left.split(' ').collect();
            let r: Vec<&str> = right.split(' ').collect();
            Some(Mount {
                point: unescape(f.get(4)?),
                ro: f.get(5)?.split(',').any(|o| o == "ro"),
                fs: (*r.first()?).to_owned(),
                source: unescape(r.get(1)?),
            })
        })
        .collect()
}

/// Whether the mount is at a place removable media gets mounted.
fn media_path(p: &str) -> bool {
    p.starts_with("/run/media/") || p.starts_with("/media/")
}

/// What sysfs and `/dev/disk/by-uuid` know about a block device.
trait Sys {
    /// The disk (`sdb`, `mmcblk0`) behind a partition (`sdb1`, `mmcblk0p1`) with whether the kernel calls it removable.
    fn disk(&self, part: &str) -> Option<(String, bool)>;
    fn uuid(&self, part: &str) -> Option<String>;
}

struct Linux;

impl Sys for Linux {
    fn disk(&self, part: &str) -> Option<(String, bool)> {
        let real = std::fs::canonicalize(format!("/sys/class/block/{part}")).ok()?;
        // A partition lives below its disk's folder; a whole disk is its own folder (it has no `partition` file).
        let disk = if real.join("partition").exists() {
            real.parent()?
        } else {
            real.as_path()
        };
        let removable = std::fs::read_to_string(disk.join("removable"))
            .is_ok_and(|s| s.trim() == "1")
            || disk
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with("usb"));
        Some((disk.file_name()?.to_string_lossy().into_owned(), removable))
    }

    fn uuid(&self, part: &str) -> Option<String> {
        std::fs::read_dir("/dev/disk/by-uuid")
            .ok()?
            .flatten()
            .find(|e| {
                std::fs::canonicalize(e.path())
                    .is_ok_and(|t| t.file_name().is_some_and(|n| n == part))
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
    }
}

fn devices_in(mountinfo: &str, sys: &dyn Sys) -> Vec<Device> {
    let mut out: Vec<Device> = Vec::new();
    for m in parse_mountinfo(mountinfo) {
        if out.iter().any(|d| d.mount == Path::new(&m.point)) {
            continue;
        }
        let part = m.source.strip_prefix("/dev/").unwrap_or("");
        let disk = (!part.is_empty()).then(|| sys.disk(part)).flatten();
        if !(media_path(&m.point) || disk.as_ref().is_some_and(|d| d.1)) {
            continue;
        }
        let kind = if disk.as_ref().is_some_and(|d| d.0.starts_with("mmcblk")) {
            Kind::Sd
        } else {
            Kind::Usb
        };
        let id = sys.uuid(part).unwrap_or_else(|| m.point.clone());
        let support = Support::of(&m.fs, kind);
        out.push(Device {
            source: m.source,
            read_only: m.ro,
            ..Device::new(id, m.point.into(), m.fs, kind, support)
        });
    }
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

/// Space of one mount, looked up at most every [`SPACE_TTL`] (it takes a process).
static SPACE: LazyLock<Mutex<HashMap<PathBuf, (Instant, u64, u64)>>> =
    LazyLock::new(Mutex::default);
const SPACE_TTL: Duration = Duration::from_secs(5);

/// `(total, free)` bytes of the file system at `path` (`stat -f`: block size, blocks, blocks free to a user); `None` if `stat` is
/// missing or the path is gone. Cached; [`forget`] after writing.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    if let Some(&(_, total, free)) = SPACE.lock().get(path).filter(|e| e.0.elapsed() < SPACE_TTL) {
        return Some((total, free));
    }
    let out = std::process::Command::new("stat")
        .args(["-f", "-c", "%S %b %a"])
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let n: Vec<u64> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let (total, free) = (n.first()? * n.get(1)?, n.first()? * n.get(2)?);
    SPACE
        .lock()
        .insert(path.to_owned(), (Instant::now(), total, free));
    Some((total, free))
}

/// What the plugins said about one mount, asked at most every [`SPACE_TTL`] (they read the device).
static PROBED: LazyLock<Mutex<HashMap<PathBuf, (Instant, Option<String>, DeviceInfo)>>> =
    LazyLock::new(Mutex::default);

fn probe(mount: &Path) -> (Option<String>, DeviceInfo) {
    if let Some((_, p, i)) = PROBED
        .lock()
        .get(mount)
        .filter(|e| e.0.elapsed() < SPACE_TTL)
    {
        return (p.clone(), i.clone());
    }
    let (p, i) = plugin::device::find(mount).map_or((None, DeviceInfo::default()), |(p, i)| {
        (Some(p.name().to_owned()), i)
    });
    PROBED
        .lock()
        .insert(mount.to_owned(), (Instant::now(), p.clone(), i.clone()));
    (p, i)
}

/// Drops the cached space and plugin answer of `path` so the next [`scan`] asks again (after a job wrote to it).
pub fn forget(path: &Path) {
    SPACE.lock().remove(path);
    PROBED.lock().remove(path);
}

/// The devices mounted right now, by mount path, with file system, space, the plugin that handles them and what it found.
// ponytail: the app polls this every second and compares ids; udev / udisks D-Bus signals would avoid the poll, but need a bus crate.
pub fn scan() -> Vec<Device> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut v = devices_in(&text, &Linux);
    v.extend(pretend_devices(
        &std::env::var("OPENFADER_DEVICE_ROOTS").unwrap_or_default(),
    ));
    for d in &mut v {
        if let Some((total, free)) = space(&d.mount) {
            (d.total, d.free) = (total, free);
        }
        (d.provider, d.info) = probe(&d.mount);
    }
    v
}

/// `OPENFADER_DEVICE_ROOTS=<dir>[:<dir>…]`: every existing folder listed counts as a mounted FAT32 USB stick (for tests and
/// demos on a machine without removable media). Never set in normal use; ejecting one runs the real eject commands on the folder.
fn pretend_devices(roots: &str) -> Vec<Device> {
    roots
        .split(':')
        .filter(|r| Path::new(r).is_dir())
        .map(|r| {
            Device::new(
                r.to_owned(),
                r.into(),
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

    struct Fake;

    impl Sys for Fake {
        fn disk(&self, part: &str) -> Option<(String, bool)> {
            match part {
                "sdb1" => Some(("sdb".into(), true)),
                "mmcblk0p1" => Some(("mmcblk0".into(), true)),
                "sda2" => Some(("sda".into(), false)),
                _ => None,
            }
        }

        fn uuid(&self, part: &str) -> Option<String> {
            (part == "sdb1").then(|| "ABCD-1234".into())
        }
    }

    const MOUNTINFO: &str = "\
24 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
25 24 0:5 / /proc rw - proc proc rw
80 24 8:17 / /run/media/dj/MY\\040KEY rw,nosuid,relatime - vfat /dev/sdb1 rw,fmask=0022
81 24 179:1 / /mnt/card ro,relatime - exfat /dev/mmcblk0p1 ro
82 24 8:3 / /media/dj/data rw - ntfs3 /dev/sdc1 rw
83 24 7:0 / /run/media/dj/loop rw - ext4 /dev/loop0 rw
";

    #[test]
    fn finds_removable_mounts_with_their_file_systems() {
        let v = devices_in(MOUNTINFO, &Fake);
        let shape: Vec<_> = v
            .iter()
            .map(|d| {
                (
                    d.mount.to_str().unwrap(),
                    d.label.as_str(),
                    d.fs.as_str(),
                    d.kind,
                    d.read_only,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("/media/dj/data", "data", "ntfs3", Kind::Usb, false),
                ("/mnt/card", "card", "exfat", Kind::Sd, true), // not under /media, but the kernel says removable
                ("/run/media/dj/MY KEY", "MY KEY", "vfat", Kind::Usb, false),
                ("/run/media/dj/loop", "loop", "ext4", Kind::Usb, false),
            ]
        );
        assert_eq!(v[2].id, "ABCD-1234");
        assert_eq!(v[0].id, "/media/dj/data", "no uuid: the mount path");
        assert!(v[2].writable() && !v[0].writable() && !v[1].writable());
        assert_eq!(
            v[1].support,
            Support::Unsupported("exFAT on an SD card: players read FAT16 / FAT32 cards only")
        );
        assert_eq!(
            Support::of("exfat", Kind::Usb),
            Support::Warn("exFAT: only some players read it; FAT32 works on all")
        );
        assert!(Support::of("vfat", Kind::Sd).message().is_none());
    }

    #[test]
    fn device_roots_make_folders_into_fat32_sticks() {
        let dir = std::env::temp_dir();
        let spec = format!("{}:/no/such/folder", dir.display());
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
}
