//! A transfer onto a device with the built-in device plugin (rekordbox-device) on a fake
//! device root: tracks written to a device playlist, tracks added to it later (the members keep their cues, the new tracks
//! arrive whole, a track already there is skipped) and tracks sent to the device itself (no playlist is made). Each step is read
//! back through the plugin.

mod common;

use openfader::analysis::{Analysis, Cue};
use openfader::device::{self, export::export, transfer};
use openfader::library::{self, snapshot::Snapshot, Library, Playlist, Track};
use openfader::plugin::device::{Control, ExportOptions};
use openfader::store;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

fn scanned(stick: &Path) -> device::Device {
    device::forget(stick);
    device::scan()
        .into_iter()
        .find(|d| d.mount == stick)
        .expect("the fake device is listed")
}

fn cue(hot_cue: u16, time_ms: u32) -> Cue {
    Cue {
        hot_cue,
        time_ms,
        loop_end_ms: None,
        color: None,
        comment: None,
    }
}

/// A track of the collection with a user's cue, a grid and a key.
fn whole(t: &Track, cues: Vec<Cue>) -> (Track, Option<Analysis>) {
    let a = Analysis {
        cues,
        beats: common::grid(32),
        key: Some("8A".into()),
        ..Analysis::default()
    };
    (t.clone(), Some(a))
}

fn cues_of(lib: &dyn Library, t: &Track) -> Vec<(u16, u32)> {
    let mut v: Vec<_> = lib
        .analysis(t)
        .expect("the device holds an analysis")
        .cues
        .iter()
        .map(|c| (c.hot_cue, c.time_ms))
        .collect();
    v.sort_unstable();
    v
}

fn titled<'a>(tracks: &'a [Track], title: &str) -> &'a Track {
    tracks.iter().find(|t| t.title == title).unwrap()
}

fn titles(lib: &dyn Library, playlist: Option<&str>) -> Vec<String> {
    lib.tracks(playlist)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect()
}

fn playlist<'a>(pls: &'a [Playlist], name: &str) -> &'a Playlist {
    pls.iter().find(|p| p.name == name).unwrap()
}

#[test]
fn a_transfer_keeps_what_the_device_holds_and_writes_the_new_tracks_whole() {
    common::device_plugin();
    let root = common::fresh_dir("transfer");
    let (music, stick) = (root.join("music"), root.join("stick"));
    std::fs::create_dir_all(&music).unwrap();
    std::fs::create_dir_all(&stick).unwrap();
    for (file, secs) in [("a", 4.0), ("b", 5.0), ("c", 6.0), ("d", 7.0), ("e", 8.0)] {
        common::click_wav(&music.join(format!("{file}.wav")), 120.0, secs);
    }
    // SAFETY: set before the first scan; the only test of this binary.
    unsafe { std::env::set_var("OPENFADER_DEVICE_ROOTS", &stick) };
    let dev = scanned(&stick);
    let writers = [dev.provider().expect("a plugin handles the device")];
    let ctl = Arc::new(Control::default());
    let o = ExportOptions::default();
    let collection = library::open(&music).unwrap();
    let tracks = collection.tracks(None).unwrap();
    let folders = ["Sets".to_owned()];

    // 1. a and b go to the new device playlist Sets / Mix with cues of their own.
    let first = transfer::append(
        vec![
            whole(titled(&tracks, "a"), vec![cue(1, 1000), cue(0, 2000)]),
            whole(titled(&tracks, "b"), vec![cue(2, 1500)]),
        ],
        Vec::new(),
        &folders,
        "Mix",
    );
    assert_eq!((first.added, first.skipped), (2, 0));
    export(&writers, &stick, &first.snapshot, &o, &ctl).unwrap();
    let lib = device::open_library(&scanned(&stick)).unwrap();
    let pls = lib.playlists().unwrap();
    let mix = playlist(&pls, "Mix");
    assert_eq!(titles(&*lib, Some(&mix.id)), ["a", "b"]);
    assert_eq!((pls.len(), mix.depth), (2, 1), "Sets holds Mix");
    let on_device = lib.tracks(None).unwrap();
    assert_eq!(
        cues_of(&*lib, titled(&on_device, "a")),
        [(0, 2000), (1, 1000)]
    );

    // 2. c, d and a again go to Mix: the members come first with their cues (the collection has no analysis of a and b: the
    // device's is kept), a is skipped, c arrives whole, d is analysed by the plugin.
    let own: HashMap<String, &Track> = tracks.iter().map(|t| (store::key(t), t)).collect();
    let current = transfer::members(&*lib, &mix.id, &own, &|_| None).unwrap();
    assert_eq!(current.len(), 2);
    let second = transfer::append(
        vec![
            whole(titled(&tracks, "c"), vec![cue(1, 3000)]),
            (titled(&tracks, "d").clone(), None),
            whole(titled(&tracks, "a"), vec![cue(5, 9999)]),
        ],
        current,
        &folders,
        "Mix",
    );
    assert_eq!((second.added, second.skipped), (2, 1));
    export(&writers, &stick, &second.snapshot, &o, &ctl).unwrap();
    let lib = device::open_library(&scanned(&stick)).unwrap();
    let pls = lib.playlists().unwrap();
    let mix = playlist(&pls, "Mix");
    assert_eq!(titles(&*lib, Some(&mix.id)), ["a", "b", "c", "d"]);
    assert_eq!(pls.len(), 2, "no playlist was added");
    let on_device = lib.tracks(None).unwrap();
    assert_eq!(on_device.len(), 4, "no track is on the device twice");
    assert_eq!(
        cues_of(&*lib, titled(&on_device, "a")),
        [(0, 2000), (1, 1000)],
        "the skipped track keeps its cues"
    );
    assert_eq!(cues_of(&*lib, titled(&on_device, "b")), [(2, 1500)]);
    assert_eq!(cues_of(&*lib, titled(&on_device, "c")), [(1, 3000)]);
    for t in ["c", "d"] {
        let t = titled(&on_device, t);
        let a = lib.analysis(t).unwrap();
        assert!(a.beats.len() > 4, "{}: {} beats", t.title, a.beats.len());
        assert!(!t.key.is_empty(), "{}: no key", t.title);
    }

    // 3. e goes to the device itself: the library gets the track and no playlist.
    let root_only = Snapshot::of_tracks(vec![whole(titled(&tracks, "e"), vec![cue(3, 4000)])]);
    export(&writers, &stick, &root_only, &o, &ctl).unwrap();
    let lib = device::open_library(&scanned(&stick)).unwrap();
    let pls = lib.playlists().unwrap();
    assert_eq!(pls.len(), 2, "still Sets and Mix");
    assert_eq!(
        titles(&*lib, Some(&playlist(&pls, "Mix").id)),
        ["a", "b", "c", "d"]
    );
    let on_device = lib.tracks(None).unwrap();
    assert_eq!(on_device.len(), 5);
    assert_eq!(cues_of(&*lib, titled(&on_device, "e")), [(3, 4000)]);

    // 4. b's device copy is edited as a track only the device has (Make Auto Cue point on the stick's list): written back
    // from the device's own analysis with the new cues, it keeps its place, its playlist and its grid.
    let b = titled(&on_device, "b").clone();
    let mut a = library::load_ref(&b).expect("the device's analysis of b");
    let beats = a.beats.len();
    a.cues = vec![cue(0, 120), cue(1, 120), cue(2, 2120)];
    export(
        &writers,
        &stick,
        &Snapshot::of_tracks(vec![(b, Some(a))]),
        &o,
        &ctl,
    )
    .unwrap();
    let lib = device::open_library(&scanned(&stick)).unwrap();
    let on_device = lib.tracks(None).unwrap();
    assert_eq!(on_device.len(), 5, "b is not copied a second time");
    assert_eq!(
        titles(&*lib, Some(&playlist(&lib.playlists().unwrap(), "Mix").id)),
        ["a", "b", "c", "d"]
    );
    let b = titled(&on_device, "b");
    assert_eq!(cues_of(&*lib, b), [(0, 120), (1, 120), (2, 2120)]);
    assert_eq!(
        lib.analysis(b).unwrap().beats.len(),
        beats,
        "the grid stays"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
