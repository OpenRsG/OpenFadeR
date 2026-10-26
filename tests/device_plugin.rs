//! The device ops through the host, with the built-in device plugin (rekordbox-device) on
//! a fake device root: the app finds the device and its plugin, exports a collection to it, reads the library back with its
//! analysis, takes edits back (Update Collection), refreshes it, changes the device's settings, deletes from it, and backs it up
//! and restores it. The core knows no layout: it only follows what the plugin answers.

mod common;

use openfader::device::{self, backup, update};
use openfader::library::{self, Library, snapshot::Snapshot};
use openfader::plugin::device::{Control, ExportOptions, Feature, RemoveTarget};
use openfader::store::Store;
use openfader_plugin::library::{FieldKind, split_ref};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::Ordering::Relaxed};

fn scanned(stick: &Path) -> device::Device {
    device::forget(stick);
    device::scan()
        .into_iter()
        .find(|d| d.mount == stick)
        .expect("the fake device is listed")
}

fn titles(lib: &dyn Library, playlist: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = lib
        .tracks(playlist)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    v.sort();
    v
}

/// The file called `name` of size `size` below `dir`.
fn find(dir: &Path, name: &str, size: u64) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        if p.is_dir() {
            find(&p, name, size)
        } else {
            (p.file_name()? == name && p.metadata().ok()?.len() == size).then_some(p)
        }
    })
}

#[test]
fn the_plugin_exports_reads_updates_edits_and_backs_up_a_device() {
    common::device_plugin();
    let root = common::fresh_dir("device-plugin");
    let (music, stick) = (root.join("music"), root.join("stick"));
    std::fs::create_dir_all(music.join("House")).unwrap();
    std::fs::create_dir_all(&stick).unwrap();
    for (file, secs) in [("House/a.wav", 4.0), ("House/b.wav", 5.0), ("c.wav", 6.0)] {
        common::click_wav(&music.join(file), 120.0, secs);
    }
    // SAFETY: set before the first scan; the only test of this binary.
    unsafe { std::env::set_var("OPENFADER_DEVICE_ROOTS", &stick) };

    // The device is found, with the plugin that can write a library to it.
    let dev = scanned(&stick);
    assert!(!dev.has_library() && dev.info.can_write && dev.can(Feature::Export));
    let provider = dev.provider().expect("a plugin handles the device");

    // Export the whole collection.
    let collection = library::open(&music).unwrap();
    let snap = Snapshot::take(&*collection).unwrap();
    assert_eq!(snap.len(), 3);
    let ctl = Arc::new(Control::default());
    let report = provider
        .export(&stick, &snap, &ExportOptions::default(), &ctl)
        .unwrap();
    assert_eq!((report.tracks, report.added, report.updated), (3, 3, 0));
    assert!(!ctl.phase().is_empty(), "the plugin reported its progress");

    // The library reads back through the plugin: tracks, playlists, and their analysis (the host's analysis of the audio).
    let dev = scanned(&stick);
    assert!(dev.has_library());
    let lib = device::open_library(&dev).unwrap();
    assert_eq!(titles(&*lib, None), ["a", "b", "c"]);
    let pls = lib.playlists().unwrap();
    assert!(pls.iter().any(|p| p.name == "House"));
    let tracks = lib.tracks(None).unwrap();
    assert!(tracks.iter().all(|t| {
        t.file
            .as_ref()
            .is_some_and(|f| f.starts_with(&stick) && f.is_file())
    }));
    let a = lib
        .analysis(&tracks[0])
        .expect("analysis of a device track");
    assert!(a.beats.len() > 4, "{} beats", a.beats.len());
    assert!(!a.detail.is_empty() || !a.three_band_detail.is_empty());
    // A stored track names its plugin; the analysis is found again without the library.
    let reference = tracks[0].analysis_ref.as_deref().unwrap();
    assert_eq!(split_ref(reference).map(|r| r.0), dev.provider.as_deref());
    assert_eq!(
        library::load_ref(&tracks[0]).unwrap().beats.len(),
        a.beats.len()
    );

    // Exporting again refreshes the tracks the device has and copies nothing twice.
    let again = provider
        .export(&stick, &snap, &ExportOptions::default(), &ctl)
        .unwrap();
    assert_eq!((again.added, again.updated), (0, 3));

    // Update Collection: nothing was edited on the player, so nothing changes.
    let store = Store::in_memory().unwrap();
    store.set_device_sigs(&dev.id, &report.baselines).unwrap();
    let u = update::plan(&provider, &stick, &dev.id, &snap, &store, &ctl).unwrap();
    assert_eq!((u.matched, u.rows.len(), u.unmatched), (3, 0, 0));

    // Settings pages come from the plugin; values written to the device read back, with the fingerprint the write answered.
    let schema = provider.schema().unwrap();
    let choices: Vec<(String, String)> = schema
        .tabs
        .iter()
        .flat_map(|t| &t.groups)
        .flat_map(|g| &g.fields)
        .filter_map(|f| match &f.kind {
            FieldKind::Choice {
                choices,
                from_list: None,
                ..
            } => Some((
                f.key.clone(),
                choices.iter().find(|c| c.value != f.default)?.value.clone(),
            )),
            _ => None,
        })
        .collect();
    assert!(!choices.is_empty(), "choice fields");
    let written = provider
        .settings_write(&stick, &choices.iter().cloned().collect())
        .unwrap();
    let read = provider.settings_read(&stick).unwrap();
    assert!(
        choices.iter().any(|(k, v)| read.values.get(k) == Some(v)),
        "no written value reads back: {:?}",
        read.values
    );
    assert_eq!(read.fingerprint, written.fingerprint);

    // Deleting a playlist and a track.
    let cfg = BTreeMap::new();
    let r = provider
        .remove(
            &stick,
            &RemoveTarget::Playlist {
                path: vec!["House".into()],
            },
            false,
            &cfg,
            &ctl,
        )
        .unwrap();
    assert_eq!(r.playlists, 1);
    let gone = tracks.iter().find(|t| t.title == "c").unwrap();
    let file = gone.file.clone().unwrap();
    let r = provider
        .remove(
            &stick,
            &RemoveTarget::Track { file: file.clone() },
            false,
            &cfg,
            &ctl,
        )
        .unwrap();
    assert_eq!(r.tracks, 1);
    assert!(!file.exists());
    let dev = scanned(&stick);
    let lib = device::open_library(&dev).unwrap();
    assert_eq!(titles(&*lib, None), ["a", "b"]);
    assert!(!lib.playlists().unwrap().iter().any(|p| p.name == "House"));

    // Back up the library, lose it, restore it.
    let made = backup::create(&dev, &root.join("backups"), "this computer", &ctl).unwrap();
    assert!(made.files > 0);
    for folder in &dev.info.backup {
        std::fs::remove_dir_all(stick.join(folder)).unwrap();
    }
    assert!(!scanned(&stick).has_library());
    let finder = |name: &str, size: u64| find(&music, name, size);
    let done = backup::restore(&provider, &made.dir, &stick, &finder, false, &ctl).unwrap();
    assert!(matches!(done, backup::Restore::Done { .. }), "{done:?}");
    let lib = device::open_library(&scanned(&stick)).unwrap();
    assert_eq!(titles(&*lib, None), ["a", "b"]);
    assert!(ctl.done.load(Relaxed) <= ctl.total.load(Relaxed));
    backup::delete(&made.dir).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}
