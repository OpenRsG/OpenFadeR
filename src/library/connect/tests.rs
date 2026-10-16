use super::*;
use crate::library::{Library, Playlist, Track};
use crate::store::{id_key, Store};
use std::path::Path;

struct Src {
    tracks: Vec<Track>,
}

impl Library for Src {
    fn kind(&self) -> &str {
        "fake"
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(vec![Playlist {
            id: "p".into(),
            name: "Warm up".into(),
            depth: 0,
            is_folder: false,
        }])
    }
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        Ok(match playlist {
            Some(_) => self.tracks.iter().take(1).cloned().collect(),
            None => self.tracks.clone(),
        })
    }
}

fn track(name: &str) -> Track {
    Track {
        title: name.into(),
        file: Some(format!("/music/{name}.mp3").into()),
        ..Track::default()
    }
}

fn titles(s: &Store) -> Vec<String> {
    let mut t: Vec<String> = s
        .collection_tracks()
        .unwrap()
        .iter()
        .map(|t| t.title.clone())
        .collect();
    t.sort();
    t
}

fn conn(s: &Store, wins: Wins) -> Connection {
    let c = s.connection_add("", Path::new("/fake"), "Fake", 0).unwrap();
    s.connection_set(c.id, Mode::Import, wins).unwrap();
    s.connections().unwrap().remove(0)
}

fn go(s: &Store, c: &Connection, src: &Src) -> Report {
    run(s, c, src, "st", &|_, _| {}).unwrap()
}

#[test]
fn adds_tracks_and_playlists_then_leaves_an_unchanged_source_alone() {
    let s = Store::in_memory().unwrap();
    let c = conn(&s, Wins::Source);
    let src = Src {
        tracks: vec![track("a"), track("b")],
    };
    let r = go(&s, &c, &src);
    assert_eq!((r.added, r.updated, r.removed), (2, 0, 0));
    assert_eq!(titles(&s), ["a", "b"]);
    let nodes = s.nodes().unwrap();
    let root = nodes
        .iter()
        .find(|n| n.name == "Fake")
        .expect("folder named after the connection");
    let list = nodes.iter().find(|n| n.name == "Warm up").unwrap();
    assert_eq!(list.parent, Some(root.id));
    assert_eq!(s.entries(list.id).unwrap().len(), 1);
    let c = s.connections().unwrap().remove(0);
    assert_eq!(go(&s, &c, &src), Report::default());
}

#[test]
fn a_changed_track_follows_the_rule() {
    for (wins, title, stars) in [(Wins::Source, "a2", None), (Wins::Ours, "a", Some(3))] {
        let s = Store::in_memory().unwrap();
        let c = conn(&s, wins);
        let mut src = Src {
            tracks: vec![track("a")],
        };
        go(&s, &c, &src);
        let id = s.collection_tracks().unwrap()[0].id.unwrap();
        s.set_rating(&id_key(id), Some(3)).unwrap();
        src.tracks[0].title = "a2".into();
        let r = go(&s, &s.connections().unwrap()[0], &src);
        assert_eq!(r.updated, usize::from(wins == Wins::Source));
        assert_eq!(s.collection_tracks().unwrap()[0].title, title);
        assert_eq!(s.edits(&id_key(id)).unwrap().rating, stars);
    }
}

#[test]
fn edits_of_ours_survive_when_the_source_did_not_change() {
    let s = Store::in_memory().unwrap();
    let c = conn(&s, Wins::Source);
    let src = Src {
        tracks: vec![track("a")],
    };
    go(&s, &c, &src);
    let id = s.collection_tracks().unwrap()[0].id.unwrap();
    s.set_comment(&id_key(id), Some("mine")).unwrap();
    go(&s, &s.connections().unwrap()[0], &src);
    assert_eq!(
        s.edits(&id_key(id)).unwrap().comment.as_deref(),
        Some("mine")
    );
}

#[test]
fn tracks_that_leave_the_source_stay_in_the_collection() {
    let s = Store::in_memory().unwrap();
    let c = conn(&s, Wins::Source);
    go(
        &s,
        &c,
        &Src {
            tracks: vec![track("a"), track("b")],
        },
    );
    let r = go(&s, &s.connections().unwrap()[0], &Src { tracks: vec![] });
    assert_eq!(r.removed, 2);
    assert_eq!(titles(&s), ["a", "b"]);
    assert_eq!(s.connection_tracks(c.id).unwrap(), 0);
}

#[test]
fn adopt_makes_a_folder_connection_of_a_directory() {
    let s = Store::in_memory().unwrap();
    let dir = std::env::temp_dir().join(format!("ofr-adopt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let c = adopt(&s, &dir).unwrap();
    assert_eq!((c.plugin.as_str(), c.path.as_path()), ("", dir.as_path()));
    assert_eq!(adopt(&s, &dir).unwrap().id, c.id);
    assert!(adopt(&s, &dir.join("gone")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn detect_patterns_expand_globs_and_home() {
    let dir = std::env::temp_dir().join(format!("ofr-detect-{}", std::process::id()));
    for v in ["Tool 3.2", "Tool 4.0", "Other"] {
        std::fs::create_dir_all(dir.join(v)).unwrap();
        std::fs::write(dir.join(v).join("collection.nml"), b"x").unwrap();
    }
    let pat = format!("{}/Tool */collection.nml", dir.display());
    let found = expand(&pat);
    assert_eq!(
        found,
        [
            dir.join("Tool 3.2/collection.nml"),
            dir.join("Tool 4.0/collection.nml")
        ]
    );
    assert!(expand(&format!("{}/Tool */missing.nml", dir.display())).is_empty());
    if let Some(home) = crate::paths::home() {
        assert_eq!(expand("~/"), [home]);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A source whose playlist tree is chosen by the test.
struct Tree(Vec<Playlist>);

impl Library for Tree {
    fn kind(&self) -> &str {
        "fake"
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(self.0.clone())
    }
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        Ok(playlist.map_or_else(|| vec![track("a")], |_| vec![track("a")]))
    }
}

fn list(id: &str, depth: usize, is_folder: bool) -> Playlist {
    Playlist {
        id: id.into(),
        name: id.into(),
        depth,
        is_folder,
    }
}

#[test]
fn a_playlist_that_left_its_folder_survives_the_folders_removal() {
    let s = Store::in_memory().unwrap();
    let c = conn(&s, Wins::Source);
    run(
        &s,
        &c,
        &Tree(vec![list("F", 0, true), list("P", 1, false)]),
        "1",
        &|_, _| {},
    )
    .unwrap();
    let c = s.connections().unwrap().remove(0);
    run(&s, &c, &Tree(vec![list("P", 0, false)]), "2", &|_, _| {}).unwrap();
    let nodes = s.nodes().unwrap();
    let root = nodes.iter().find(|n| n.name == "Fake").unwrap();
    let p = nodes.iter().find(|n| n.name == "P").expect("kept");
    assert_eq!(p.parent, Some(root.id));
    assert_eq!(s.entries(p.id).unwrap().len(), 1);
    assert!(nodes.iter().all(|n| n.name != "F"));
}

#[test]
fn the_winning_source_clears_what_it_no_longer_has() {
    let s = Store::in_memory().unwrap();
    let c = conn(&s, Wins::Source);
    let mut src = Src {
        tracks: vec![track("a")],
    };
    src.tracks[0].comment = "hi".into();
    go(&s, &c, &src);
    let id = s.collection_tracks().unwrap()[0].id.unwrap();
    s.set_comment(&id_key(id), Some("mine")).unwrap();
    src.tracks[0].comment = String::new();
    src.tracks[0].title = "a2".into();
    go(&s, &s.connections().unwrap()[0], &src);
    assert!(s
        .edits(&id_key(id))
        .unwrap()
        .comment
        .unwrap_or_default()
        .is_empty());
}

/// A source whose every track has one hot cue at 2.5 s.
struct Cued(Src);

impl Library for Cued {
    fn kind(&self) -> &str {
        "cued"
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        self.0.playlists()
    }
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        self.0.tracks(playlist)
    }
    fn analysis_reader(&self) -> Option<crate::library::AnalysisReader> {
        Some(std::sync::Arc::new(|_| {
            Some(crate::analysis::Analysis {
                cues: vec![cue(2500)],
                ..Default::default()
            })
        }))
    }
}

fn cue(ms: u32) -> crate::analysis::Cue {
    crate::analysis::Cue {
        hot_cue: 1,
        time_ms: ms,
        loop_end_ms: None,
        color: None,
        comment: None,
    }
}

#[test]
fn the_source_cues_replace_automatic_cues_but_never_the_users() {
    for (auto, kept) in [(true, 2500), (false, 900)] {
        let s = Store::in_memory().unwrap();
        let id = s.collection_upsert(&track("a"), None).unwrap();
        let key = id_key(id);
        if auto {
            s.set_auto_cues(&key, &[cue(900)]).unwrap();
        } else {
            s.set_cues(&key, Some(&[cue(900)])).unwrap();
        }
        let c = conn(&s, Wins::Ours);
        run(
            &s,
            &c,
            &Cued(Src {
                tracks: vec![track("a")],
            }),
            "st",
            &|_, _| {},
        )
        .unwrap();
        let cues = s.edits(&key).unwrap().cues.unwrap();
        assert_eq!(cues[0].time_ms, kept);
    }
}

#[test]
fn a_folder_stamp_sees_changes_at_any_depth() {
    let s = Store::in_memory().unwrap();
    let dir = std::env::temp_dir().join(format!("ofr-stamp-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("Artist/Album")).unwrap();
    let c = adopt(&s, &dir).unwrap();
    let before = stamp(&c);
    std::fs::write(dir.join("Artist/Album/song.mp3"), b"x").unwrap();
    let added = stamp(&c);
    assert_ne!(before, added);
    std::fs::write(dir.join("Artist/Album/song.mp3"), b"xy").unwrap();
    assert_ne!(added, stamp(&c));
    std::fs::remove_dir_all(&dir).unwrap();
}
