//! The persistent store: edits survive a reopen, and the overlay lets the store win over the library.

use openfader::{
    analysis::{Analysis, Beat, Cue},
    library::{Library, Playlist, Track},
    store::{self, NodeKind, Overlay, Store},
};
use std::{path::PathBuf, sync::Arc};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("openfader-store-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cue(hot: u16, ms: u32) -> Cue {
    Cue {
        hot_cue: hot,
        time_ms: ms,
        loop_end_ms: None,
        color: None,
        comment: None,
    }
}

fn beat(bar_pos: u16, time_ms: u32) -> Beat {
    Beat {
        bar_pos,
        bpm_x100: 12800,
        time_ms,
    }
}

#[test]
fn edits_round_trip_through_a_reopen() {
    let dir = temp("roundtrip");
    let path = dir.join("store.db");
    let cues = vec![
        Cue {
            hot_cue: 1,
            time_ms: 1500,
            loop_end_ms: None,
            color: Some([0xff, 0x00, 0x17]),
            comment: Some("drop".into()),
        },
        Cue {
            hot_cue: 0,
            time_ms: 4000,
            loop_end_ms: Some(6000),
            color: None,
            comment: None,
        },
    ];
    let (grid, orig) = (
        vec![beat(1, 100), beat(2, 569)],
        vec![beat(1, 0), beat(2, 469)],
    );
    {
        let s = Store::open_at(&path).unwrap();
        assert_eq!(s.edits("k").unwrap(), Default::default());
        s.set_rating("k", Some(4)).unwrap();
        s.set_comment("k", Some("old comment")).unwrap();
        s.set_metadata("k", "Título", "Artist", "", "House", "Label", "warm-up")
            .unwrap();
        s.set_color("k", Some(7)).unwrap();
        s.set_cues("k", Some(&cues)).unwrap();
        s.set_grid("k", &grid, &orig).unwrap();
        s.set_grid("k", &grid, &grid).unwrap(); // the second edit must not replace the original
        s.set_color("other", Some(9)).unwrap(); // not a colour id: ignored
    }
    let s = Store::open_at(&path).unwrap();
    let e = s.edits("k").unwrap();
    assert_eq!(
        (e.rating, e.comment.as_deref(), e.color),
        (Some(4), Some("warm-up"), Some(7))
    );
    assert_eq!(
        (
            e.title.as_deref(),
            e.artist.as_deref(),
            e.album.as_deref(),
            e.genre.as_deref(),
            e.label.as_deref()
        ),
        (
            Some("Título"),
            Some("Artist"),
            Some(""),
            Some("House"),
            Some("Label")
        )
    );
    assert_eq!(e.cues.as_deref(), Some(&cues[..]));
    assert_eq!(e.grid.as_deref(), Some(&grid[..]));
    assert_eq!(s.track_color("other").unwrap(), None);
    assert_eq!(s.reset_grid("k").unwrap(), Some(orig));
    assert_eq!(s.edits("k").unwrap().grid, None);
    assert_eq!(s.reset_grid("k").unwrap(), None);
    s.set_cues("k", None).unwrap();
    assert_eq!(s.edits("k").unwrap().cues, None);
    s.set_cues("k", Some(&[])).unwrap(); // every cue deleted is an edit, not "no edit"
    assert_eq!(s.edits("k").unwrap().cues, Some(vec![]));
    drop(s); // Windows cannot delete a database that is still open
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn playlists_mytags_and_history() {
    let s = Store::in_memory().unwrap();
    let folder = s.add_node(None, "Sets", NodeKind::Folder).unwrap();
    let a = s.add_node(Some(folder), "A", NodeKind::Playlist).unwrap();
    let b = s.add_node(Some(folder), "B", NodeKind::Smart).unwrap();
    let top = s.add_node(None, "Top", NodeKind::Playlist).unwrap();
    let names = |s: &Store| {
        s.nodes()
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&s), ["Sets", "A", "B", "Top"]);
    s.move_node(b, Some(folder), 0).unwrap();
    s.move_node(top, None, 0).unwrap();
    assert_eq!(names(&s), ["Top", "Sets", "B", "A"]);
    assert!(s.move_node(folder, Some(a), 0).is_err());
    s.set_smart(b, "bpm>120").unwrap();
    assert_eq!(s.smart(b).unwrap().as_deref(), Some("bpm>120"));

    let keys = |v: &[&str]| v.iter().map(|k| k.to_string()).collect::<Vec<_>>();
    s.add_entries(a, &keys(&["x|1", "y|2"])).unwrap();
    s.add_entries(a, &keys(&["z|3"])).unwrap();
    s.move_entry(a, 2, 0).unwrap();
    assert_eq!(s.entries(a).unwrap(), keys(&["z|3", "x|1", "y|2"]));
    s.remove_entry(a, 1).unwrap();
    assert_eq!(s.entries(a).unwrap(), keys(&["z|3", "y|2"]));
    s.rename_node(a, "A2").unwrap();
    s.delete_node(folder).unwrap(); // cascades to A, B and A's entries
    assert_eq!(names(&s), ["Top"]);
    assert!(s.entries(a).unwrap().is_empty());

    let cat = s.add_mytag_category("Genre").unwrap();
    let t1 = s.add_mytag(cat, "House").unwrap();
    let t2 = s.add_mytag(cat, "Techno").unwrap();
    s.set_track_mytag("x|1", t1, true).unwrap();
    s.set_track_mytag("x|1", t2, true).unwrap();
    s.set_track_mytag("x|1", t2, true).unwrap();
    s.set_track_mytag("y|2", t2, true).unwrap();
    s.set_track_mytag("x|1", t1, false).unwrap();
    assert_eq!(s.track_mytags("x|1").unwrap(), [t2]);
    assert_eq!(s.all_track_mytags().unwrap().len(), 2);
    s.delete_mytag_category(cat).unwrap();
    assert!(s.mytags().unwrap().is_empty() && s.all_track_mytags().unwrap().is_empty());

    s.history_append("2026-10-12", "x|1", 10).unwrap();
    s.history_append("2026-10-13", "y|2", 30).unwrap();
    s.history_append("2026-10-13", "x|1", 20).unwrap();
    assert_eq!(s.history_days().unwrap(), ["2026-10-13", "2026-10-12"]);
    assert_eq!(s.history_day("2026-10-13").unwrap(), keys(&["x|1", "y|2"]));
}

/// A library of two tracks whose own rating / comment / cues / grid the store may override.
struct Lib(Arc<Vec<Track>>);

impl Library for Lib {
    fn kind(&self) -> &str {
        "test"
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(vec![])
    }
    fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
        Ok((*self.0).clone())
    }
    fn tracks_all(&self) -> Result<Arc<Vec<Track>>, String> {
        Ok(self.0.clone())
    }
    fn analysis(&self, _: &Track) -> Option<Analysis> {
        Some(Analysis {
            beats: vec![beat(1, 0), beat(2, 469)],
            cues: vec![cue(1, 1000), cue(2, 2000)],
            ..Default::default()
        })
    }
}

#[test]
fn the_store_wins_over_the_library_but_only_where_it_has_an_edit() {
    let dir = temp("overlay");
    let files: Vec<PathBuf> = ["a.mp3", "b.mp3"].iter().map(|n| dir.join(n)).collect();
    for f in &files {
        std::fs::write(f, b"audio").unwrap();
    }
    let track = |f: &PathBuf| Track {
        title: "t".into(),
        artist: "source artist".into(),
        album: "source album".into(),
        genre: "source genre".into(),
        label: "source label".into(),
        file: Some(f.clone()),
        rating: 1,
        comment: "library".into(),
        ..Default::default()
    };
    let tracks: Vec<Track> = files.iter().map(track).collect();
    let store = Arc::new(Store::in_memory().unwrap());
    let lib = Overlay::new(Box::new(Lib(Arc::new(tracks.clone()))), store.clone());

    // Untouched store: the library as it is.
    assert_eq!(lib.tracks(None).unwrap(), tracks);
    assert_eq!(lib.tracks_all().unwrap().as_ref(), &tracks);
    assert_eq!(lib.analysis(&tracks[0]).unwrap().cues.len(), 2);

    let k = store::key(&tracks[0]);
    store.set_rating(&k, Some(5)).unwrap();
    store.set_comment(&k, Some("mine")).unwrap();
    store
        .set_metadata(&k, "edited", "artist", "", "Techno", "label", "mine")
        .unwrap();
    store.set_cues(&k, Some(&[cue(3, 3000)])).unwrap();
    store.set_grid(&k, &[beat(1, 50)], &[]).unwrap();

    let got = lib.tracks(None).unwrap();
    assert_eq!((got[0].rating, got[0].comment.as_str()), (5, "mine"));
    assert_eq!(got[1], tracks[1]); // another track: untouched
    assert_eq!(
        (
            &*got[0].title,
            &*got[0].artist,
            &*got[0].album,
            &*got[0].genre,
            &*got[0].label
        ),
        ("edited", "artist", "", "Techno", "label")
    );
    assert_eq!(store::key(&got[0]), k);
    assert_eq!(lib.tracks_all().unwrap().as_ref(), &got);
    let a = lib.analysis(&tracks[0]).unwrap();
    assert_eq!((a.cues, a.beats), (vec![cue(3, 3000)], vec![beat(1, 50)]));
    assert_eq!(lib.analysis(&tracks[1]).unwrap().cues.len(), 2);
    assert_eq!(lib.analysis(&got[0]).unwrap().cues, vec![cue(3, 3000)]);

    // A metadata-only edit must refresh the cached listing, including explicitly cleared text.
    let k2 = store::key(&tracks[1]);
    let cached = lib.tracks_all().unwrap();
    store
        .set_metadata(&k2, "second", "", "", "", "", "")
        .unwrap();
    let refreshed = lib.tracks_all().unwrap();
    assert_eq!(refreshed[1].title, "second");
    assert!(refreshed[1].artist.is_empty() && refreshed[1].comment.is_empty());
    assert_eq!(cached[1], tracks[1]);
    assert_eq!(refreshed[0], got[0]);

    // A rating-only edit keeps the library's cues and grid; an emptied cue list stays empty.
    let k2 = store::key(&tracks[1]);
    store.set_rating(&k2, Some(0)).unwrap();
    assert_eq!(lib.tracks(None).unwrap()[1].rating, 0);
    assert_eq!(lib.analysis(&tracks[1]).unwrap().cues.len(), 2);
    store.set_cues(&k2, Some(&[])).unwrap();
    let a = lib.analysis(&tracks[1]).unwrap();
    assert!(a.cues.is_empty() && a.beats.len() == 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn metadata_is_one_undo_step_and_keeps_other_edits_and_identity() {
    let s = Store::in_memory().unwrap();
    let original = Track {
        id: Some(7),
        title: "source title".into(),
        artist: "source artist".into(),
        ..Default::default()
    };
    let key = store::key(&original);
    s.set_rating(&key, Some(4)).unwrap();
    s.set_comment(&key, Some("keep")).unwrap();
    s.set_color(&key, Some(2)).unwrap();
    s.set_cues(&key, Some(&[cue(1, 500)])).unwrap();
    let before = s.edits(&key).unwrap();
    let seq = s.journal_seq().unwrap();
    s.set_metadata(
        &key,
        "first",
        "artist",
        "album",
        "genre",
        "label",
        "first comment",
    )
    .unwrap();
    let first = s.edits(&key).unwrap();
    s.set_metadata(&key, "second", "", "", "", "", "").unwrap();
    let second = s.edits(&key).unwrap();
    let mut shown = original.clone();
    second.apply_metadata(&mut shown);
    assert_eq!(store::key(&shown), key);
    assert_eq!(
        (shown.title.as_str(), shown.artist.as_str()),
        ("second", "")
    );
    assert_eq!(s.journal_touched_since(seq).unwrap(), [7]);
    s.undo().unwrap();
    assert_eq!(s.edits(&key).unwrap(), first);
    s.undo().unwrap();
    assert_eq!(s.edits(&key).unwrap(), before);
    s.redo().unwrap();
    s.redo().unwrap();
    assert_eq!(s.edits(&key).unwrap(), second);

    s.set_metadata("t8", "new", "", "", "", "", "").unwrap();
    s.undo().unwrap();
    assert_eq!(s.edits("t8").unwrap(), Default::default());
    s.redo().unwrap();
    assert_eq!(s.edits("t8").unwrap().title.as_deref(), Some("new"));
}

#[test]
fn metadata_upgrade_preserves_old_edits_and_failed_updates_are_atomic() {
    let dir = temp("metadata-migration");
    let path = dir.join("store.db");
    {
        let s = Store::open_at(&path).unwrap();
        s.set_rating("t7", Some(3)).unwrap();
    }
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        // Restore the pre-override track schema while retaining its data and journal.
        db.execute_batch(
            "ALTER TABLE track DROP COLUMN title;
             ALTER TABLE track DROP COLUMN artist;
             ALTER TABLE track DROP COLUMN album;
             ALTER TABLE track DROP COLUMN genre;
             ALTER TABLE track DROP COLUMN label;
             PRAGMA user_version = 15;",
        )
        .unwrap();
    }
    let s = Store::open_at(&path).unwrap();
    assert_eq!(s.edits("t7").unwrap().rating, Some(3));
    assert_eq!(s.edits("t7").unwrap().title, None);
    s.undo().unwrap();
    assert_eq!(s.edits("t7").unwrap(), Default::default());
    s.redo().unwrap();
    s.set_metadata(
        "t7", "title", "artist", "album", "genre", "label", "comment",
    )
    .unwrap();
    let before = s.edits("t7").unwrap();
    let seq = s.journal_seq().unwrap();
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER reject_metadata BEFORE UPDATE ON track WHEN NEW.genre = 'reject'
             BEGIN SELECT RAISE(ABORT, 'rejected metadata'); END;",
        )
        .unwrap();
    }
    assert!(
        s.set_metadata("t7", "changed", "", "", "reject", "", "changed comment")
            .is_err()
    );
    assert_eq!(s.edits("t7").unwrap(), before);
    assert_eq!(s.journal_seq().unwrap(), seq);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn metadata_follows_legacy_adoption_copies_and_rekey() {
    let dir = temp("metadata-adoption");
    let mut tracks = Vec::new();
    for folder in ["a", "b"] {
        let folder = dir.join(folder);
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("same.mp3");
        std::fs::write(&file, b"audio").unwrap();
        tracks.push(Track {
            file: Some(file),
            title: "source".into(),
            ..Default::default()
        });
    }
    let s = Store::in_memory().unwrap();
    let legacy = s.key_of(&tracks[0]);
    s.set_metadata(&legacy, "ours", "artist", "", "genre", "label", "comment")
        .unwrap();
    let expected = s.edits(&legacy).unwrap();
    s.collection_put(&tracks, false).unwrap();
    assert_eq!(s.edits(&legacy).unwrap(), Default::default());
    for track in &tracks {
        let key = s.key_of(track);
        assert_eq!(s.edits(&key).unwrap(), expected);
    }
    let first = s.key_of(&tracks[0]);
    s.rekey(&first, "replacement|5").unwrap();
    assert_eq!(s.edits("replacement|5").unwrap(), expected);
    assert_eq!(s.edits(&first).unwrap(), Default::default());
    std::fs::remove_dir_all(dir).unwrap();
}
