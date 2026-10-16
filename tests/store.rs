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
        s.set_comment("k", Some("warm-up")).unwrap();
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
struct Lib(Vec<Track>);

impl Library for Lib {
    fn kind(&self) -> &str {
        "test"
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(vec![])
    }
    fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
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
        file: Some(f.clone()),
        rating: 1,
        comment: "library".into(),
        ..Default::default()
    };
    let tracks: Vec<Track> = files.iter().map(track).collect();
    let store = Arc::new(Store::in_memory().unwrap());
    let lib = Overlay::new(Box::new(Lib(tracks.clone())), store.clone());

    // Untouched store: the library as it is.
    assert_eq!(lib.tracks(None).unwrap(), tracks);
    assert_eq!(lib.analysis(&tracks[0]).unwrap().cues.len(), 2);

    let k = store::key(&tracks[0]);
    store.set_rating(&k, Some(5)).unwrap();
    store.set_comment(&k, Some("mine")).unwrap();
    store.set_cues(&k, Some(&[cue(3, 3000)])).unwrap();
    store.set_grid(&k, &[beat(1, 50)], &[]).unwrap();

    let got = lib.tracks(None).unwrap();
    assert_eq!((got[0].rating, got[0].comment.as_str()), (5, "mine"));
    assert_eq!(got[1], tracks[1]); // another track: untouched
    let a = lib.analysis(&tracks[0]).unwrap();
    assert_eq!((a.cues, a.beats), (vec![cue(3, 3000)], vec![beat(1, 50)]));
    assert_eq!(lib.analysis(&tracks[1]).unwrap().cues.len(), 2);

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
