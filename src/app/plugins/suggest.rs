//! Suggesting a reader by what a device holds: a stick with `Engine Library/` on it has an Engine DJ library, but with
//! `engine-dj` off no plugin handles the stick and the device list does not show it at all. [`devices`] looks at the
//! top-level folders of every mounted device (cheap: one directory listing, once per device per run) and, for a reader that
//! is installed and off, opens the welcome window with just that extension ([`crate::app::welcome::offer`]). The rekordbox
//! readers are built into the app and always on, so `PIONEER/` is never offered.

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

/// Offers the extensions that are off and read what a device in `devices` holds that was not looked at before. A device seen
/// while the welcome window is up is looked at again by the next call, so the offer does not replace what the user reads.
pub fn devices(app: &App, devices: &[Device]) {
    for d in devices {
        if LOOKED.with(|l| l.borrow().contains(&d.id)) || welcome::visible(app) {
            continue;
        }
        LOOKED.with(|l| l.borrow_mut().insert(d.id.clone()));
        let names: Vec<String> = std::fs::read_dir(&d.mount)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let wanted = readers(&names);
        let off: Vec<_> = current()
            .disabled
            .iter()
            .filter(|p| wanted.iter().any(|w| *w == p.manifest.name))
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
