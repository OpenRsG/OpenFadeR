//! Importing into our Collection ([`crate::store`]): music files and folders, M3U / M3U8 / PLS playlists, and whole other
//! libraries (DJ-library XML, iTunes) with their playlists, ratings, comments, cues and beat grids.
//!
//! `known` is the set of files the Collection already lists (what `Library::tracks(None)` returns); those are skipped, never
//! duplicated. Edits of an imported library (rating, comment, cues, grid) and playlist entries are stored under the key of the
//! track's Collection row ([`crate::store::Store::key_of`]), so a file that is missing now keeps them too.

use super::{folder, playlist_file, today, Library, Playlist, Track};
use crate::store::{NodeKind, Store, R};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Why an entry of the source did not join the Collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The entry names no file, or the playlist file lists one that is not there.
    MissingFile,
    /// The file's extension is not a format the app decodes.
    UnsupportedFormat,
    /// The Collection lists that file already (or the source lists it twice).
    Duplicate,
}

/// One entry that was not imported: the file's path (the track's title when it has no file) and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub name: String,
    pub reason: Reason,
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// New tracks in the Collection.
    pub added: usize,
    /// Tracks the Collection had already.
    pub known: usize,
    /// Entries that could not be imported (file not found).
    pub failed: usize,
    /// Tracks whose file is not a format the app decodes.
    pub unsupported: usize,
    /// Playlists and folders made in My Playlists.
    pub playlists: usize,
    /// Cue points and beat grids (tracks with one) the source held for the new tracks.
    pub cues: usize,
    pub grids: usize,
    /// New tracks whose file is not at its path now: they are listed (with their edits) and show as missing.
    pub absent: usize,
    /// Every entry not imported, in source order; the counts above are these by reason.
    pub skipped: Vec<Skipped>,
    /// The files that were added, for analysis.
    pub files: Vec<PathBuf>,
    /// The playlist a playlist file made (store node id), to show it.
    pub node: Option<i64>,
}

/// How the report names an entry: its path, else "artist — title".
fn label(t: &Track) -> String {
    match (&t.file, t.artist.is_empty()) {
        (Some(f), _) => f.display().to_string(),
        (None, true) => t.title.clone(),
        (None, false) => format!("{} — {}", t.artist, t.title),
    }
}

impl Report {
    fn skip(&mut self, name: String, reason: Reason) {
        match reason {
            Reason::MissingFile => self.failed += 1,
            Reason::UnsupportedFormat => self.unsupported += 1,
            Reason::Duplicate => self.known += 1,
        }
        self.skipped.push(Skipped { name, reason });
    }

    pub fn text(&self) -> String {
        let mut s = format!(
            "imported {} track{}",
            self.added,
            if self.added == 1 { "" } else { "s" }
        );
        for (n, what) in [
            (self.playlists, "playlists"),
            (self.known, "already in the Collection"),
            (self.failed, "not found"),
            (self.unsupported, "not a supported format"),
        ] {
            if n > 0 {
                s += &format!(" · {n} {what}");
            }
        }
        s
    }
}

/// The music files named by `paths`: a file as is (any extension, the user chose it), a folder's music files.
pub fn audio_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found = Vec::new();
            folder::walk(p, &mut found);
            found.sort();
            out.extend(found);
        } else if p.is_file() {
            out.push(p.clone());
        }
    }
    out
}

/// Reads the tags of `files` not in `known` and adds them to the Collection. `progress(done, total)` is called per file.
pub fn files(
    store: &Store,
    known: &HashSet<PathBuf>,
    paths: &[PathBuf],
    progress: &dyn Fn(usize, usize),
) -> R<Report> {
    let all = audio_files(paths);
    let mut r = Report::default();
    let mut tracks = Vec::new();
    for (i, f) in all.iter().enumerate() {
        progress(i, all.len());
        if known.contains(f) {
            r.skip(f.display().to_string(), Reason::Duplicate);
            continue;
        }
        let mut t = folder::read_track(f);
        t.date_added = today();
        tracks.push(t);
    }
    r.added = store.collection_put(&tracks, false)?;
    r.known += tracks.len() - r.added;
    r.files = tracks.into_iter().filter_map(|t| t.file).collect();
    progress(all.len(), all.len());
    Ok(r)
}

/// A playlist file: its existing tracks join the Collection and a new playlist of that name lists them in file order. One undo
/// step.
pub fn playlist_file(
    store: &Store,
    known: &HashSet<PathBuf>,
    path: &Path,
    progress: &dyn Fn(usize, usize),
) -> R<Report> {
    let parsed = playlist_file::read(path)?;
    let (found, lost): (Vec<PathBuf>, Vec<PathBuf>) =
        parsed.files.iter().cloned().partition(|f| f.is_file());
    store.action("Import playlist", || {
        let mut r = files(store, known, &found, progress)?;
        for f in lost {
            r.skip(f.display().to_string(), Reason::MissingFile);
        }
        let keys: Vec<String> = found
            .iter()
            .map(|f| {
                store.key_of(&Track {
                    file: Some(f.clone()),
                    ..Track::default()
                })
            })
            .collect();
        let id = store.add_node(None, &parsed.name, NodeKind::Playlist)?;
        store.add_entries(id, &keys)?;
        r.playlists = 1;
        r.node = Some(id);
        Ok(r)
    })
}

/// What of a source [`library`] imports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope<'a> {
    /// Every track; with `playlists` the whole playlist tree too.
    All { playlists: bool },
    /// One playlist (a folder: everything under it): its tracks and a playlist of it.
    List(&'a str),
}

/// The tracks of `src` named by `scope` join the Collection (those with a file, new ones only) with their rating, comment,
/// cues and grid; the playlists of `scope` become new top-level items of My Playlists. One undo step.
pub fn library(
    store: &Store,
    src: &dyn Library,
    known: &HashSet<PathBuf>,
    scope: Scope,
    progress: &dyn Fn(usize, usize),
) -> R<Report> {
    store.action("Import", || {
        import_library(store, src, known, scope, progress)
    })
}

fn import_library(
    store: &Store,
    src: &dyn Library,
    known: &HashSet<PathBuf>,
    scope: Scope,
    progress: &dyn Fn(usize, usize),
) -> R<Report> {
    let all = src.tracks(match scope {
        Scope::All { .. } => None,
        Scope::List(id) => Some(id),
    })?;
    let mut r = Report::default();
    let mut fresh = Vec::new();
    // The Collection keys a file by its path, so that is what makes two entries one (shortcut: a moved or re-encoded copy
    // is a new track, as everywhere else in the Collection).
    let mut seen = HashSet::new();
    for (i, t) in all.iter().enumerate() {
        progress(i, all.len());
        match &t.file {
            None => r.skip(label(t), Reason::MissingFile),
            Some(f) if !folder::is_audio(f) => r.skip(label(t), Reason::UnsupportedFormat),
            Some(f) if known.contains(f) || !seen.insert(f) => r.skip(label(t), Reason::Duplicate),
            Some(_) => fresh.push(Track {
                date_added: if t.date_added.is_empty() {
                    today()
                } else {
                    t.date_added.clone()
                },
                ..t.clone()
            }),
        }
    }
    r.added = store.collection_put(&fresh, false)?;
    r.absent = fresh
        .iter()
        .filter(|t| t.file.as_deref().is_some_and(|f| !f.exists()))
        .count();
    for t in &fresh {
        let k = store.key_of(t);
        if t.rating > 0 {
            store.set_rating(&k, Some(t.rating))?;
        }
        if !t.comment.is_empty() {
            store.set_comment(&k, Some(&t.comment))?;
        }
        if let Some(a) = src.analysis(t) {
            if !a.cues.is_empty() {
                store.set_cues(&k, Some(&a.cues))?;
                r.cues += a.cues.len();
            }
            if !a.beats.is_empty() {
                store.set_grid(&k, &a.beats, &a.beats)?;
                r.grids += 1;
            }
        }
    }
    r.files = fresh.into_iter().filter_map(|t| t.file).collect();
    r.playlists = match scope {
        Scope::All { playlists: false } => 0,
        Scope::All { playlists: true } => copy_playlists(store, src, &src.playlists()?)?,
        Scope::List(id) => {
            let all = src.playlists()?;
            let at = all
                .iter()
                .position(|p| p.id == id)
                .ok_or("that playlist is gone")?;
            let base = all[at].depth;
            let tree: Vec<Playlist> = all[at..]
                .iter()
                .enumerate()
                .take_while(|(n, p)| *n == 0 || p.depth > base)
                .map(|(_, p)| Playlist {
                    depth: p.depth - base,
                    ..p.clone()
                })
                .collect();
            copy_playlists(store, src, &tree)?
        }
    };
    progress(all.len(), all.len());
    Ok(r)
}

/// `tree` (depth-first with depths, as [`Library::playlists`] returns it) as new top-level items of My Playlists, each
/// playlist listing its tracks by store key. Returns how many items were made.
pub fn copy_playlists(store: &Store, src: &dyn Library, tree: &[Playlist]) -> R<usize> {
    let mut parents: Vec<Option<i64>> = vec![None];
    for p in tree {
        parents.truncate(p.depth + 1);
        let parent = parents[p.depth.min(parents.len() - 1)];
        if p.is_folder {
            parents.push(Some(store.add_node(parent, &p.name, NodeKind::Folder)?));
        } else {
            let keys: Vec<String> = src
                .tracks(Some(&p.id))?
                .iter()
                .map(|t| store.key_of(t))
                .collect();
            let id = store.add_node(parent, &p.name, NodeKind::Playlist)?;
            store.add_entries(id, &keys)?;
        }
    }
    Ok(tree.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::id_key;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("openfader-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn files_and_folders_import_once_and_playlist_files_make_a_playlist() {
        let d = tmp("files");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["a.mp3", "sub/b.flac", "sub/notes.txt"] {
            std::fs::write(d.join(f), b"not really audio").unwrap();
        }
        let store = Store::in_memory().unwrap();
        let none = |_: usize, _: usize| {};
        let r = files(&store, &HashSet::new(), std::slice::from_ref(&d), &none).unwrap();
        assert_eq!((r.added, r.known), (2, 0)); // the .txt is no music
        let tracks = store.collection_tracks().unwrap();
        assert_eq!(
            tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        ); // untagged: title = file stem
        assert!(!tracks[0].date_added.is_empty());
        // Again: nothing new.
        let known: HashSet<PathBuf> = tracks.iter().filter_map(|t| t.file.clone()).collect();
        let again = files(&store, &known, &[d.clone()], &none).unwrap();
        assert_eq!((again.added, again.known), (0, 2));
        assert!(
            again.skipped.iter().all(|s| s.reason == Reason::Duplicate) && again.skipped.len() == 2
        );

        std::fs::write(
            d.join("set.m3u8"),
            format!(
                "#EXTM3U\n{}\n{}\n/gone/x.mp3\n",
                d.join("sub/b.flac").display(),
                d.join("a.mp3").display()
            ),
        )
        .unwrap();
        let r = playlist_file(&store, &known, &d.join("set.m3u8"), &none).unwrap();
        assert_eq!((r.playlists, r.failed), (1, 1));
        // The two listed files are in the Collection already; the entry that is not there is skipped for good.
        assert_eq!(
            r.skipped.iter().map(|s| s.reason).collect::<Vec<_>>(),
            [Reason::Duplicate, Reason::Duplicate, Reason::MissingFile]
        );
        // Windows reads "/gone/x.mp3" as a path on the current drive.
        assert!(Path::new(&r.skipped[2].name).ends_with("gone/x.mp3"), "{}", r.skipped[2].name);
        let node = &store.nodes().unwrap()[0];
        assert_eq!(node.name, "set");
        let keys = store.entries(node.id).unwrap();
        let ids: Vec<_> = tracks.iter().map(|t| id_key(t.id.unwrap())).collect();
        assert_eq!(keys, [ids[1].clone(), ids[0].clone()]); // file order, the missing entry skipped
        std::fs::remove_dir_all(&d).unwrap();
    }

    struct Source(Vec<Track>);
    impl Library for Source {
        fn kind(&self) -> &str {
            "test"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            let p = |id: &str, depth, is_folder| Playlist {
                id: id.into(),
                name: id.into(),
                depth,
                is_folder,
            };
            Ok(vec![p("F", 0, true), p("P", 1, false)])
        }
        fn tracks(&self, p: Option<&str>) -> Result<Vec<Track>, String> {
            Ok(if p == Some("P") {
                self.0[..1].to_vec()
            } else {
                self.0.clone()
            })
        }
        fn analysis(&self, t: &Track) -> Option<crate::analysis::Analysis> {
            use crate::analysis::{Analysis, Beat, Cue};
            let cue = |ms| Cue {
                hot_cue: 1,
                time_ms: ms,
                loop_end_ms: None,
                color: None,
                comment: None,
            };
            (t.title == "Real").then(|| Analysis {
                cues: vec![cue(100), cue(200)],
                beats: vec![Beat {
                    bar_pos: 1,
                    bpm_x100: 12000,
                    time_ms: 0,
                }],
                ..Analysis::default()
            })
        }
        fn analysis_reader(&self) -> Option<crate::library::AnalysisReader> {
            Some(std::sync::Arc::new(|t| Source(Vec::new()).analysis(t)))
        }
    }

    #[test]
    fn a_library_imports_tracks_edits_and_playlists() {
        let d = tmp("lib");
        let real = d.join("real.mp3");
        std::fs::write(&real, b"x").unwrap();
        let t = |n: &str, f: Option<&Path>, rating| Track {
            title: n.into(),
            file: f.map(Path::to_path_buf),
            rating,
            comment: format!("c-{n}"),
            ..Track::default()
        };
        let src = Source(vec![
            t("Real", Some(&real), 4),
            t("Lost", Some(Path::new("/gone/lost.mp3")), 5),
            t("None", None, 0),
        ]);
        let store = Store::in_memory().unwrap();
        let all = Scope::All { playlists: true };
        let r = library(&store, &src, &HashSet::new(), all, &|_, _| {}).unwrap();
        assert_eq!((r.added, r.failed, r.playlists), (2, 1, 2));
        let k = store.key_of(&src.0[0]);
        let e = store.edits(&k).unwrap();
        assert_eq!((e.rating, e.comment.as_deref()), (Some(4), Some("c-Real")));
        assert_eq!(
            store.edits(&store.key_of(&src.0[1])).unwrap().rating,
            Some(5)
        ); // a missing file keeps its edits too
        let nodes = store.nodes().unwrap();
        assert_eq!(
            nodes
                .iter()
                .map(|n| (n.name.as_str(), n.parent.is_some()))
                .collect::<Vec<_>>(),
            [("F", false), ("P", true)]
        );
        assert_eq!(store.entries(nodes[1].id).unwrap(), [k]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn one_playlist_imports_its_tracks_and_only_itself() {
        let d = tmp("list");
        let (a, b) = (d.join("a.mp3"), d.join("b.mp3"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"y").unwrap();
        let t = |n: &str, f: &Path| Track {
            title: n.into(),
            file: Some(f.to_path_buf()),
            ..Track::default()
        };
        let src = Source(vec![t("A", &a), t("B", &b)]); // playlist "P" lists only the first
        let store = Store::in_memory().unwrap();
        let r = library(&store, &src, &HashSet::new(), Scope::List("P"), &|_, _| {}).unwrap();
        assert_eq!((r.added, r.playlists), (1, 1));
        let nodes = store.nodes().unwrap();
        assert_eq!(
            (nodes.len(), nodes[0].name.as_str(), nodes[0].parent),
            (1, "P", None)
        ); // not under the folder "F"
        assert!(library(
            &store,
            &src,
            &HashSet::new(),
            Scope::List("nope"),
            &|_, _| {}
        )
        .is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_report_lists_every_skipped_entry_with_its_reason_and_counts_what_came_in() {
        let d = tmp("report");
        let (real, mine, ogg) = (d.join("real.mp3"), d.join("mine.mp3"), d.join("x.ogg"));
        for f in [&real, &mine, &ogg] {
            std::fs::write(f, b"x").unwrap();
        }
        let t = |n: &str, f: Option<&Path>| Track {
            title: n.into(),
            artist: "Art".into(),
            file: f.map(Path::to_path_buf),
            ..Track::default()
        };
        let src = Source(vec![
            t("Real", Some(&real)),
            t("Lost", Some(Path::new("/gone/lost.mp3"))),
            t("Real twice", Some(&real)),
            t("Mine", Some(&mine)),
            t("Ogg", Some(&ogg)),
            t("None", None),
        ]);
        let known = HashSet::from([mine.clone()]);
        let store = Store::in_memory().unwrap();
        let all = Scope::All { playlists: false };
        let r = library(&store, &src, &known, all, &|_, _| {}).unwrap();
        let skip = |name: String, reason| Skipped { name, reason };
        assert_eq!(
            r.skipped,
            [
                skip(real.display().to_string(), Reason::Duplicate),
                skip(mine.display().to_string(), Reason::Duplicate),
                skip(ogg.display().to_string(), Reason::UnsupportedFormat),
                skip("Art — None".into(), Reason::MissingFile),
            ]
        );
        // The counts are the list by reason; the two imported tracks are the first "Real" and "Lost" (its file is not there).
        assert_eq!(
            (r.added, r.known, r.unsupported, r.failed, r.absent),
            (2, 2, 1, 1, 1)
        );
        assert_eq!((r.cues, r.grids), (2, 1)); // only "Real" has an analysis
        assert_eq!(store.collection_tracks().unwrap().len(), 2);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
