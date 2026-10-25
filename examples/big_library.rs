//! Fills this user's store (`<data dir>/store.db`, so run it with a private HOME) with N synthetic Collection tracks, the way a
//! big library looks to the app at start: `cargo run --release --example big_library -- 50000`. `.github/smoke.sh` uses it for
//! the start-up time check (`SMOKE_TRACKS`).
use openfader::{library::Track, store::Store};

fn main() -> Result<(), String> {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .ok_or("usage: big_library <tracks>")?;
    let tracks: Vec<Track> = (0..n)
        .map(|i| Track {
            title: format!("Track number {i}"),
            artist: format!("Artist {}", i % 700),
            album: format!("Album {}", i % 3000),
            genre: ["House", "Techno", "Trance", "Drum & Bass"][i % 4].into(),
            key: format!("{}{}", i % 12 + 1, ["A", "B"][i % 2]),
            bpm_x100: 9000 + (i % 9000) as i64,
            length_s: 120 + (i % 300) as i64,
            file: Some(format!("/music/disk{}/{i}.mp3", i % 7).into()),
            ..Track::default()
        })
        .collect();
    let put = Store::open()?.collection_put(&tracks, false)?;
    println!("{put} tracks in the store");
    Ok(())
}
