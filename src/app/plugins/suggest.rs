//! Suggesting a reader by what a device holds: a stick with `Engine Library/` on it has an Engine DJ library, but with
//! `engine-dj` off no plugin handles the stick and the device list does not show it at all. [`discover`] reads each unseen
//! device's top-level folders on the device worker; [`devices`] offers an installed reader that is off through the welcome
//! window ([`crate::app::welcome::offer`]). The rekordbox readers are built into the app and always on.

use crate::app::{App, welcome};
use openfader::{device::Device, plugin::current};
use std::{cell::RefCell, collections::HashSet};

/// A top-level folder of a device, and the extension that reads the library inside it.
const FOLDERS: &[(&str, &str)] = &[
    ("Engine Library", "engine-dj"),
    ("_Serato_", "serato"),
    ("VirtualDJ", "virtualdj"),
    ("Traktor", "traktor"),
];

thread_local! {
    /// The devices looked at this run (by id): a suggestion comes once per device, however often it is plugged in.
    static LOOKED: RefCell<HashSet<String>> = RefCell::default();
}

/// The extensions that read the libraries of the folders `names` (the entries at the root of a device), in the order of
/// [`FOLDERS`], once each. File systems differ in case, so the folder names are compared without it.
fn readers<S: AsRef<str>>(names: &[S]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for &(folder, plugin) in FOLDERS {
        let held = names.iter().any(|n| {
            let n: &str = n.as_ref();
            n.eq_ignore_ascii_case(folder)
        });
        if held && !out.contains(&plugin) {
            out.push(plugin);
        }
    }
    out
}

pub(crate) struct Detected {
    id: String,
    label: String,
    readers: Vec<&'static str>,
}

pub(crate) fn looked() -> HashSet<String> {
    LOOKED.with_borrow(Clone::clone)
}

/// Filesystem discovery for the scan worker; the UI owns which devices have already been offered.
pub(crate) fn discover(devices: &[Device], looked: &HashSet<String>) -> Vec<Detected> {
    devices
        .iter()
        .filter(|d| !looked.contains(&d.id))
        .map(|d| {
            let names: Vec<String> = std::fs::read_dir(&d.mount)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            Detected {
                id: d.id.clone(),
                label: d.label.clone(),
                readers: readers(&names),
            }
        })
        .collect()
}

/// Offers the extensions that are off and read what a device in `devices` holds that was not looked at before. A device seen
/// while the welcome window is up is looked at again by the next call, so the offer does not replace what the user reads.
pub(crate) fn devices(app: &App, devices: &[Detected]) {
    for d in devices {
        if LOOKED.with(|l| l.borrow().contains(&d.id)) || welcome::visible(app) {
            continue;
        }
        LOOKED.with(|l| l.borrow_mut().insert(d.id.clone()));
        let off: Vec<_> = current()
            .disabled
            .iter()
            .filter(|p| d.readers.iter().any(|w| *w == p.manifest.name))
            .cloned()
            .collect();
        if !off.is_empty() {
            welcome::offer(app, &d.label, &off);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::readers;

    #[test]
    fn a_folder_names_the_extension_that_reads_it() {
        assert!(readers(&["PIONEER", "Contents"]).is_empty(), "rekordbox is built in");
        assert_eq!(readers(&["Engine Library"]), ["engine-dj"]);
        assert_eq!(readers(&["_Serato_"]), ["serato"]);
        assert_eq!(readers(&["VirtualDJ"]), ["virtualdj"]);
        assert_eq!(readers(&["Traktor"]), ["traktor"]);
        // FAT keeps the case it was given but other systems do not: the match ignores it.
        assert_eq!(readers(&["engine library"]), ["engine-dj"]);
        // A stick can hold several libraries (one audio folder shared by every format); each reader once, in table order.
        assert_eq!(
            readers(&["Traktor", "_Serato_", "Contents", "_Serato_"]),
            ["serato", "traktor"]
        );
        assert!(readers(&["Music", "PIONEER2", "Engine"]).is_empty());
        assert!(readers::<&str>(&[]).is_empty());
    }
}
