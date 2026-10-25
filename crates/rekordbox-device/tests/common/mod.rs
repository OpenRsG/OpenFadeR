//! Shared helpers for the tests that run against a real device library. They skip unless `OPENFADER_USB` names the device
//! root (machine paths stay out of the repository); the OneLibrary key is the git-ignored `.local/onelibrary-key`.
#![allow(dead_code)] // each test binary uses a subset

use openfader_plugin::library::{LibraryPlugin, Source};
use rekordbox_device::Device;
use serde_json::json;
use std::path::{Path, PathBuf};

pub fn usb_root() -> Option<PathBuf> {
    let r = PathBuf::from(std::env::var_os("OPENFADER_USB").filter(|v| !v.is_empty())?);
    let ok = r.join("PIONEER/rekordbox").is_dir();
    if !ok {
        eprintln!("skip: OPENFADER_USB is not a device library");
    }
    ok.then_some(r)
}

/// The content of `.local/onelibrary-key` in the repository root, trimmed.
pub fn onelibrary_key() -> Option<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/onelibrary-key");
    std::fs::read_to_string(p).ok().map(|k| k.trim().to_owned())
}

/// The plugin's library of the device at `root`: the OneLibrary with a key, else the `export.pdb` one.
pub fn open(root: &Path, key: Option<&str>) -> Box<dyn Source> {
    let settings = key.map_or_else(|| json!({}), |k| json!({ "onelibrary_key": k }));
    Device::open(root, &settings).unwrap()
}

/// Every `ANLZ0000.DAT` below `dir`.
pub fn all_dats(dir: &Path, out: &mut Vec<PathBuf>) {
    for p in std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()) {
        if p.is_dir() {
            all_dats(&p, out);
        } else if p.file_name().is_some_and(|n| n == "ANLZ0000.DAT") {
            out.push(p);
        }
    }
}
