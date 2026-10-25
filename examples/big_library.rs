//! Fills this user's store (`<data dir>/store.db`, so run it with a private HOME) with N synthetic Collection tracks and what a
//! used library carries around them: cues and edits on every third track, 200 playlists (20k entries at 50k tracks, scaled
//! down for less), play history, MyTags, gain and print rows: `cargo run --release --example big_library -- 50000`.
//! `.github/smoke.sh` uses it for the start-up time check (`SMOKE_TRACKS`).
use openfader::{library::Track, store::Store};
use rusqlite::{params, Connection};

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
    let store = Store::open()?;
    let put = store.collection_put(&tracks, false)?;
    println!("{put} tracks in the store");
    let path = openfader::paths::data_dir()
        .ok_or("no data dir")?
        .join("store.db");
    drop(store);
    let mut db = Connection::open(path).map_err(|e| e.to_string())?;
    let e = |e: rusqlite::Error| e.to_string();
    let tx = db.transaction().map_err(e)?;
    let ids: Vec<i64> = tx
        .prepare("SELECT id FROM collection ORDER BY id")
        .map_err(e)?
        .query_map([], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    // Size and mtime as an import of an existing file records them (the files themselves are not made: the worker that marks
    // missing files finds them gone after the window shows).
    tx.execute("UPDATE collection SET size = 5000000 + id, mtime = 1700000000 + id", [])
        .map_err(e)?;
    // Edits (cues, rating, colour, comment) on every third track, 5 cues each; a grid on every ninth.
    {
        let mut track = tx
            .prepare("INSERT INTO track(key, rating, comment, color, cues_set, grid, grid_orig) VALUES (?1,?2,?3,?4,1,?5,?5)")
            .map_err(e)?;
        let mut cue = tx
            .prepare("INSERT INTO cue(key, pos, hot, time_ms, loop_end_ms, color, comment) VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .map_err(e)?;
        for (i, id) in ids.iter().enumerate().filter(|(i, _)| i % 3 == 0) {
            let key = format!("t{id}");
            let grid: Option<Vec<u8>> = (i % 9 == 0).then(|| {
                (0..400u32)
                    .flat_map(|b| {
                        let mut v = (b * 480).to_le_bytes().to_vec();
                        v.extend(12_800u16.to_le_bytes());
                        v.extend((b as u16 % 4 + 1).to_le_bytes());
                        v
                    })
                    .collect()
            });
            track
                .execute(params![
                    key,
                    (i % 6) as i64,
                    (i % 5 == 0).then(|| format!("comment {i}")),
                    (i % 9) as i64,
                    grid
                ])
                .map_err(e)?;
            for pos in 0..5i64 {
                cue.execute(params![
                    key,
                    pos,
                    if pos < 4 { pos + 1 } else { 0 },
                    1000 * (pos + 1) + (i as i64 % 700),
                    (pos == 4).then_some(9000 + pos),
                    0x00e000,
                    (pos % 2 == 0).then(|| "drop")
                ])
                .map_err(e)?;
            }
        }
    }
    // 200 playlists in 10 folders, 20k entries at 50k tracks (400 entries at the most per playlist).
    let (lists, entries) = (200usize, (n * 2 / 5).min(20_000));
    {
        let mut node = tx
            .prepare("INSERT INTO node(parent, name, kind, pos) VALUES (?1,?2,?3,?4)")
            .map_err(e)?;
        let mut entry = tx
            .prepare("INSERT INTO entry(node, pos, key) VALUES (?1,?2,?3)")
            .map_err(e)?;
        let per = entries / lists.max(1);
        for f in 0..10i64 {
            node.execute(params![None::<i64>, format!("Folder {f}"), 0, f])
                .map_err(e)?;
            let folder = tx.last_insert_rowid();
            for p in 0..(lists / 10) as i64 {
                node.execute(params![folder, format!("Playlist {f}-{p}"), 1, p])
                    .map_err(e)?;
                let list = tx.last_insert_rowid();
                for k in 0..per {
                    let id = ids[(f as usize * 7919 + p as usize * 131 + k * 17) % ids.len()];
                    entry
                        .execute(params![list, k as i64, format!("t{id}")])
                        .map_err(e)?;
                }
            }
        }
    }
    // Play history: 200 days of 25 plays.
    {
        let mut h = tx
            .prepare("INSERT INTO history(day, key, played_at) VALUES (?1,?2,?3)")
            .map_err(e)?;
        for d in 0..200usize {
            for k in 0..25usize {
                let id = ids[(d * 31 + k * 7) % ids.len()];
                h.execute(params![
                    format!("2026-{:02}-{:02}", d / 28 % 12 + 1, d % 28 + 1),
                    format!("t{id}"),
                    (d * 86_400 + k * 240) as i64
                ])
                .map_err(e)?;
            }
        }
    }
    // MyTags: 4 groups of 8 tags, three tags on every fourth track.
    {
        for c in 0..4i64 {
            tx.execute(
                "INSERT INTO mytag_category(id, name, pos) VALUES (?1, ?2, ?1)",
                params![c + 1, format!("Group {c}")],
            )
            .map_err(e)?;
            for t in 0..8i64 {
                tx.execute(
                    "INSERT INTO mytag(id, category, name, pos) VALUES (?1, ?2, ?3, ?4)",
                    params![c * 8 + t + 1, c + 1, format!("Tag {c}-{t}"), t],
                )
                .map_err(e)?;
            }
        }
        let mut tt = tx
            .prepare("INSERT OR IGNORE INTO track_mytag(key, tag) VALUES (?1, ?2)")
            .map_err(e)?;
        for (i, id) in ids.iter().enumerate().filter(|(i, _)| i % 4 == 0) {
            for j in 0..3 {
                tt.execute(params![format!("t{id}"), ((i + j * 5) % 32 + 1) as i64])
                    .map_err(e)?;
            }
        }
    }
    // Analysis rows: AUTO GAIN on every track, a content print on every track.
    {
        let mut g = tx
            .prepare("INSERT INTO gain(key, analysed_db, adjust_db) VALUES (?1,?2,0)")
            .map_err(e)?;
        let mut p = tx
            .prepare("INSERT INTO file_print(track, print, size, mtime) VALUES (?1,?2,?3,?4)")
            .map_err(e)?;
        for (i, &id) in ids.iter().enumerate() {
            g.execute(params![format!("t{id}"), -((i % 90) as f64) / 10.0])
                .map_err(e)?;
            p.execute(params![
                id,
                format!("{:016x}", (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                5_000_000 + id,
                1_700_000_000 + id
            ])
            .map_err(e)?;
        }
    }
    tx.commit().map_err(e)?;
    println!(
        "{lists} playlists, {entries} entries, cues on {} tracks, history, tags, gain",
        ids.len().div_ceil(3)
    );
    Ok(())
}
