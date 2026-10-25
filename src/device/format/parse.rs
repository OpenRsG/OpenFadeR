//! The parsers of what the OS tools answer, and the diskpart script. Pure text in, data out, and compiled on every OS (each OS uses
//! only its own, so the others are dead code there) so that the tests run everywhere on the answers captured from each tool.
#![allow(dead_code)] // shortcut: every OS compiles all parsers, uses its own; split per OS if the dead code ever hides a real one

use super::{Disk, Fs, Volume};

/// The partition table name of the app (`dos` = MBR, `gpt`); empty for anything else.
fn table_name(raw: &str) -> String {
    match raw {
        "dos" | "FDisk_partition_scheme" | "MBR" => "dos",
        "gpt" | "GUID_partition_scheme" | "GPT" => "gpt",
        _ => "",
    }
    .to_owned()
}

// ---- Linux: the udev database (`/run/udev/data/b<major>:<minor>`) ----

/// The value of `E:<key>=` in a udev database file.
pub fn udev_prop<'a>(udev: &'a str, key: &str) -> Option<&'a str> {
    udev.lines()
        .find_map(|l| l.strip_prefix("E:")?.strip_prefix(key)?.strip_prefix('='))
}

/// A udev text value (`ID_FS_LABEL`, `ID_MODEL`) as the user knows it: udev keeps the exact one in `<key>_ENC` with `\xNN`
/// escapes; the plain key has its spaces replaced.
pub fn udev_text(udev: &str, key: &str) -> String {
    let Some(enc) = udev_prop(udev, &format!("{key}_ENC")) else {
        return udev_prop(udev, key).unwrap_or_default().replace('_', " ");
    };
    let (b, mut out, mut i) = (enc.as_bytes(), Vec::new(), 0);
    while i < b.len() {
        let hex = (b[i] == b'\\' && b.get(i + 1) == Some(&b'x'))
            .then(|| {
                enc.get(i + 2..i + 4)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .flatten();
        match hex {
            Some(v) => {
                out.push(v);
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).trim().to_owned()
}

/// `dos` / `gpt` of a whole disk's udev data; empty when it has none.
pub fn table_from_udev(udev: &str) -> String {
    table_name(udev_prop(udev, "ID_PART_TABLE_TYPE").unwrap_or_default())
}

/// The FAT variant blkid found (`FAT12`, `FAT16`, `FAT32`).
pub fn fat_version(udev: &str) -> Option<&str> {
    udev_prop(udev, "ID_FS_VERSION")
}

// ---- macOS: `diskutil … -plist` ----

/// An XML property list, as far as `diskutil` writes it.
#[derive(Debug, PartialEq)]
pub enum Plist {
    Dict(Vec<(String, Plist)>),
    Array(Vec<Plist>),
    Str(String),
    Int(u64),
    Bool(bool),
    Other,
}

impl Plist {
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Self::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self, key: &str) -> &str {
        match self.get(key) {
            Some(Self::Str(s)) => s,
            _ => "",
        }
    }

    pub fn int(&self, key: &str) -> u64 {
        match self.get(key) {
            Some(Self::Int(n)) => *n,
            _ => 0,
        }
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Self::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn array(&self, key: &str) -> &[Plist] {
        match self.get(key) {
            Some(Self::Array(a)) => a,
            _ => &[],
        }
    }
}

/// The next tag's content (`dict`, `/dict`, `string/`), skipping text, `<?xml …?>` and `<!DOCTYPE …>`.
fn tag<'a>(s: &mut &'a str) -> Option<&'a str> {
    loop {
        let open = s.find('<')?;
        let end = s[open..].find('>')? + open;
        let t = &s[open + 1..end];
        *s = &s[end + 1..];
        if !t.starts_with('?') && !t.starts_with('!') {
            return Some(t);
        }
    }
}

/// The text up to the next tag, with the XML escapes undone.
fn text(s: &mut &str) -> String {
    let end = s.find('<').unwrap_or(s.len());
    let t = &s[..end];
    *s = &s[end..];
    t.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn value(s: &mut &str, t: &str) -> Option<Plist> {
    Some(match t {
        "dict" => {
            let mut d = Vec::new();
            loop {
                match tag(s)? {
                    "/dict" => break Plist::Dict(d),
                    "key" => {
                        let k = text(s);
                        tag(s)?; // </key>
                        let t = tag(s)?;
                        d.push((k, value(s, t)?));
                    }
                    _ => return None,
                }
            }
        }
        "array" => {
            let mut a = Vec::new();
            loop {
                match tag(s)? {
                    "/array" => break Plist::Array(a),
                    t => a.push(value(s, t)?),
                }
            }
        }
        "true/" => Plist::Bool(true),
        "false/" => Plist::Bool(false),
        "string/" => Plist::Str(String::new()),
        "string" | "integer" | "real" | "date" | "data" => {
            let v = text(s);
            tag(s)?; // the closing tag
            match t {
                "string" => Plist::Str(v),
                "integer" => Plist::Int(v.trim().parse().unwrap_or(0)),
                _ => Plist::Other,
            }
        }
        _ => Plist::Other,
    })
}

/// The top-level value of a property list.
pub fn plist(text: &str) -> Option<Plist> {
    let mut s = text;
    loop {
        let t = tag(&mut s)?;
        if t.starts_with("plist") {
            let t = tag(&mut s)?;
            return value(&mut s, t);
        }
    }
}

/// `dos` / `gpt` from `diskutil info -plist` of a whole disk (`Content`); empty when unknown.
pub fn table_from_diskutil(info: &str) -> String {
    plist(info).map_or_else(String::new, |p| table_name(p.str("Content")))
}

/// The whole disk of a BSD device node: `/dev/disk4s1` → `disk4`; `None` for anything that is no `/dev/disk…`.
pub fn mac_whole(source: &str) -> Option<String> {
    let n = source.strip_prefix("/dev/disk")?;
    let digits = n.len() - n.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (digits > 0).then(|| format!("disk{}", &n[..digits]))
}

/// The disk of `diskutil info -plist <whole disk>` and its volumes from `diskutil list -plist <whole disk>`; `system` is left for
/// the caller to fill.
pub fn disk_from_diskutil(info: &str, list: &str) -> Option<Disk> {
    let (info, list) = (plist(info)?, plist(list)?);
    let whole = list.array("AllDisksAndPartitions").first()?;
    let id = info.str("DeviceNode");
    let volumes = whole
        .array("Partitions")
        .iter()
        .map(|p| Volume {
            name: p.str("DeviceIdentifier").to_owned(),
            label: p.str("VolumeName").to_owned(),
            fs: p.str("Content").to_owned(),
            size: p.int("Size"),
            mount: Some(p.str("MountPoint"))
                .filter(|m| !m.is_empty())
                .map(Into::into),
        })
        .collect();
    Some(Disk {
        id: if id.is_empty() {
            format!("/dev/{}", info.str("DeviceIdentifier"))
        } else {
            id.to_owned()
        },
        model: info.str("MediaName").to_owned(),
        size: info.int("TotalSize").max(info.int("Size")),
        removable: info.bool("Internal") == Some(false)
            && info.bool("RemovableMediaOrExternalDevice") != Some(false),
        system: false,
        volumes,
    })
}

// ---- Windows ----

/// `dos` / `gpt` from the `PartitionStyle` of `IOCTL_DISK_GET_PARTITION_INFO_EX` (0 MBR, 1 GPT, 2 raw).
pub fn table_from_style(style: u32) -> String {
    match style {
        0 => "dos",
        1 => "gpt",
        _ => "",
    }
    .to_owned()
}

/// The disk and its volumes from the PowerShell answer of
/// `{Model, Size, Parts: [{Letter, Number, Size, Label, Fs}]}` (`ConvertTo-Json`); `system` is left for the caller to fill.
pub fn disk_from_powershell(number: u32, json: &str) -> Option<Disk> {
    let v: serde_json::Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()?;
    let parts = match &v["Parts"] {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Null => Vec::new(),
        one => vec![one.clone()],
    };
    let volumes = parts
        .iter()
        .map(|p| {
            // `DriveLetter` is a character: PowerShell writes it as a string or as its code, and NUL when there is none.
            let letter = match &p["Letter"] {
                serde_json::Value::String(s) => s.trim_end_matches(':').chars().next(),
                serde_json::Value::Number(n) => n.as_u64().and_then(|c| char::from_u32(c as u32)),
                _ => None,
            }
            .filter(char::is_ascii_alphabetic);
            let letter = letter.map(String::from).unwrap_or_default();
            Volume {
                name: if letter.is_empty() {
                    format!("#{}", p["Number"].as_u64().unwrap_or(0))
                } else {
                    format!("{letter}:")
                },
                label: p["Label"].as_str().unwrap_or_default().to_owned(),
                fs: p["Fs"].as_str().unwrap_or_default().to_lowercase(),
                size: p["Size"].as_u64().unwrap_or(0),
                mount: (!letter.is_empty()).then(|| format!("{letter}:\\").into()),
            }
        })
        .collect();
    Some(Disk {
        id: number.to_string(),
        model: v["Model"].as_str().unwrap_or_default().trim().to_owned(),
        size: v["Size"].as_u64().unwrap_or(0),
        removable: true,
        system: false,
        volumes,
    })
}

/// The diskpart script that empties disk `disk` and leaves one primary MBR partition of type `0x0c` (FAT32) or `0x07`
/// (exFAT) with drive letter `letter`; the file system is made afterwards (`fatfs` for FAT32, `format` for exFAT).
pub fn diskpart_script(disk: u32, fs: Fs, letter: char) -> String {
    let id = match fs {
        Fs::Fat32 => "0c",
        Fs::ExFat => "07",
    };
    [
        format!("select disk {disk}"),
        "clean".into(),
        "convert mbr".into(),
        "create partition primary".into(),
        format!("set id={id} override"),
        format!("assign letter={letter}"),
        "exit".into(),
    ]
    .join("\r\n")
        + "\r\n"
}

/// The command line of the elevated relaunch that does the whole format (`openfader --format-disk …`): disk number, file
/// system, drive letter for the new volume, a file the elevated process writes its failure text to, the label. Windows'
/// own quoting rules apply: the label and the path hold no quote.
pub fn elevated_args(disk: u32, fs: Fs, letter: char, result: &str, label: &str) -> String {
    let fs = match fs {
        Fs::Fat32 => "fat32",
        Fs::ExFat => "exfat",
    };
    format!("--format-disk {disk} {fs} {letter} \"{result}\" \"{label}\"")
}

/// [`elevated_args`] read back from the arguments after `--format-disk`.
pub fn parse_elevated(args: &[String]) -> Option<(u32, Fs, char, String, String)> {
    let [disk, fs, letter, result, label] = args else {
        return None;
    };
    let fs = match fs.as_str() {
        "fat32" => Fs::Fat32,
        "exfat" => Fs::ExFat,
        _ => return None,
    };
    let mut l = letter.chars();
    let letter = l
        .next()
        .filter(|c| c.is_ascii_alphabetic() && l.next().is_none())?;
    Some((
        disk.parse().ok()?,
        fs,
        letter,
        result.clone(),
        label.clone(),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;

    const UDEV_DISK: &str = "S:disk/by-id/usb-SanDisk_Ultra-0:0\nE:ID_MODEL=Ultra\nE:ID_MODEL_ENC=Ultra\\x20Fit\nE:ID_PART_TABLE_UUID=0f2a1c3b\nE:ID_PART_TABLE_TYPE=dos\n";
    const UDEV_PART: &str = "E:ID_FS_LABEL=MY_KEY\nE:ID_FS_LABEL_ENC=MY\\x20KEY\nE:ID_FS_TYPE=vfat\nE:ID_FS_VERSION=FAT32\nE:ID_FS_USAGE=filesystem\n";

    #[test]
    fn linux_partition_table_from_udev() {
        assert_eq!(table_from_udev(UDEV_DISK), "dos");
        assert_eq!(table_from_udev("E:ID_PART_TABLE_TYPE=gpt\n"), "gpt");
        assert_eq!(table_from_udev("E:ID_PART_TABLE_TYPE=mac\n"), "");
        assert_eq!(table_from_udev(UDEV_PART), "", "a partition has none");
        assert_eq!(udev_text(UDEV_DISK, "ID_MODEL"), "Ultra Fit");
        assert_eq!(udev_text(UDEV_PART, "ID_FS_LABEL"), "MY KEY");
        assert_eq!(
            udev_text("E:ID_FS_LABEL=A_B\n", "ID_FS_LABEL"),
            "A B",
            "no _ENC: the plain one"
        );
        assert_eq!(fat_version(UDEV_PART), Some("FAT32"));
    }

    const INFO_DOS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Content</key>
	<string>FDisk_partition_scheme</string>
	<key>DeviceIdentifier</key>
	<string>disk4</string>
	<key>DeviceNode</key>
	<string>/dev/disk4</string>
	<key>Internal</key>
	<false/>
	<key>MediaName</key>
	<string>SanDisk Ultra Media</string>
	<key>ParentWholeDisk</key>
	<string>disk4</string>
	<key>RemovableMediaOrExternalDevice</key>
	<true/>
	<key>TotalSize</key>
	<integer>15500000000</integer>
</dict>
</plist>
"#;
    const LIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>AllDisksAndPartitions</key>
	<array>
		<dict>
			<key>Content</key>
			<string>FDisk_partition_scheme</string>
			<key>DeviceIdentifier</key>
			<string>disk4</string>
			<key>Partitions</key>
			<array>
				<dict>
					<key>Content</key>
					<string>DOS_FAT_32</string>
					<key>DeviceIdentifier</key>
					<string>disk4s1</string>
					<key>MountPoint</key>
					<string>/Volumes/MY KEY &amp; CO</string>
					<key>Size</key>
					<integer>15499000000</integer>
					<key>VolumeName</key>
					<string>MY KEY &amp; CO</string>
				</dict>
				<dict>
					<key>Content</key>
					<string>Apple_HFS</string>
					<key>DeviceIdentifier</key>
					<string>disk4s2</string>
					<key>Size</key>
					<integer>1000</integer>
					<key>VolumeName</key>
					<string/>
				</dict>
			</array>
			<key>Size</key>
			<integer>15500000000</integer>
		</dict>
	</array>
	<key>WholeDisks</key>
	<array>
		<string>disk4</string>
	</array>
</dict>
</plist>
"#;

    #[test]
    fn macos_partition_table_and_disk_from_diskutil() {
        assert_eq!(table_from_diskutil(INFO_DOS), "dos");
        assert_eq!(
            table_from_diskutil(
                &INFO_DOS.replace("FDisk_partition_scheme", "GUID_partition_scheme")
            ),
            "gpt"
        );
        assert_eq!(
            table_from_diskutil(
                &INFO_DOS.replace("FDisk_partition_scheme", "Apple_partition_scheme")
            ),
            ""
        );
        assert_eq!(table_from_diskutil("not a plist"), "");
        assert_eq!(mac_whole("/dev/disk4s1").as_deref(), Some("disk4"));
        assert_eq!(mac_whole("/dev/disk12").as_deref(), Some("disk12"));
        assert_eq!(mac_whole("/dev/rdisk4s2"), None);
        assert_eq!(mac_whole("map auto_home"), None);
        let d = disk_from_diskutil(INFO_DOS, LIST).unwrap();
        assert_eq!(
            (d.id.as_str(), d.model.as_str(), d.size, d.removable),
            ("/dev/disk4", "SanDisk Ultra Media", 15_500_000_000, true)
        );
        assert_eq!(d.volumes.len(), 2);
        assert_eq!(d.volumes[0].label, "MY KEY & CO");
        assert_eq!(
            d.volumes[0].mount.as_deref(),
            Some(std::path::Path::new("/Volumes/MY KEY & CO"))
        );
        assert_eq!(
            (d.volumes[1].label.as_str(), d.volumes[1].mount.is_none()),
            ("", true)
        );
        let internal = INFO_DOS.replace(
            "<key>Internal</key>\n\t<false/>",
            "<key>Internal</key>\n\t<true/>",
        );
        assert!(!disk_from_diskutil(&internal, LIST).unwrap().removable);
    }

    #[test]
    fn windows_partition_style_and_disk_from_powershell() {
        assert_eq!(
            (
                table_from_style(0).as_str(),
                table_from_style(1).as_str(),
                table_from_style(2).as_str()
            ),
            ("dos", "gpt", "")
        );
        let json = r#"{"Model":"SanDisk Ultra USB Device","Size":15500000000,"Parts":[{"Letter":"E","Number":1,"Size":15499000000,"Label":"MY KEY","Fs":"FAT32"},{"Letter":null,"Number":2,"Size":1000,"Label":null,"Fs":null}]}"#;
        let d = disk_from_powershell(3, json).unwrap();
        assert_eq!(
            (d.id.as_str(), d.model.as_str(), d.size),
            ("3", "SanDisk Ultra USB Device", 15_500_000_000)
        );
        assert_eq!(
            d.volumes[0],
            Volume {
                name: "E:".into(),
                label: "MY KEY".into(),
                fs: "fat32".into(),
                size: 15_499_000_000,
                mount: Some("E:\\".into())
            }
        );
        assert_eq!(
            (d.volumes[1].name.as_str(), d.volumes[1].mount.is_none()),
            ("#2", true)
        );
        // PowerShell unwraps a one-element array, and `DriveLetter` can arrive as a character code.
        let one = r#"{"Model":"X","Size":5,"Parts":{"Letter":69,"Number":1,"Size":5,"Label":"","Fs":"exFAT"}}"#;
        let d = disk_from_powershell(1, one).unwrap();
        assert_eq!(
            (
                d.volumes.len(),
                d.volumes[0].name.as_str(),
                d.volumes[0].fs.as_str()
            ),
            (1, "E:", "exfat")
        );
        assert!(disk_from_powershell(1, "no json").is_none());
    }

    #[test]
    fn the_diskpart_script_empties_the_disk_and_types_the_partition() {
        assert_eq!(
            diskpart_script(3, Fs::Fat32, 'E'),
            "select disk 3\r\nclean\r\nconvert mbr\r\ncreate partition primary\r\nset id=0c override\r\nassign letter=E\r\nexit\r\n"
        );
        assert!(
            diskpart_script(12, Fs::ExFat, 'F').contains("select disk 12\r\n")
                && diskpart_script(12, Fs::ExFat, 'F').contains("set id=07 ")
        );
    }

    #[test]
    fn the_elevated_command_line_round_trips() {
        let line = elevated_args(
            3,
            Fs::Fat32,
            'E',
            r"C:\Users\DJ Name\AppData\Local\Temp\of-format.txt",
            "MY KEY",
        );
        assert_eq!(
            line,
            r#"--format-disk 3 fat32 E "C:\Users\DJ Name\AppData\Local\Temp\of-format.txt" "MY KEY""#
        );
        // What the elevated process receives after the program name and the flag (Windows has split the quotes).
        let got: Vec<String> = [
            "3",
            "fat32",
            "E",
            r"C:\Users\DJ Name\AppData\Local\Temp\of-format.txt",
            "MY KEY",
        ]
        .map(String::from)
        .into();
        assert_eq!(
            parse_elevated(&got),
            Some((
                3,
                Fs::Fat32,
                'E',
                r"C:\Users\DJ Name\AppData\Local\Temp\of-format.txt".into(),
                "MY KEY".into()
            ))
        );
        let blank: Vec<String> = ["12", "exfat", "F", "r.txt", ""].map(String::from).into();
        assert_eq!(
            parse_elevated(&blank),
            Some((12, Fs::ExFat, 'F', "r.txt".into(), String::new()))
        );
        assert_eq!(parse_elevated(&got[..4]), None);
        assert_eq!(
            parse_elevated(&["x", "fat32", "E", "r", ""].map(String::from)),
            None
        );
        assert_eq!(
            parse_elevated(&["3", "ntfs", "E", "r", ""].map(String::from)),
            None
        );
        assert_eq!(
            parse_elevated(&["3", "fat32", "1", "r", ""].map(String::from)),
            None,
            "no drive letter"
        );
    }
}
