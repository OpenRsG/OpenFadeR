//! Shared rekordbox library helpers: path remapping, `file://` locations, DeviceSQL byte readers ([`pdb`]),
//! analysis files ([`anlz`]), SQLCipher databases ([`sql`]) and library helpers ([`list`]).

pub mod anlz;
pub mod list;
pub mod pdb;
pub mod sql;

pub use openfader_plugin::library::pathmap::{
    PathMap, location, parse_path_map, path_map, remap_path,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A plugin setting (`settings` of a request) as trimmed text; `None` when absent, not a string or empty.
pub fn setting(settings: &Value, id: &str) -> Option<String> {
    let s = settings.get(id)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// The SQLCipher key of `exportLibrary.db` (OneLibrary, formerly Device Library Plus): the plugin's `onelibrary_key` setting
/// when set, else the built-in one. It is the same key on every device and already public (pyrekordbox ships it,
/// https://github.com/dylanljones/pyrekordbox/blob/master/docs/source/formats/devicelib_plus.md); it is kept XOR-masked so the
/// binary does not carry it as plain text.
pub fn onelibrary_key(settings: &Value) -> String {
    const MASK: &[u8] = b"OpenFadeR";
    const MASKED: [u8; 64] = [
        0x3d, 0x48, 0x02, 0x0a, 0x22, 0x0f, 0x16, 0x51, 0x39, 0x77, 0x44, 0x52, 0x56, 0x75, 0x51,
        0x05, 0x17, 0x64, 0x2c, 0x01, 0x1f, 0x0c, 0x2d, 0x0a, 0x54, 0x00, 0x3e, 0x79, 0x01, 0x1c,
        0x1a, 0x2b, 0x03, 0x57, 0x11, 0x20, 0x2d, 0x12, 0x1d, 0x56, 0x76, 0x54, 0x0e, 0x08, 0x65,
        0x7b, 0x06, 0x00, 0x14, 0x70, 0x55, 0x0d, 0x50, 0x3d, 0x77, 0x16, 0x0b, 0x1c, 0x37, 0x13,
        0x1d, 0x14, 0x3e, 0x3c,
    ];
    setting(settings, "onelibrary_key").unwrap_or_else(|| {
        MASKED
            .iter()
            .zip(MASK.iter().cycle())
            .map(|(b, m)| char::from(b ^ m))
            .collect()
    })
}

/// Adds the path to an IO error: `path: message`.
pub fn at<T, E: std::fmt::Display>(r: Result<T, E>, p: &Path) -> Result<T, String> {
    r.map_err(|e| format!("{}: {e}", p.display()))
}

/// `rel` as a device library stores a path (leading `/` allowed) under the device root `root`.
pub fn on_device(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.trim_start_matches('/'))
}
