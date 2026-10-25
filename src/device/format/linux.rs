//! Formatting on Linux through udisks2 over the system D-Bus: no root (polkit lets the logged-in user format removable media, and
//! asks for a password through the desktop's agent when it must), the same calls a disk utility makes: unmount every volume,
//! `Block.Format("dos")` on the whole disk (a new empty MBR), `PartitionTable.CreatePartitionAndFormat` (one primary partition of
//! type `0x0c` / `0x07`, `vfat` with `mkfs.vfat -F 32` so FAT32 holds on any size, or `exfat`, and the label), then
//! `Filesystem.Mount`. The disk and its volumes are described from sysfs, the udev database and `/proc/self/mountinfo`.

use super::{parse, Disk, Done, Error, Fs, Step, Volume};
use crate::device::{
    os::{parse_mountinfo, udev, Linux, Sys},
    Device,
};
use std::{collections::HashMap, path::Path, time::Duration};
use zbus::{
    blocking::Connection,
    zvariant::{OwnedObjectPath, Value},
};

const SYS: &str = "/sys/class/block";

/// `sdb1` of `/dev/sdb1`; `None` for a pretend folder.
fn node(source: &str) -> Option<&str> {
    source.strip_prefix("/dev/").filter(|n| !n.is_empty())
}

fn sectors(path: &Path) -> u64 {
    std::fs::read_to_string(path.join("size"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
        * 512
}

/// The disk `dev` is on with its volumes.
pub(super) fn describe(dev: &Device) -> Result<Disk, Error> {
    let part = node(&dev.source).ok_or(Error::NoDisk)?;
    let (name, kernel_removable) = Linux.disk(part).ok_or(Error::NoDisk)?;
    let base = Path::new(SYS).join(&name);
    let text = |rel: &str| std::fs::read_to_string(base.join(rel)).unwrap_or_default().trim().to_owned();
    let disk_udev = udev(&name);
    // A loop image is a file of the user's; an SD card in a laptop's slot is not flagged removable by the kernel.
    let removable = kernel_removable || name.starts_with("loop") || text("device/type") == "SD";
    let model = if name.starts_with("loop") {
        Path::new(&text("loop/backing_file")).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        let m = format!("{} {}", parse::udev_text(&disk_udev, "ID_VENDOR"), parse::udev_text(&disk_udev, "ID_MODEL"));
        match m.trim() {
            "" => text("device/model"),
            m => m.to_owned(),
        }
    };
    let mounts = parse_mountinfo(&std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default());
    let volume = |n: &str| {
        let u = udev(n);
        Volume {
            name: n.to_owned(),
            label: parse::udev_text(&u, "ID_FS_LABEL"),
            // The FAT variant when blkid knows it (`fat16`): a card can come out FAT16 below about 512 MB.
            fs: match (parse::udev_prop(&u, "ID_FS_TYPE"), parse::fat_version(&u)) {
                (Some("vfat"), Some(v)) => v.to_lowercase(),
                (t, _) => t.unwrap_or_default().to_owned(),
            },
            size: sectors(&Path::new(SYS).join(n)),
            mount: mounts.iter().find(|m| m.source == format!("/dev/{n}")).map(|m| m.point.clone().into()),
        }
    };
    let mut parts: Vec<(u32, String)> = std::fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let number = std::fs::read_to_string(e.path().join("partition")).ok()?;
            n.starts_with(&name).then(|| (number.trim().parse().unwrap_or(0), n))
        })
        .collect();
    parts.sort();
    let mut volumes: Vec<Volume> = parts.iter().map(|(_, n)| volume(n)).collect();
    if volumes.is_empty() {
        // A file system straight on the disk, no table (a "super floppy").
        let v = volume(&name);
        if !v.fs.is_empty() || v.mount.is_some() {
            volumes.push(v);
        }
    }
    let swaps = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
    let swapping = volumes.iter().any(|v| swaps.lines().any(|l| l.split_whitespace().next() == Some(&format!("/dev/{}", v.name))));
    let mount_points: Vec<&Path> = volumes.iter().filter_map(|v| v.mount.as_deref()).collect();
    Ok(Disk {
        id: format!("/dev/{name}"),
        model,
        size: sectors(&base),
        removable,
        system: swapping || super::holds_system(&mount_points, &super::protected()),
        volumes,
    })
}

/// Why a udisks call failed.
#[derive(Debug, PartialEq, Eq)]
enum Fail {
    /// polkit said no, or the password prompt was dismissed.
    Denied,
    /// The device is in use.
    Busy,
    /// The call hit an object udisks has not finished making yet.
    NotYet,
    /// Already mounted (a desktop automounter was quicker).
    Mounted,
    /// udisks is not there.
    Missing,
    Other(String),
}

/// The udisks calls a format makes; the real one talks D-Bus, the tests' double records.
trait Udisks {
    /// Unmounts block device `node` (`sdb1`).
    fn unmount(&mut self, node: &str) -> Result<(), Fail>;
    /// Makes a new empty MBR on whole disk `disk` (`sdb`).
    fn new_table(&mut self, disk: &str) -> Result<(), Fail>;
    /// One primary partition of type `0x0c` / `0x07` over the whole free space of `disk`; its node (`sdb1`).
    fn new_partition(&mut self, disk: &str, fs: Fs) -> Result<String, Fail>;
    /// Makes the file system on `node`; `fat32`: `mkfs.vfat -F 32`, which udisks lets a user do only with an extra authorisation.
    fn format(&mut self, node: &str, fs: Fs, label: &str, fat32: bool) -> Result<(), Fail>;
    /// Mounts block device `node`.
    fn mount(&mut self, node: &str) -> Result<(), Fail>;
}

const DEST: &str = "org.freedesktop.UDisks2";

struct Bus(Connection);

type Opts<'a> = HashMap<&'a str, Value<'a>>;

impl Bus {
    fn call<B>(&self, node: &str, iface: &str, method: &str, body: &B) -> Result<zbus::Message, Fail>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let path = format!("/org/freedesktop/UDisks2/block_devices/{node}");
        self.0
            .call_method(Some(DEST), path, Some(format!("{DEST}.{iface}")), method, body)
            .map_err(|e| {
                let zbus::Error::MethodError(name, detail, _) = &e else {
                    return Fail::Other(e.to_string());
                };
                let detail = detail.clone().unwrap_or_default();
                match name.rsplit('.').next().unwrap_or_default() {
                    "NotAuthorized" | "NotAuthorizedCanObtain" | "NotAuthorizedDismissed" => Fail::Denied,
                    "DeviceBusy" => Fail::Busy,
                    "AlreadyMounted" => Fail::Mounted,
                    "UnknownMethod" | "UnknownInterface" | "UnknownObject" => Fail::NotYet,
                    "ServiceUnknown" | "NameHasNoOwner" => Fail::Missing,
                    _ if detail.contains("busy") => Fail::Busy,
                    n => Fail::Other(format!("{n}: {detail}")),
                }
            })
    }
}

impl Udisks for Bus {
    fn unmount(&mut self, node: &str) -> Result<(), Fail> {
        self.call(node, "Filesystem", "Unmount", &(Opts::new(),)).map(drop)
    }

    fn new_table(&mut self, disk: &str) -> Result<(), Fail> {
        self.call(disk, "Block", "Format", &("dos", Opts::new())).map(drop)
    }

    fn new_partition(&mut self, disk: &str, fs: Fs) -> Result<String, Fail> {
        let ptype = match fs {
            Fs::Fat32 => "0x0c",
            Fs::ExFat => "0x07",
        };
        // offset 0, size 0: from the first aligned sector to the end of the disk.
        let reply = self.call(disk, "PartitionTable", "CreatePartition", &(0u64, 0u64, ptype, "", Opts::new()))?;
        let path: OwnedObjectPath = reply.body().deserialize().map_err(|e| Fail::Other(e.to_string()))?;
        Ok(path.rsplit('/').next().unwrap_or_default().to_owned())
    }

    fn format(&mut self, node: &str, fs: Fs, label: &str, fat32: bool) -> Result<(), Fail> {
        let mut options = Opts::from([("label", Value::new(label))]);
        if fat32 {
            options.insert("mkfs-args", Value::new(vec!["-F", "32"]));
        }
        let kind = match fs {
            Fs::Fat32 => "vfat",
            Fs::ExFat => "exfat",
        };
        self.call(node, "Block", "Format", &(kind, options)).map(drop)
    }

    fn mount(&mut self, node: &str) -> Result<(), Fail> {
        match self.call(node, "Filesystem", "Mount", &(Opts::new(),)) {
            Ok(_) | Err(Fail::Mounted) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

fn failed(step: Step, erased: bool, f: Fail, node: &str) -> Error {
    match f {
        Fail::Denied => Error::Denied { erased: erased && step != Step::Table },
        Fail::Busy if step == Step::Unmount => Error::InUse(node.to_owned()),
        Fail::Missing => Error::Missing("udisks2"),
        Fail::Busy | Fail::NotYet | Fail::Mounted => Error::Failed { step, detail: "the device is busy".into(), erased },
        Fail::Other(detail) => Error::Failed { step, detail, erased },
    }
}

/// The format as a list of udisks calls. `fat_version(node)` is what the system reports for the new volume once it exists.
///
/// FAT32 is asked for after the plain format when `mkfs.vfat` made something else (it picks FAT16 below about 512 MB): that call
/// may need an extra authorisation (a password with a desktop agent, a refusal without one). Without it the volume stays FAT16,
/// which every player reads, and [`Done::short`] says so.
fn sequence(
    bus: &mut dyn Udisks,
    disk: &Disk,
    fs: Fs,
    label: &str,
    step: &dyn Fn(Step),
    fat_version: &dyn Fn(&str) -> Option<String>,
) -> Result<Done, Error> {
    let name = disk.id.strip_prefix("/dev/").unwrap_or(&disk.id);
    step(Step::Unmount);
    for v in disk.mounted() {
        bus.unmount(&v.name).map_err(|f| failed(Step::Unmount, false, f, &v.name))?;
    }
    step(Step::Table);
    bus.new_table(name).map_err(|f| failed(Step::Table, true, f, name))?;
    step(Step::Partition);
    // The partition table's interface shows up a moment after `Format` returns.
    let mut tries = 0;
    let part = loop {
        match bus.new_partition(name, fs) {
            Err(Fail::NotYet) if tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            r => break r.map_err(|f| failed(Step::Partition, true, f, name))?,
        }
    };
    bus.format(&part, fs, label, false).map_err(|f| failed(Step::Partition, true, f, &part))?;
    let mut short = None;
    if fs == Fs::Fat32
        && let Some(v) = fat_version(&part).filter(|v| v != "FAT32")
    {
        match bus.format(&part, fs, label, true) {
            Ok(()) => {}
            // The first format stands: FAT16, which every player reads.
            Err(Fail::Denied) => short = Some(v),
            Err(f) => return Err(failed(Step::Partition, true, f, &part)),
        }
    }
    step(Step::Mount);
    bus.mount(&part).map_err(|f| failed(Step::Mount, true, f, &part))?;
    Ok(Done { source: format!("/dev/{part}"), fs, label: label.to_owned(), short })
}

pub(super) fn format(_dev: &Device, disk: &Disk, fs: Fs, label: &str, step: &dyn Fn(Step)) -> Result<Done, Error> {
    let mut bus = Bus(Connection::system().map_err(|_| Error::Missing("udisks2"))?);
    // udev takes a moment to probe the new file system: wait for the version blkid reports.
    let version = |part: &str| {
        (0..8).find_map(|i| {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(250));
            }
            parse::fat_version(&udev(part)).map(str::to_owned)
        })
    };
    sequence(&mut bus, disk, fs, label, step, &version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the calls and fails the ones asked to.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<String>,
        fail: Option<(&'static str, Fail)>,
        not_yet: u32,
    }

    impl Recorder {
        fn go(&mut self, what: &str, call: String) -> Result<(), Fail> {
            self.calls.push(call);
            match self.fail.take() {
                Some((w, f)) if w == what => Err(f),
                other => {
                    self.fail = other;
                    Ok(())
                }
            }
        }
    }

    impl Udisks for Recorder {
        fn unmount(&mut self, node: &str) -> Result<(), Fail> {
            self.go("unmount", format!("unmount {node}"))
        }
        fn new_table(&mut self, disk: &str) -> Result<(), Fail> {
            self.go("table", format!("Format(dos) {disk}"))
        }
        fn new_partition(&mut self, disk: &str, fs: Fs) -> Result<String, Fail> {
            if self.not_yet > 0 {
                self.not_yet -= 1;
                self.calls.push("not yet".into());
                return Err(Fail::NotYet);
            }
            self.go("partition", format!("CreatePartition {disk} {fs:?}"))?;
            Ok(format!("{disk}1"))
        }
        fn format(&mut self, node: &str, fs: Fs, label: &str, fat32: bool) -> Result<(), Fail> {
            let args = if fat32 { " -F 32" } else { "" };
            self.go(if fat32 { "format-args" } else { "format" }, format!("Format {node} {fs:?} {label}{args}"))
        }
        fn mount(&mut self, node: &str) -> Result<(), Fail> {
            self.go("mount", format!("mount {node}"))
        }
    }

    fn two_volumes() -> Disk {
        let v = |n: &str, mounted: bool| Volume {
            name: n.into(),
            mount: mounted.then(|| format!("/run/media/dj/{n}").into()),
            ..Volume::default()
        };
        Disk { id: "/dev/sdb".into(), removable: true, volumes: vec![v("sdb1", true), v("sdb2", false), v("sdb3", true)], ..Disk::default() }
    }

    fn run(bus: &mut Recorder, fs: Fs, version: Option<&str>) -> Result<Done, Error> {
        sequence(bus, &two_volumes(), fs, "DJ", &|_| {}, &|_| version.map(str::to_owned))
    }

    #[test]
    fn unmounts_then_new_mbr_then_one_formatted_partition_then_mounts() {
        let mut bus = Recorder::default();
        let done = run(&mut bus, Fs::Fat32, Some("FAT32")).unwrap();
        assert_eq!(
            bus.calls,
            ["unmount sdb1", "unmount sdb3", "Format(dos) sdb", "CreatePartition sdb Fat32", "Format sdb1 Fat32 DJ", "mount sdb1"]
        );
        assert_eq!(done, Done { source: "/dev/sdb1".into(), fs: Fs::Fat32, label: "DJ".into(), short: None });
    }

    #[test]
    fn a_partition_table_that_is_not_there_yet_is_waited_for() {
        let mut bus = Recorder { not_yet: 2, ..Recorder::default() };
        assert!(run(&mut bus, Fs::ExFat, None).is_ok());
        assert_eq!(bus.calls.iter().filter(|c| *c == "not yet").count(), 2);
    }

    #[test]
    fn a_small_card_that_came_out_fat16_is_formatted_again_as_fat32() {
        let mut bus = Recorder::default();
        let done = run(&mut bus, Fs::Fat32, Some("FAT16")).unwrap();
        assert_eq!(&bus.calls[4..], ["Format sdb1 Fat32 DJ", "Format sdb1 Fat32 DJ -F 32", "mount sdb1"]);
        assert_eq!(done.short, None);
    }

    #[test]
    fn without_the_extra_authorisation_the_fat16_volume_stands_and_is_reported() {
        let mut bus = Recorder { fail: Some(("format-args", Fail::Denied)), ..Recorder::default() };
        let done = run(&mut bus, Fs::Fat32, Some("FAT16")).unwrap();
        assert_eq!(done.short.as_deref(), Some("FAT16"));
        assert_eq!(bus.calls.last().map(String::as_str), Some("mount sdb1"));
    }

    #[test]
    fn a_forced_fat32_that_fails_for_another_reason_says_the_disk_has_no_file_system() {
        let mut bus = Recorder { fail: Some(("format-args", Fail::Other("mkfs failed".into()))), ..Recorder::default() };
        assert_eq!(
            run(&mut bus, Fs::Fat32, Some("FAT16")),
            Err(Error::Failed { step: Step::Partition, detail: "mkfs failed".into(), erased: true })
        );
    }

    #[test]
    fn exfat_is_never_asked_for_fat32() {
        let mut bus = Recorder::default();
        assert!(run(&mut bus, Fs::ExFat, Some("FAT16")).is_ok());
        assert!(!bus.calls.iter().any(|c| c.ends_with("-F 32")));
    }

    #[test]
    fn polkit_saying_no_at_the_start_erases_nothing_and_later_it_says_the_disk_was_erased() {
        let mut bus = Recorder { fail: Some(("table", Fail::Denied)), ..Recorder::default() };
        assert_eq!(run(&mut bus, Fs::Fat32, None), Err(Error::Denied { erased: false }));
        assert!(!bus.calls.iter().any(|c| c.starts_with("Create")));
        let mut bus = Recorder { fail: Some(("format", Fail::Denied)), ..Recorder::default() };
        assert_eq!(run(&mut bus, Fs::Fat32, None), Err(Error::Denied { erased: true }));
    }

    #[test]
    fn a_busy_volume_is_reported_before_anything_is_erased() {
        let mut bus = Recorder { fail: Some(("unmount", Fail::Busy)), ..Recorder::default() };
        assert_eq!(run(&mut bus, Fs::Fat32, None), Err(Error::InUse("sdb1".into())));
        assert_eq!(bus.calls, ["unmount sdb1"]);
    }

    #[test]
    fn a_failed_partition_step_says_the_disk_was_erased() {
        let mut bus = Recorder { fail: Some(("partition", Fail::Other("no room".into()))), ..Recorder::default() };
        assert_eq!(
            run(&mut bus, Fs::Fat32, None),
            Err(Error::Failed { step: Step::Partition, detail: "no room".into(), erased: true })
        );
    }
}
