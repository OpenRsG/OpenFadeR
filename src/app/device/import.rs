//! Import from a device: its tracks copied onto this computer and added to the collection with what the device knows of them
//! (key, BPM, rating, comment, cues, hot cues, loops, beat grid, phrases). The track menu's "Import to Collection" on tracks of
//! a device ([`import_tracks`]), and a device playlist's (its right-click menu in the tree, its + button), which also becomes a
//! My Playlists playlist of the copies.
//!
//! The audio goes to `<Music>/<device label>/<its file name>` ([`copy_audio`]: whole or not at all), read from the device
//! only: nothing is written to it. A track the collection already has (same file name and size, [`Store::key_of`]) is not copied
//! again, and one the collection only listed on the device (an older import) moves to its copy. Where a track the collection
//! already has holds other values than the device's, nothing is overwritten by itself: those tracks open the review dialog
//! ([`update::row_of`], every track ticked), APPLY takes the device's values of the ticked ones, CANCEL keeps all of yours, and
//! UNDO stays available.

use super::{check::copy_audio, toast, transfer::with_ctx, Ctx};
use crate::app::{analyze, prefs};
use openfader::{
    device::{self, link, update},
    library::{AnalysisReader, Library, Playlist, Track},
    plugin::device::Control,
    store::{NodeKind, Store},
};
use parking_lot::Mutex;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{atomic::Ordering::Relaxed, Arc},
};

/// What [`copy_in`] did.
#[derive(Default)]
pub(super) struct Copied {
    /// The new collection tracks (for the analysis).
    pub added: Vec<Track>,
    /// Collection tracks that were listed on the device and now are the copy.
    pub moved: usize,
    /// Tracks the collection already had, with other values than the device's: the review's rows.
    pub rows: Vec<update::Row>,
    /// Device tracks whose file is not there.
    pub missing: usize,
}

/// What is imported: tracks (by their file on the device), or a device playlist (and what is under it).
enum Pick {
    Tracks(HashSet<PathBuf>),
    Playlist(String),
}

/// The track menu's "Import to Collection" on `tracks` of device `device`.
pub fn import_tracks(device: &str, tracks: &[Track]) {
    let files = tracks.iter().filter_map(|t| t.file.clone()).collect();
    with_ctx(|cx| cx.import_from(device, Pick::Tracks(files)));
}

impl Ctx {
    /// A device playlist's "Import to Collection" (the tree's right-click menu, its + button).
    pub(super) fn import_playlist(self: &Rc<Self>, device: &str, id: &str) {
        self.import_from(device, Pick::Playlist(id.to_owned()));
    }

    /// Copies what `pick` names from device `id` into the collection (a job); the tracks it already had with other values open the
    /// review.
    fn import_from(self: &Rc<Self>, id: &str, pick: Pick) {
        let Some(d) = self.device_or_say(id) else {
            return;
        };
        let Some(music) = crate::app::library::settings::music_dir() else {
            return self.tell(tr!("No Music folder to copy into: set XDG_MUSIC_DIR"), true);
        };
        if self.busy() {
            return self.tell(crate::app::i18n::tr_dyn(super::BUSY), true);
        }
        let dest = music.join(safe_name(&d.label));
        let (store, reader) = (
            self.store.clone(),
            self.lib
                .analysis_reader()
                .unwrap_or_else(|| Arc::new(|_: &Track| None)),
        );
        let slot = Arc::new(Mutex::new(Copied::default()));
        let (out, dev) = (slot.clone(), d.clone());
        self.start_then(
            Some(d.id.clone()),
            &tr!("Copying from {}", d.label),
            move |ctl| {
                let lib = device::open_library(&dev)?;
                let (tracks, list) = match &pick {
                    Pick::Tracks(files) => {
                        let all = lib.tracks(None)?;
                        (
                            all.into_iter()
                                .filter(|t| t.file.as_ref().is_some_and(|f| files.contains(f)))
                                .collect(),
                            None,
                        )
                    }
                    Pick::Playlist(id) => {
                        let pls = lib.playlists()?;
                        let at = pls
                            .iter()
                            .position(|p| p.id == *id)
                            .ok_or(tr!("that playlist is not on the device any more"))?;
                        (under(&*lib, &pls, at)?, Some((pls, at)))
                    }
                };
                let c = copy_in(&store, &*lib, &reader, &tracks, &dev.mount, &dest, &ctl)?;
                if let Some((pls, at)) = &list {
                    store.system(|| make_node(&store, &*lib, pls, *at, None))?;
                }
                let mut text = match &list {
                    Some((pls, at)) => tr!(
                        "Imported {} from {} into My Playlists",
                        pls[*at].name,
                        dev.label
                    ),
                    None => tr!("Imported from {}", dev.label),
                };
                text += &tr!(
                    ": {} track(s) copied to {}, {} already in your collection",
                    c.added.len() + c.moved,
                    dest.display(),
                    tracks.len().saturating_sub(c.added.len() + c.moved + c.missing)
                );
                if c.missing > 0 {
                    text += &tr!("; {} file(s) missing on the device", c.missing);
                }
                *out.lock() = c;
                Ok(text)
            },
            move |cx, r| {
                let c = std::mem::take(&mut *slot.lock());
                match r {
                    Ok(text) => cx.tell(text.clone(), false),
                    Err(e) if e == "cancelled" => {}
                    Err(e) => cx.tell(e.clone(), true),
                }
                cx.reload_browser();
                // Only what the device did not analyse: CUE Analysis would replace the cues it brought.
                let todo: Vec<Track> = c.added.into_iter().filter(|t| t.bpm_x100 == 0 || t.key.is_empty()).collect();
                if !todo.is_empty() && prefs::with(|p| p.analysis.auto_analysis) {
                    analyze::request(&todo, false);
                }
                if !c.rows.is_empty() {
                    cx.open_update_review(&d, update::of_rows(c.rows), true);
                }
            },
        );
    }

    /// The window's status line and a toast: an import is started from the browser, with the window closed.
    fn tell(&self, text: impl AsRef<str>, failed: bool) {
        let text = text.as_ref();
        self.say(text, failed);
        if let Some(app) = self.app() {
            toast::show(&app, text);
        }
    }
}

/// `text` as one folder name: no separators, never empty.
fn safe_name(text: &str) -> String {
    let s: String = text
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    match s.trim() {
        "" | "." | ".." => "device".to_owned(),
        t => t.to_owned(),
    }
}

/// The tracks of device playlist `at` and of every playlist under it, each file once.
fn under(lib: &dyn Library, pls: &[Playlist], at: usize) -> Result<Vec<Track>, String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let depth = pls[at].depth;
    for p in pls[at..]
        .iter()
        .enumerate()
        .take_while(|(n, p)| *n == 0 || p.depth > depth)
        .map(|(_, p)| p)
    {
        if !p.is_folder {
            out.extend(
                lib.tracks(Some(&p.id))?
                    .into_iter()
                    .filter(|t| t.file.clone().is_some_and(|f| seen.insert(f))),
            );
        }
    }
    Ok(out)
}

/// Device playlist `at` (and what is under it) as a My Playlists node under `parent`, listing the collection's copies.
fn make_node(
    store: &Store,
    lib: &dyn Library,
    pls: &[Playlist],
    at: usize,
    parent: Option<i64>,
) -> Result<(), String> {
    let p = &pls[at];
    if p.is_folder {
        let node = store.add_node(parent, &p.name, NodeKind::Folder)?;
        for j in (at + 1..pls.len())
            .take_while(|&j| pls[j].depth > p.depth)
            .filter(|&j| pls[j].depth == p.depth + 1)
        {
            make_node(store, lib, pls, j, Some(node))?;
        }
        return Ok(());
    }
    // A device track's key is its copy's row now (same file name and size).
    let keys: Vec<String> = lib
        .tracks(Some(&p.id))?
        .iter()
        .map(|t| store.key_of(t))
        .collect();
    let node = store.add_node(parent, &p.name, NodeKind::Playlist)?;
    store.set_entries(node, &keys)
}

/// Where the copy of `file` goes: `dest/<its file name>` (one folder per device; the Music folder's playlists follow its folders,
/// so a stick's own layout would make dozens). A different file already there stays, and the copy goes into `dest (2)`,
/// `dest (3)` … instead, keeping its name (the collection knows a device track by file name and size). `true` with the path:
/// that very file is there already (same size), no copy needed.
fn target(file: &Path, dest: &Path) -> (PathBuf, bool) {
    let rel = Path::new(file.file_name().unwrap_or_default());
    let size = |p: &Path| std::fs::metadata(p).ok().map(|m| m.len());
    let want = size(file);
    let name = dest.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut root = dest.to_path_buf();
    for n in 2.. {
        let at = root.join(rel);
        match size(&at) {
            None => return (at, false),
            Some(s) if Some(s) == want => return (at, true),
            Some(_) => root = dest.with_file_name(format!("{name} ({n})")),
        }
    }
    unreachable!()
}

/// Copies `tracks` (read from the device library `lib` mounted at `mount`) into `dest` and the collection, see the module docs.
/// The collection's own analysis is read with `reader`. Progress and cancel through `ctl`.
pub(super) fn copy_in(
    store: &Store,
    lib: &dyn Library,
    reader: &AnalysisReader,
    tracks: &[Track],
    mount: &Path,
    dest: &Path,
    ctl: &Control,
) -> Result<Copied, String> {
    let mut out = Copied::default();
    ctl.start(tracks.len());
    let size = |t: &Track| {
        t.file
            .as_deref()
            .and_then(|f| std::fs::metadata(f).ok())
            .map_or(0, |m| m.len())
    };
    ctl.bytes_total
        .store(tracks.iter().map(size).sum(), Relaxed);
    for t in tracks {
        if ctl.cancelled() {
            return Err("cancelled".into());
        }
        let Some(file) = t.file.as_deref().filter(|f| f.is_file()) else {
            out.missing += 1;
            ctl.done.fetch_add(1, Relaxed);
            continue;
        };
        let dev_a = lib.analysis(t);
        let key = store.key_of(t);
        let row = key
            .strip_prefix('t')
            .and_then(|n| n.parse().ok())
            .map(|id| store.collection_track(id))
            .transpose()?
            .flatten();
        let copy = || -> Result<PathBuf, String> {
            let (to, there) = target(file, dest);
            if there {
                ctl.bytes_done.fetch_add(size(t), Relaxed);
            } else {
                copy_audio(file, &to, ctl)?;
            }
            Ok(to)
        };
        match row {
            Some(mut row) => {
                if row.file.as_deref().is_none_or(|f| f.starts_with(mount)) {
                    let to = copy()?;
                    store.relocate(&row, &to)?;
                    row.file = Some(to);
                    out.moved += 1;
                } else {
                    ctl.bytes_done.fetch_add(size(t), Relaxed);
                }
                let key = store.key_of(&row);
                let mut col = row.clone();
                store.edits(&key)?.apply_metadata(&mut col);
                let col_a = link::collection_analysis(reader, store, &row);
                let color = store.track_color(&key)?;
                out.rows
                    .extend(update::row_of(key, &col, &col_a, color, t, dev_a.as_ref()));
            }
            None => {
                let new = Track {
                    id: None,
                    file: Some(copy()?),
                    // The device's analysis files and artwork stay on the device: the edits below carry what counts.
                    analysis_ref: None,
                    artwork: None,
                    date_added: if t.date_added.is_empty() {
                        chrono::Local::now().format("%Y-%m-%d").to_string()
                    } else {
                        t.date_added.clone()
                    },
                    ..t.clone()
                };
                store.collection_put(std::slice::from_ref(&new), false)?;
                let k = store.key_of(&new);
                if new.rating > 0 {
                    store.set_rating(&k, Some(new.rating))?;
                }
                if !new.comment.is_empty() {
                    store.set_comment(&k, Some(&new.comment))?;
                }
                if let Some(a) = &dev_a {
                    let m = link::Marks::from(a);
                    link::pull(
                        store,
                        &k,
                        &link::Parts {
                            cues: Some(m.cues).filter(|c| !c.is_empty()),
                            beats: Some(m.beats).filter(|b| !b.is_empty()),
                            phrases: m.phrases,
                        },
                        &[],
                    )?;
                }
                out.added.push(Track {
                    id: k.strip_prefix('t').and_then(|n| n.parse().ok()),
                    ..new
                });
            }
        }
        ctl.done.fetch_add(1, Relaxed);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::{Analysis, Beat, Cue};

    /// A device library: its tracks with their analysis.
    struct Stick(Vec<(Track, Analysis)>);

    impl Library for Stick {
        fn kind(&self) -> &str {
            "stick"
        }
        fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
            Ok(self.0.iter().map(|(t, _)| t.clone()).collect())
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            Ok(Vec::new())
        }
        fn analysis(&self, t: &Track) -> Option<Analysis> {
            self.0
                .iter()
                .find(|(s, _)| s.file == t.file)
                .map(|(_, a)| a.clone())
        }
    }

    fn cue(ms: u32) -> Cue {
        Cue {
            hot_cue: 1,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    fn grid(start: u32) -> Vec<Beat> {
        (0..8)
            .map(|i| Beat {
                bar_pos: (i % 4) as u16 + 1,
                bpm_x100: 12000,
                time_ms: start + i * 500,
            })
            .collect()
    }

    #[test]
    fn new_tracks_are_copied_with_their_marks_and_known_ones_wait_for_the_review() {
        let dir = std::env::temp_dir().join(format!("of-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (stick, music, mine) = (dir.join("stick"), dir.join("Music/STICK"), dir.join("mine"));
        for d in [stick.join("Contents/A"), mine.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(stick.join("Contents/A/new.mp3"), vec![1u8; 4000]).unwrap();
        std::fs::write(stick.join("Contents/A/known.mp3"), vec![2u8; 3000]).unwrap();
        std::fs::write(mine.join("known.mp3"), vec![2u8; 3000]).unwrap();
        // A different file already holds the new track's name in the Music folder: it stays.
        std::fs::create_dir_all(&music).unwrap();
        std::fs::write(music.join("new.mp3"), b"other").unwrap();
        let store = Store::in_memory().unwrap();
        let known = Track {
            title: "Known".into(),
            key: "8A".into(),
            file: Some(mine.join("known.mp3")),
            ..Track::default()
        };
        store
            .collection_put(std::slice::from_ref(&known), false)
            .unwrap();
        let on = |name: &str, key: &str, rating: u8| Track {
            title: name.into(),
            key: key.into(),
            rating,
            file: Some(stick.join(format!("Contents/A/{name}.mp3"))),
            ..Track::default()
        };
        let lib = Stick(vec![
            (
                on("new", "5A", 4),
                Analysis {
                    cues: vec![cue(1000)],
                    beats: grid(20),
                    ..Analysis::default()
                },
            ),
            // Am is 8A: the key is no difference, the cues are.
            (
                on("known", "Am", 0),
                Analysis {
                    cues: vec![cue(2000)],
                    ..Analysis::default()
                },
            ),
        ]);
        let reader: AnalysisReader = Arc::new(|_: &Track| None);
        let ctl = Control::default();
        let c = copy_in(
            &store,
            &lib,
            &reader,
            &lib.tracks(None).unwrap(),
            &stick,
            &music,
            &ctl,
        )
        .unwrap();

        let copy = dir.join("Music/STICK (2)/new.mp3");
        assert_eq!(std::fs::read(&copy).unwrap().len(), 4000);
        assert_eq!(
            std::fs::read(music.join("new.mp3")).unwrap(),
            b"other"
        );
        assert_eq!((c.added.len(), c.moved, c.missing), (1, 0, 0));
        let k = store.key_of(&c.added[0]);
        let e = store.edits(&k).unwrap();
        assert_eq!(
            (e.rating, e.cues, e.grid),
            (Some(4), Some(vec![cue(1000)]), Some(grid(20)))
        );
        assert_eq!(
            store
                .collection_track(c.added[0].id.unwrap())
                .unwrap()
                .unwrap()
                .key,
            "5A"
        );
        assert!(
            !mine.join("known (2).mp3").exists(),
            "a known track is not copied"
        );
        // The known track: nothing written to it, one review row with the cues.
        assert_eq!(c.rows.len(), 1);
        assert_eq!(
            c.rows[0].diff,
            vec![update::Diff::Cues {
                added: 1,
                removed: 0,
                moved: 0
            }]
        );
        let kk = store.key_of(&known);
        assert_eq!(store.edits(&kk).unwrap().cues, None);
        // APPLY takes the device's cues; UNDO gives them back.
        let a = update::apply(
            &update::of_rows(c.rows),
            &[true],
            "dev",
            "Import",
            &store,
            1,
        )
        .unwrap();
        assert_eq!(store.edits(&kk).unwrap().cues, Some(vec![cue(2000)]));
        openfader::device::undo::run(&store, a.undo.unwrap()).unwrap();
        assert_eq!(store.edits(&kk).unwrap().cues, None);

        // Again: the copy is in the collection now, so nothing is copied twice and nothing differs.
        let again = copy_in(
            &store,
            &lib,
            &reader,
            &lib.tracks(None).unwrap()[..1],
            &stick,
            &music,
            &ctl,
        )
        .unwrap();
        assert_eq!((again.added.len(), again.rows.len()), (0, 0));
        assert!(!dir.join("Music/STICK (3)").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_key_is_a_review_row_and_apply_writes_it_back_undoably() {
        let store = Store::in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("of-import-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("k.mp3"), b"k").unwrap();
        let t = Track {
            key: "8A".into(),
            file: Some(dir.join("k.mp3")),
            ..Track::default()
        };
        store
            .collection_put(std::slice::from_ref(&t), false)
            .unwrap();
        let k = store.key_of(&t);
        let dev = Track {
            key: "9A".into(),
            ..t.clone()
        };
        let row = update::row_of(k.clone(), &t, &Analysis::default(), None, &dev, None).unwrap();
        assert_eq!(
            row.diff,
            vec![update::Diff::Key {
                from: "8A".into(),
                to: "9A".into()
            }]
        );
        let a = update::apply(
            &update::of_rows(vec![row]),
            &[true],
            "dev",
            "Import",
            &store,
            1,
        )
        .unwrap();
        assert_eq!(store.row_tkey(&k).unwrap().as_deref(), Some("9A"));
        openfader::device::undo::run(&store, a.undo.unwrap()).unwrap();
        assert_eq!(store.row_tkey(&k).unwrap().as_deref(), Some("8A"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
