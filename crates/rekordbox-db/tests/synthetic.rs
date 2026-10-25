//! The plugin on a tiny synthetic SQLCipher database (the schema `src/lib.rs` reads), so it runs without the sample pack.

use openfader_plugin::library::{LibraryPlugin, Source};
use rekordbox_db::MasterDb;
use rusqlite::Connection;
use serde_json::json;
use std::{fs, path::PathBuf};

const KEY: &str = "synthetic-key";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rekordbox-db-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// `<dir>/master.db` encrypted with [`KEY`]: tracks A (in playlist 2, history h2), B (smart list), C (deleted).
fn build(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("master.db");
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "key", KEY).unwrap();
    db.execute_batch(
        "create table djmdContent(ID, FolderPath, FileNameL, Title, ArtistID, AlbumID, KeyID, GenreID, BPM, Length, AnalysisDataPath, ImagePath, Rating, Commnt, StockDate, DateCreated, BitRate, SampleRate, rb_local_deleted default 0);
         create table djmdArtist(ID, Name); create table djmdAlbum(ID, Name); create table djmdKey(ID, ScaleName); create table djmdGenre(ID, Name);
         create table djmdPlaylist(ID, Seq, Name, Attribute, ParentID, SmartList, rb_local_deleted default 0);
         create table djmdSongPlaylist(ID, PlaylistID, ContentID, TrackNo, rb_local_deleted default 0);
         create table djmdHistory(ID, Seq, Name, Attribute, ParentID, rb_local_deleted default 0);
         create table djmdSongHistory(ID, HistoryID, ContentID, TrackNo, rb_local_deleted default 0);
         insert into djmdArtist values ('1', 'Alpha'); insert into djmdAlbum values ('1', 'Warm'); insert into djmdKey values ('1', '8A'); insert into djmdGenre values ('1', 'House');
         insert into djmdContent values
          ('a', '/music/a.wav', 'a.wav', 'Alpha Song', '1', '1', '1', '1', 12800, 200, '/PIONEER/USBANLZ/a.DAT', NULL, 4, 'warm', NULL, '2026-01-01', 1411, 44100, 0),
          ('b', '/music/b.wav', 'b.wav', 'Beta', NULL, NULL, NULL, NULL, 14000, 100, NULL, NULL, 9, '', NULL, NULL, 0, 0, 0),
          ('c', '/music/c.wav', 'c.wav', 'Gone', NULL, NULL, NULL, NULL, 12800, 100, NULL, NULL, 0, '', NULL, NULL, 0, 0, 1);
         insert into djmdPlaylist(ID, Seq, Name, Attribute, ParentID) values ('1', 0, 'Folder', 1, 'root'), ('2', 0, 'Plain', 0, '1'), ('3', 1, 'Gone', 0, 'root');
         update djmdPlaylist set rb_local_deleted = 1 where ID = '3';
         insert into djmdPlaylist(ID, Seq, Name, Attribute, ParentID, SmartList) values
          ('s', 2, 'Fast', 4, 'root', '<NODE LogicalOperator=\"1\"><CONDITION PropertyName=\"bpm\" Operator=\"3\" ValueUnit=\"\" ValueLeft=\"13000\" ValueRight=\"\"/></NODE>'),
          ('bad', 3, 'Bad', 4, 'root', '<NODE LogicalOperator=\"1\"><CONDITION PropertyName=\"mixName\" Operator=\"1\" ValueUnit=\"\" ValueLeft=\"x\" ValueRight=\"\"/></NODE>');
         insert into djmdSongPlaylist values ('p1', '2', 'b', 1, 0), ('p2', '2', 'a', 2, 0), ('p3', '2', 'c', 3, 0);
         insert into djmdHistory values ('h1', 0, '2026', 1, 'root', 0), ('h2', 0, 'Friday', 0, 'h1', 0);
         insert into djmdSongHistory values ('s1', 'h2', 'a', 1, 0);",
    )
    .unwrap();
    path
}

fn open(path: &std::path::Path, settings: serde_json::Value) -> Result<Box<dyn Source>, String> {
    MasterDb::open(path, &settings)
}

fn titles(s: &dyn Source, playlist: Option<&str>) -> Vec<String> {
    s.tracks(playlist)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect()
}

#[test]
fn probe_claims_only_encrypted_db_files() {
    let d = dir("probe");
    let enc = build(&d);
    let p = MasterDb::probe(&enc).unwrap();
    assert_eq!((p.score, p.kind.as_str()), (60, "collection database"));
    let renamed = d.join("lib.DB");
    fs::copy(&enc, &renamed).unwrap();
    assert!(MasterDb::probe(&renamed).is_some());
    let other = d.join("master.txt");
    fs::copy(&enc, &other).unwrap();
    assert!(MasterDb::probe(&other).is_none());
    let plain = d.join("plain.db");
    Connection::open(&plain)
        .unwrap()
        .execute_batch("create table t(a)")
        .unwrap();
    assert!(MasterDb::probe(&plain).is_none());
    assert!(MasterDb::probe(&d.join("missing.db")).is_none());
    assert!(MasterDb::probe(&d).is_none());
    let _ = fs::remove_dir_all(d); // cleanup: Windows may still hold a handle
}

#[test]
fn reads_playlists_tracks_smart_lists_and_histories() {
    let d = dir("read");
    let path = build(&d);
    assert!(
        open(&path, json!({}))
            .err()
            .unwrap()
            .contains("database key")
    );
    assert!(open(&path, json!({"key": "wrong"})).is_err());

    let lib = open(&path, json!({"key": KEY, "path_map": "/music=>/mapped"})).unwrap();
    assert_eq!(lib.kind(), "master.db");
    assert_eq!(titles(&*lib, None), ["Alpha Song", "Beta"]); // the deleted row is hidden
    let all = lib.tracks(None).unwrap();
    let a = &all[0];
    assert_eq!(
        (
            a.artist.as_str(),
            a.album.as_str(),
            a.key.as_str(),
            a.genre.as_str(),
            a.bpm_x100,
            a.rating
        ),
        ("Alpha", "Warm", "8A", "House", 12800, 4)
    );
    assert_eq!(
        a.file.as_deref(),
        Some(std::path::Path::new("/mapped/a.wav"))
    );
    assert_eq!(
        a.analysis_ref.as_deref().map(std::path::Path::new), // as paths: Windows mixes `\` and `/` here
        Some(d.join("share/PIONEER/USBANLZ/a.DAT").as_path())
    );
    assert_eq!(all[1].rating, 5); // clamped
    assert!(all[1].analysis_ref.is_none());

    let shape: Vec<_> = lib
        .playlists()
        .unwrap()
        .iter()
        .map(|p| (p.name.clone(), p.depth, p.is_folder))
        .collect();
    let want = [
        ("Folder", 0, true),
        ("Plain", 1, false),
        ("Fast", 0, false),
        ("Bad", 0, false),
        ("Histories", 0, true),
        ("2026", 1, true),
        ("Friday", 2, false),
    ];
    assert_eq!(shape, want.map(|(n, d, f)| (n.to_owned(), d, f)));
    assert_eq!(titles(&*lib, Some("2")), ["Beta", "Alpha Song"]); // playlist order, deleted entry hidden
    assert_eq!(titles(&*lib, Some("s")), ["Beta"]); // smart list: bpm > 130.00
    assert!(lib.tracks(Some("bad")).unwrap_err().contains("mixName"));
    assert_eq!(titles(&*lib, Some("history:h2")), ["Alpha Song"]);
    assert!(titles(&*lib, Some("history:h1")).is_empty()); // a folder lists no tracks
    drop(lib); // Windows cannot delete a database that is still open
    let _ = fs::remove_dir_all(d); // cleanup: Windows may still hold a handle
}
