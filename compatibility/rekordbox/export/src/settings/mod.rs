//! Device settings (rekordbox Preferences > DJ SYSTEM, and the per-device panel): the four `PIONEER/*SETTING.DAT` files
//! as an editable, loss-free model ([`Settings`]), everything else a device export is configured with ([`DeviceConfig`]:
//! device name, background colour, category / sort / column lists, colour comments, hot cue auto load), and the three
//! host ops over them: [`schema()`] (the pages the host draws), [`read()`] and [`write()`] (the files of a device).
//!
//! Children: [`file`] (the file container and [`Settings`]), [`my`] (`MYSETTING.DAT`, `MYSETTING2.DAT`), [`djm`]
//! (`DJMMYSETTING.DAT`), [`dev`] (`DEVSETTING.DAT`): one field table each; [`browse`]: the category / sort / colour tables;
//! [`schema`]: the host's [`SettingsSchema`](openfader_plugin::library::SettingsSchema) over all of it.
//!
//! File format (spec: rekordcrate `src/setting.rs`, <https://github.com/Holzhaus/rekordcrate>, fetched 2026-10-13;
//! observed: the three files of a real rekordbox 7 export, which [`Settings::to_bytes`] reproduces byte for byte): little
//! endian, `len_stringdata u4 = 0x60`, brand / software / version as three NUL-padded 32-byte strings, `len_data u4`, the
//! data, `crc u2`, `0 u2`. The CRC is CRC-16/XMODEM over the data, except in `DJMMYSETTING.DAT` where it covers every
//! byte before it (both verified on the real files). A field is one byte of the data ([`Field`]); bytes no field names
//! are kept as read, so editing one setting never disturbs another.
//!
//! A [`DeviceConfig`] is addressed by string keys ([`DeviceConfig::get`] / [`DeviceConfig::set`]), the keys of the host's
//! stored values (the host keeps only the differences from the defaults).

pub mod browse;
mod dev;
mod djm;
mod file;
mod my;
mod schema;

pub use file::Settings;
pub use schema::schema;

use openfader_plugin::library::{SettingsRead, SettingsWritten};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// The two-valued OFF / ON choice of the player and mixer tables.
const OFF_ON: &[(u8, &str)] = &[(0x80, "OFF"), (0x81, "ON")];

/// One of the four settings files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    My,
    My2,
    Djm,
    Dev,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::My, Kind::My2, Kind::Djm, Kind::Dev];

    pub const fn file_name(self) -> &'static str {
        match self {
            Kind::My => "MYSETTING.DAT",
            Kind::My2 => "MYSETTING2.DAT",
            Kind::Djm => "DJMMYSETTING.DAT",
            Kind::Dev => "DEVSETTING.DAT",
        }
    }

    const fn data_len(self) -> usize {
        match self {
            Kind::My | Kind::My2 => 40,
            Kind::Djm => 52,
            Kind::Dev => 32,
        }
    }

    /// Brand and version strings of the header (observed in the three real files; `DEVSETTING.DAT` the spec's `6.6.1`).
    const fn header(self) -> (&'static str, &'static str) {
        match self {
            Kind::My | Kind::My2 => ("PIONEER", "0.001"),
            Kind::Djm => ("PioneerDJ", "1.000"),
            Kind::Dev => ("PIONEER DJ", "6.6.1"),
        }
    }

    fn default_data(self) -> Vec<u8> {
        match self {
            Kind::My => my::data(),
            Kind::My2 => my::data2(),
            Kind::Djm => djm::data(),
            Kind::Dev => dev::data(),
        }
    }

    /// The fields of this file, in byte order.
    pub fn fields(self) -> &'static [Field] {
        match self {
            Kind::My => my::MY,
            Kind::My2 => my::MY2,
            Kind::Djm => djm::FIELDS,
            Kind::Dev => dev::FIELDS,
        }
    }
}

/// One setting: a byte of a file's data with a closed set of values.
#[derive(Debug)]
pub struct Field {
    pub kind: Kind,
    /// Stable name (the key `dat.<id>`).
    pub id: &'static str,
    /// Where it sits on the player / mixer, e.g. `PLAYER > DJ SETTING`.
    pub group: &'static str,
    pub label: &'static str,
    /// Byte offset in the file's data.
    pub offset: usize,
    /// `(value byte, label)` in the order rekordbox lists them.
    pub choices: &'static [(u8, &'static str)],
}

impl Field {
    const fn new(
        kind: Kind,
        id: &'static str,
        group: &'static str,
        label: &'static str,
        offset: usize,
        choices: &'static [(u8, &'static str)],
    ) -> Self {
        Self {
            kind,
            id,
            group,
            label,
            offset,
            choices,
        }
    }
}

/// Every field of every file, in file order.
pub fn fields() -> impl Iterator<Item = &'static Field> {
    Kind::ALL.into_iter().flat_map(Kind::fields)
}

/// Name of the file that carries the custom jog image on the device (inferred to sit in the settings folder, like the four
/// `.DAT` files it is listed with).
const JOG_IMAGE_FILE: &str = "rekordbox_customimage.jpeg";

/// Everything one device export is configured with. `Default` reproduces the constants of the observed export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConfig {
    pub settings: Settings,
    /// Name shown on the player; empty = none (OneLibrary `property.deviceName`).
    pub device_name: String,
    /// 0 = none, 1..=8 = the colour of that [`browse::COLORS`] entry (OneLibrary `property.backGroundColorType`; the
    /// values are inferred, only 0 is observed).
    pub background: u8,
    /// Active category ids in display order ([`browse::CATEGORIES`]).
    pub categories: Vec<u16>,
    /// Active sort ids in display order ([`browse::SORTS`]).
    pub sorts: Vec<u16>,
    /// Sort id shown as the sub column right of the titles ("Column" tab), if any.
    pub column: Option<u16>,
    /// The eight colour comments (OneLibrary `color.name`, `export.pdb` colours table).
    pub colors: [String; 8],
    /// `content.isHotCueAutoLoadOn` of every exported track (observed 1 in the real export).
    pub hotcue_autoload: bool,
    /// Image On-Jog Display: false = ARTWORK, true = CUSTOMIMAGE (see [`DeviceConfig::jog_image`]).
    pub jog_custom: bool,
    /// "My Settings": whether the settings of every item are applied to the device by each export (manual p104). Off keeps
    /// the `.DAT` files the device already has (and "Apply on all connected devices" still writes them on demand).
    pub auto_apply: bool,
    /// JPEG bytes of the custom image, written as [`JOG_IMAGE_FILE`] when `jog_custom`. Not a [`DeviceConfig::get`] /
    /// [`DeviceConfig::set`] key: [`DeviceConfig::from_values`] reads it from the file the `jog_image` value names.
    pub jog_image: Vec<u8>,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            device_name: String::new(),
            background: 0,
            categories: browse::DEFAULT_CATEGORIES.to_vec(),
            sorts: browse::DEFAULT_SORTS.to_vec(),
            column: None,
            colors: browse::COLORS.map(String::from),
            hotcue_autoload: true,
            jog_custom: false,
            auto_apply: true,
            jog_image: Vec::new(),
        }
    }
}

fn ids(v: &[u16]) -> String {
    v.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
}

/// `"2,3,4"` → ids, each accepted by `known` and none repeated.
fn parse_ids(s: &str, known: impl Fn(u16) -> bool) -> Result<Vec<u16>, String> {
    let mut out = Vec::new();
    for part in s.split(',').filter(|p| !p.is_empty()) {
        let id: u16 = part
            .trim()
            .parse()
            .map_err(|_| format!("'{part}' is not an id"))?;
        if !known(id) || out.contains(&id) {
            return Err(format!("id {id} is unknown or repeated"));
        }
        out.push(id);
    }
    Ok(out)
}

/// Index into [`DeviceConfig::colors`] of the key `color.<1..=8>`.
fn color_slot(key: &str) -> Option<usize> {
    key.strip_prefix("color.")?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
}

/// The field of the key `dat.<id>`.
fn dat_field(key: &str) -> Option<&'static Field> {
    let id = key.strip_prefix("dat.")?;
    fields().find(|f| f.id == id)
}

impl DeviceConfig {
    pub fn get(&self, key: &str) -> Option<String> {
        Some(match key {
            "device_name" => self.device_name.clone(),
            "background" => self.background.to_string(),
            "categories" => ids(&self.categories),
            "sorts" => ids(&self.sorts),
            "column" => self.column.map(|c| c.to_string()).unwrap_or_default(),
            "hotcue_autoload" => u8::from(self.hotcue_autoload).to_string(),
            "jog_custom" => u8::from(self.jog_custom).to_string(),
            "auto_apply" => u8::from(self.auto_apply).to_string(),
            _ => {
                return color_slot(key)
                    .and_then(|i| self.colors.get(i).cloned())
                    .or_else(|| dat_field(key).map(|f| self.settings.byte(f).to_string()));
            }
        })
    }

    /// Sets `key` from its stored text; an unknown key or a value that is not valid for it changes nothing.
    pub fn set(&mut self, key: &str, v: &str) -> Result<(), String> {
        let bool = || match v {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(format!("{key}: '{v}' is not 0 or 1")),
        };
        match key {
            "device_name" => self.device_name = v.to_owned(),
            "background" => {
                self.background = v
                    .parse()
                    .ok()
                    .filter(|&b| b <= 8)
                    .ok_or_else(|| format!("{key}: '{v}' is not 0..=8"))?
            }
            "categories" => {
                self.categories = parse_ids(v, |id| browse::category_menu(id).is_some())?
            }
            "sorts" => {
                self.sorts = parse_ids(v, |id| browse::sort_menu(id).is_some())?;
                self.column = self.column.filter(|c| self.sorts.contains(c));
            }
            "column" => {
                let c = if v.is_empty() {
                    None
                } else {
                    Some(
                        v.parse::<u16>()
                            .ok()
                            .filter(|&c| browse::is_column(c))
                            .ok_or_else(|| format!("{key}: '{v}' is not a sort id"))?,
                    )
                };
                self.column = c.filter(|c| self.sorts.contains(c));
            }
            "hotcue_autoload" => self.hotcue_autoload = bool()?,
            "jog_custom" => self.jog_custom = bool()?,
            "auto_apply" => self.auto_apply = bool()?,
            _ => {
                if let Some(slot) = color_slot(key).and_then(|i| self.colors.get_mut(i)) {
                    *slot = v.to_owned();
                    return Ok(());
                }
                let f = dat_field(key).ok_or_else(|| format!("unknown key {key}"))?;
                let b: u8 = v
                    .parse()
                    .map_err(|_| format!("{key}: '{v}' is not a byte"))?;
                if !self.settings.set_byte(f, b) {
                    return Err(format!("{key}: {b:#x} is not one of the choices"));
                }
            }
        }
        Ok(())
    }

    /// Table 17 rows of `export.pdb` for [`Self::categories`] (see [`browse`]).
    pub fn table17(&self) -> Vec<[u16; 4]> {
        browse::table17(&self.categories)
    }

    /// Table 18 rows of `export.pdb` for [`Self::sorts`].
    pub fn table18(&self) -> Vec<[u16; 4]> {
        browse::table18(&self.sorts)
    }

    /// Files to write under `<device>/PIONEER/`: the four `.DAT` files (unless [`Self::auto_apply`] is off), plus the
    /// custom jog image when chosen.
    pub fn files(&self) -> Vec<(&'static str, Vec<u8>)> {
        let mut f = if self.auto_apply {
            self.settings.to_bytes()
        } else {
            Vec::new()
        };
        if self.jog_custom && !self.jog_image.is_empty() {
            f.push((JOG_IMAGE_FILE, self.jog_image.clone()));
        }
        f
    }

    /// Writes [`Self::files`] into `<root>/PIONEER` (created when missing), all or nothing: they switch together
    /// ([`openfader_plugin::library::commit`]).
    pub fn write_files(&self, root: &Path) -> Result<(), String> {
        let mut c = openfader_plugin::library::commit::Commit::new(root, crate::OWNER)?;
        for (name, bytes) in self.files() {
            c.stage(&root.join(DIR).join(name), &bytes)?;
        }
        c.commit()
    }

    /// The default configuration with the `.DAT` files of `dir` (`<device>/PIONEER`) read back: what the player changed on
    /// its own. Everything else keeps its default.
    pub fn read(dir: &Path) -> Result<Self, String> {
        Ok(Self {
            settings: Settings::read(dir)?,
            ..Self::default()
        })
    }

    /// The default configuration with the host's stored `values` (key → text, see [`schema()`]) on top. Infallible: an
    /// unknown key or an invalid value is ignored. `jog_image` is the path of a JPEG file (a file that is not one is
    /// ignored).
    pub fn from_values(values: &BTreeMap<String, String>) -> Self {
        let mut c = Self::default();
        // `column` is only valid against the final `sorts`, whatever order the keys have.
        let mut pairs: Vec<_> = values.iter().collect();
        pairs.sort_by_key(|(k, _)| *k == "column");
        for (k, v) in pairs {
            if k == "jog_image" {
                c.jog_image = std::fs::read(v)
                    .ok()
                    .filter(|b| b.starts_with(&[0xff, 0xd8, 0xff]))
                    .unwrap_or_default();
            } else if !(v.is_empty() && color_slot(k).is_some()) {
                // An empty colour comment restores the colour's own name.
                let _ = c.set(k, v);
            }
        }
        c
    }
}

/// Folder of the settings files on a device.
pub const DIR: &str = "PIONEER";

/// Stable fingerprint (FNV-1a 64 over names and bytes) of the `.DAT` files in `dir`; empty when there are none.
fn fingerprint(dir: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut any = false;
    for k in Kind::ALL {
        let Ok(bytes) = std::fs::read(dir.join(k.file_name())) else {
            continue;
        };
        any = true;
        for b in k.file_name().bytes().chain(bytes) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    if any {
        format!("{h:016x}")
    } else {
        String::new()
    }
}

/// `settings_read`: the `dat.<id>` values of the files the device has, and their fingerprint.
pub fn read(root: &Path, _settings: &Value) -> Result<SettingsRead, String> {
    let dir = root.join(DIR);
    let cfg = DeviceConfig::read(&dir)?;
    let values = Kind::ALL
        .into_iter()
        .filter(|k| dir.join(k.file_name()).is_file())
        .flat_map(Kind::fields)
        .filter_map(|f| {
            let key = format!("dat.{}", f.id);
            Some((key.clone(), cfg.get(&key)?))
        })
        .collect();
    Ok(SettingsRead {
        values,
        fingerprint: fingerprint(&dir),
    })
}

/// `settings_write`: the four `.DAT` files (and the custom jog image when chosen) of `values` into the device's settings
/// folder. It is an explicit write, so `auto_apply` (which only gates exports) is not consulted.
pub fn write(
    root: &Path,
    values: &BTreeMap<String, String>,
    _settings: &Value,
) -> Result<SettingsWritten, String> {
    let dir = root.join(DIR);
    let mut cfg = DeviceConfig::from_values(values);
    cfg.auto_apply = true;
    cfg.write_files(root)?;
    Ok(SettingsWritten {
        fingerprint: fingerprint(&dir),
    })
}

#[cfg(test)]
mod tests;
