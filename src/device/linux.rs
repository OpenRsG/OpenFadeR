//! Finding the mounted removable devices on Linux: `/proc/self/mountinfo` lists every mount; a mount counts as a device when it is
//! under `/run/media` or `/media` (where udisks and the desktops mount removable media) or when its disk is removable
//! (`/sys/block/<disk>/removable`) or on USB (its sysfs path runs through a `usb` bus: USB SSDs report not removable), so a
//! stick mounted by hand shows up too. The same path says whether the disk sits behind a USB hub ([`behind_hub`]).

use super::{format::parse, Device, Kind, Support};
use std::path::Path;

/// The udev database text of block device `name` (`sdb`, `sdb1`; `/run/udev/data/b<major>:<minor>`); empty when there is none.
pub(super) fn udev(name: &str) -> String {
    std::fs::read_to_string(format!("/sys/class/block/{name}/dev"))
        .and_then(|d| std::fs::read_to_string(format!("/run/udev/data/b{}", d.trim())))
        .unwrap_or_default()
}

/// One line of `/proc/self/mountinfo` that matters here.
pub(super) struct Mount {
    pub(super) point: String,
    pub(super) fs: String,
    pub(super) source: String,
    pub(super) ro: bool,
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
pub(super) fn parse_mountinfo(text: &str) -> Vec<Mount> {
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

/// Whether a sysfs device path (`…/usb6/6-2/6-2.4/6-2.4.2/6-2.4.2:1.0/host6/…/block/sdb`) runs through a USB hub: below the root
/// hub (`usb6`) every device is named `<bus>-<port>[.<port>…]`, one more port level for each hub in between, so more than one
/// level means the disk's own device hangs off a hub (`6-2`: a port of the computer; `6-2.4`: port 4 of a hub in it). Interface
/// names (`6-2.4.2:1.0`) end the chain.
fn behind_hub(path: &Path) -> bool {
    fn device(c: &str) -> bool {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        c.split_once('-')
            .is_some_and(|(bus, ports)| digits(bus) && ports.split('.').all(digits))
    }
    let mut names = path.components().map(|c| c.as_os_str().to_string_lossy());
    let root = |c: &str| c.strip_prefix("usb").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    names.any(|c| root(&c)) && names.take_while(|c| device(c)).count() > 1
}

/// What sysfs and `/dev/disk/by-uuid` know about a block device.
pub(super) trait Sys {
    /// The disk (`sdb`, `mmcblk0`) behind a partition (`sdb1`, `mmcblk0p1`) with whether the kernel calls it removable.
    fn disk(&self, part: &str) -> Option<(String, bool)>;
    fn uuid(&self, part: &str) -> Option<String>;
    /// The partition table type of whole disk `disk` (`dos`, `gpt`); empty while unknown.
    fn table(&self, disk: &str) -> String {
        let _ = disk;
        String::new()
    }
    /// Whether whole disk `disk` is plugged into a USB hub rather than a port of the computer.
    fn via_hub(&self, disk: &str) -> bool {
        let _ = disk;
        false
    }
    /// The file system type udev found on partition `part` (`exfat`, `ntfs`), for a mount that names only a FUSE driver.
    fn fs_type(&self, part: &str) -> Option<String> {
        let _ = part;
        None
    }
}

pub(super) struct Linux;

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

    fn table(&self, disk: &str) -> String {
        parse::table_from_udev(&udev(disk))
    }

    fn via_hub(&self, disk: &str) -> bool {
        std::fs::canonicalize(format!("/sys/class/block/{disk}")).is_ok_and(|p| behind_hub(&p))
    }

    fn fs_type(&self, part: &str) -> Option<String> {
        parse::udev_prop(&udev(part), "ID_FS_TYPE").map(str::to_owned)
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
        // A FUSE driver (exfat-fuse, ntfs-3g) reports `fuseblk`: what the disk holds says which file system it is.
        let fs = if m.fs == "fuseblk" {
            sys.fs_type(part).unwrap_or(m.fs)
        } else {
            m.fs
        };
        let via_hub = disk.as_ref().is_some_and(|d| sys.via_hub(&d.0));
        let support = match Support::of(&fs, kind) {
            // The one-line note of the device list: the file system warning, which is permanent, goes first.
            Support::Ok if via_hub => Support::Warn(
                "plugged in through a USB hub: long exports can fail when the hub resets; use a port of the computer",
            ),
            s => s,
        };
        out.push(Device {
            source: m.source,
            read_only: m.ro,
            via_hub,
            table: disk.map_or_else(String::new, |d| sys.table(&d.0)),
            ..Device::new(id, m.point.into(), fs, kind, support)
        });
    }
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

/// The removable devices mounted now.
pub(super) fn mounted() -> Vec<Device> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    devices_in(&text, &Linux)
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

        fn table(&self, disk: &str) -> String {
            (disk == "sdb").then(|| "dos".into()).unwrap_or_default()
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
            v.iter().map(|d| d.table.as_str()).collect::<Vec<_>>(),
            ["", "", "dos", ""],
            "only the disk the fake knows has a table; an unknown one stays empty"
        );
        assert_eq!(
            v[1].support,
            Support::Unsupported("exFAT on an SD card: players read FAT16 / FAT32 cards only")
        );
    }

    #[test]
    fn a_disk_behind_a_usb_hub_is_told_from_its_sysfs_path() {
        let at = |dev: &str| format!("/sys/devices/pci0000:00/0000:00:14.0/{dev}/host6/target6:0:0/6:0:0:0/block/sdb");
        for (path, hub) in [
            ("usb6/6-2/6-2:1.0", false),
            ("usb6/6-2/6-2.4/6-2.4:1.0", true),
            ("usb6/6-2/6-2.4/6-2.4.2/6-2.4.2:1.0", true),
            ("usb1/1-1/1-1.1/1-1.1:1.0", true),
        ] {
            assert_eq!(behind_hub(Path::new(&at(path))), hub, "{path}");
        }
        // A disk that is not on USB, and a name that only starts like a bus.
        assert!(!behind_hub(Path::new("/sys/devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda")));
        assert!(!behind_hub(Path::new("/sys/devices/platform/usbhid/1-2/1-2.3/block/sdb")));
    }

    #[test]
    fn a_device_behind_a_hub_says_so_and_keeps_a_file_system_warning() {
        struct Hub;
        impl Sys for Hub {
            fn disk(&self, _: &str) -> Option<(String, bool)> {
                Some(("sdb".into(), true))
            }
            fn uuid(&self, _: &str) -> Option<String> {
                None
            }
            fn via_hub(&self, _: &str) -> bool {
                true
            }
        }
        let fat = devices_in("80 24 8:17 / /run/media/dj/A rw - vfat /dev/sdb1 rw\n", &Hub);
        assert!(fat[0].via_hub && matches!(fat[0].support, Support::Warn(m) if m.contains("hub")));
        let exfat = devices_in("80 24 8:17 / /run/media/dj/A rw - exfat /dev/sdb1 rw\n", &Hub);
        assert!(exfat[0].via_hub && matches!(exfat[0].support, Support::Warn(m) if m.contains("exFAT")));
        assert!(!devices_in(MOUNTINFO, &Fake).iter().any(|d| d.via_hub));
    }

    #[test]
    fn a_fuse_mount_is_the_file_system_the_disk_holds() {
        struct Fuse(Option<&'static str>);
        impl Sys for Fuse {
            fn disk(&self, _: &str) -> Option<(String, bool)> {
                Some(("sdb".into(), true))
            }
            fn uuid(&self, _: &str) -> Option<String> {
                None
            }
            fn fs_type(&self, _: &str) -> Option<String> {
                self.0.map(str::to_owned)
            }
        }
        let mount = "80 24 8:17 / /run/media/dj/EX rw - fuseblk /dev/sdb1 rw\n";
        let v = devices_in(mount, &Fuse(Some("exfat")));
        assert_eq!(v[0].fs, "exfat", "exfat-fuse is exFAT, not NTFS");
        assert!(matches!(v[0].support, Support::Warn(_)));
        assert_eq!(
            devices_in(mount, &Fuse(None))[0].fs,
            "fuseblk",
            "without udev's answer it stays as the kernel says"
        );
    }
}
