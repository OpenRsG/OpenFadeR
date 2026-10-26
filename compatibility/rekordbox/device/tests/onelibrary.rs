//! Cross-checks the two library formats in one real device: OneLibrary (`exportLibrary.db`)
//! against Device Library (`export.pdb`). Skips without `OPENFADER_USB` or `.local/onelibrary-key`.

mod common;

use openfader_plugin::library::{LibraryPlugin, Playlist, Track};
use compat_rekordbox_common::list::HISTORY;
use compat_rekordbox_device::Device;
use serde_json::json;

#[test]
fn onelibrary_matches_export_pdb() {
    let (Some(root), Some(key)) = (common::usb_root(), common::onelibrary_key()) else {
        return;
    };
    assert!(Device::open(&root, &json!({ "onelibrary_key": "wrong" })).is_err());
    let one = common::open(&root, Some(&key));
    let pdb = common::open(&root, None);
    assert_eq!(
        (one.kind().as_str(), pdb.kind().as_str()),
        ("USB (OneLibrary)", "USB (Device Library)")
    );
    let sorted = |mut t: Vec<Track>| {
        t.sort_by(|a, b| a.file.cmp(&b.file));
        t
    };
    let (a, b) = (
        sorted(one.tracks(None).unwrap()),
        sorted(pdb.tracks(None).unwrap()),
    );
    // Every field of `Track` (incl. key/genre joins, artwork path, rating, comment, date added,
    // bitrate, sample rate) must agree; the fields compared must not be vacuously empty.
    assert!(a.iter().any(|t| !t.key.is_empty()), "no track has a key");
    assert!(
        a.iter().any(|t| !t.comment.is_empty()),
        "no track has a comment"
    );
    assert!(
        a.iter().any(|t| !t.album.is_empty()),
        "no track has an album"
    );
    assert!(
        a.iter().any(|t| !t.date_added.is_empty()),
        "no track has a date added"
    );
    assert!(a.iter().all(|t| t.bitrate > 0 && t.sample_rate > 0));
    assert_eq!(a.len(), b.len());
    // Observed: export.pdb names artwork `/PIONEER/Artwork/<dir>/a<N>.jpg`, OneLibrary `…/b<N>.jpg`
    // (byte-identical 80x80 JPEGs, each with a `_m` 240x240 sibling), so compare dir + `<N>.jpg`.
    let art = |t: &Track| {
        t.artwork.as_ref().map(|p| {
            (
                p.parent().unwrap().to_owned(),
                p.file_name().unwrap().to_string_lossy()[1..].to_owned(),
            )
        })
    };
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(art(x), art(y));
        let (mut x, mut y) = (x.clone(), y.clone());
        (x.artwork, y.artwork) = (None, None);
        assert_eq!(x, y);
    }
    for (name, n) in [
        ("artwork", a.iter().filter(|t| t.artwork.is_some()).count()),
        ("rating > 0", a.iter().filter(|t| t.rating > 0).count()),
    ] {
        eprintln!(
            "{name}: {n} of {} tracks{}",
            a.len(),
            if n == 0 {
                " — NOTE: parity of this field is vacuous on this USB"
            } else {
                ""
            }
        );
    }
    let (lists_one, lists_pdb) = (one.playlists().unwrap(), pdb.playlists().unwrap());
    let shape = |l: &[Playlist]| {
        l.iter()
            .map(|p| (p.name.clone(), p.depth, p.is_folder))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&lists_one), shape(&lists_pdb));
    let files = |t: Vec<Track>| t.into_iter().map(|t| t.file).collect::<Vec<_>>();
    for (a, b) in lists_one.iter().zip(&lists_pdb) {
        let (x, y) = (
            one.tracks(Some(&a.id)).unwrap(),
            pdb.tracks(Some(&b.id)).unwrap(),
        );
        assert_eq!(files(x), files(y), "order of {}", a.name);
    }
    // Histories are part of both trees (under "Histories") and of the loop above; say so when there are none.
    let histories = lists_one
        .iter()
        .filter(|p| p.id.starts_with(HISTORY) && !p.is_folder)
        .count();
    eprintln!(
        "histories: {histories} playlists{}",
        if histories == 0 {
            " — NOTE: parity of histories is vacuous on this USB (no history rows in either format)"
        } else {
            ""
        }
    );
}
