//! Speed of listing the Collection through the store overlay. The bench is `#[ignore]`d (100k rows take seconds to build):
//! `cargo test --release --test library_speed -- --ignored --nocapture`.
use openfader::{
    library::{own::Own, Library, Track},
    store::{self, Overlay, Store},
};
use std::{sync::Arc, time::Instant};

fn track(i: usize) -> Track {
    Track {
        title: format!("Track number {i}"),
        artist: format!("Artist {}", i % 700),
        album: format!("Album {}", i % 3000),
        genre: ["House", "Techno", "Trance", "Drum & Bass"][i % 4].into(),
        key: format!("{}{}", i % 12 + 1, ["A", "B"][i % 2]),
        bpm_x100: 9000 + (i % 9000) as i64,
        length_s: 120 + (i % 300) as i64,
        file: Some(format!("/music/disk{}/{i}.mp3", i % 7).into()),
        ..Track::default()
    }
}

fn collection(n: usize) -> Arc<Store> {
    let store = Arc::new(Store::in_memory().unwrap());
    let all: Vec<Track> = (0..n).map(track).collect();
    assert_eq!(store.collection_put(&all, false).unwrap(), n);
    store
}

fn overlay(store: &Arc<Store>) -> Overlay {
    Overlay::new(Box::new(Own::new(store.clone())), store.clone())
}

fn time<T>(f: impl FnOnce() -> T) -> (T, std::time::Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

/// An edit made after a listing is in the next one: the overlay's cached ratings follow the store's generation.
#[test]
fn a_rating_edit_shows_in_the_next_listing() {
    let store = collection(3);
    let lib = overlay(&store);
    let first = lib.tracks(None).unwrap();
    assert!(first.iter().all(|t| t.rating == 0));
    store.set_rating(&store::key(&first[1]), Some(4)).unwrap();
    store
        .set_comment(&store::key(&first[2]), Some("peak"))
        .unwrap();
    let again = lib.tracks(None).unwrap();
    assert_eq!(
        again.iter().map(|t| t.rating).collect::<Vec<_>>(),
        [0, 4, 0],
        "the rating is visible at once"
    );
    assert_eq!(again[2].comment, "peak");
    store.set_rating(&store::key(&first[1]), None).unwrap();
    assert_eq!(
        lib.tracks(None).unwrap()[1].rating,
        0,
        "and so is clearing it"
    );
}

#[test]
#[ignore = "bench: 100k tracks"]
fn bench_listing_100k_collection_tracks() {
    const N: usize = 100_000;
    let (store, build) = time(|| collection(N));
    eprintln!("build {N} rows: {build:?}");
    // A tenth of the tracks carry a rating or comment: the overlay has real work to do.
    let first = Own::new(store.clone()).tracks(None).unwrap();
    for t in first.iter().step_by(10) {
        store.set_rating(&store::key(t), Some(3)).unwrap();
    }
    let own = Own::new(store.clone());
    let lib = overlay(&store);

    let (_, own_cold) = time(|| own.tracks(None).unwrap());
    let (_, own_warm) = time(|| own.tracks(None).unwrap());
    let (rows, cold) = time(|| lib.tracks(None).unwrap());
    let (_, warm) = time(|| lib.tracks(None).unwrap());
    let (_, warm2) = time(|| lib.tracks(None).unwrap());
    let (_, shared_cold) = time(|| lib.tracks_all().unwrap());
    let (shared, shared_warm) = time(|| lib.tracks_all().unwrap());
    eprintln!("Own::tracks(None)    cold {own_cold:?}, warm {own_warm:?}");
    eprintln!("Overlay::tracks(None) cold {cold:?}, warm {warm:?}, again {warm2:?}");
    eprintln!("Overlay::tracks_all   cold {shared_cold:?}, warm {shared_warm:?}");
    assert_eq!((rows.len(), shared.len()), (N, N));
    assert_eq!(rows.iter().filter(|t| t.rating == 3).count(), N / 10);
    assert_eq!(shared.iter().filter(|t| t.rating == 3).count(), N / 10);

    // One edit invalidates both caches: the next listing is cold again, then warm.
    store.set_rating(&store::key(&rows[1]), Some(5)).unwrap();
    let (after, edit_cold) = time(|| lib.tracks(None).unwrap());
    let (_, edit_warm) = time(|| lib.tracks(None).unwrap());
    let (_, edit_shared) = time(|| lib.tracks_all().unwrap());
    let (_, edit_shared_warm) = time(|| lib.tracks_all().unwrap());
    eprintln!("after one rating edit: cold {edit_cold:?}, warm {edit_warm:?}; tracks_all {edit_shared:?}, warm {edit_shared_warm:?}");
    assert_eq!(after[1].rating, 5);

    assert!(
        shared_warm.as_millis() < 20,
        "a warm shared listing of {N} tracks should take < 20 ms, took {shared_warm:?}"
    );
}
