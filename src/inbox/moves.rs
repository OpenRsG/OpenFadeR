//! A batch of moves: tracks go to another stage, their files with them when the user asked for that, and the whole batch is one
//! undo step ([`Store::stage_moves`]). A track already in the Collection stays there; a track already in the stage is skipped.
//! A file that is gone does not stop a move: the row moves on, the file stays missing. A file that cannot be carried is reported
//! and its track stays where it was; the others move.

use super::{rules, Config};
use crate::{
    paths,
    store::{Move, Role, Store, R},
};
use std::path::PathBuf;

/// What a batch did.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub moved: usize,
    /// Tracks left where they were (in the stage already, in the Collection, or changed since).
    pub skipped: usize,
    /// Tracks whose file could not be carried, with the reason.
    pub failed: Vec<String>,
    /// The files of the moved tracks as they are now.
    pub files: Vec<PathBuf>,
}

/// One track to take to `to_stage`: its file goes to `wanted` (`free`: a taken name gets a ` (2)` suffix; else a taken name
/// fails the track).
struct Plan {
    track: i64,
    from_stage: i64,
    from_path: PathBuf,
    to_stage: i64,
    wanted: PathBuf,
    free: bool,
}

fn taken(store: &Store, p: &std::path::Path) -> bool {
    p.exists() || store.id_of_path(p).ok().flatten().is_some()
}

/// Carries out `plans` (files first, then the rows as one step called `label(n)` for the `n` that moved).
fn apply(
    store: &Store,
    plans: Vec<Plan>,
    label: impl Fn(usize) -> String,
    r: &mut Report,
) -> R<()> {
    let (mut moves, mut done) = (Vec::new(), Vec::new());
    for p in plans {
        let mut dest = p.wanted.clone();
        if dest != p.from_path && taken(store, &dest) {
            if !p.free {
                r.failed.push(format!("{} is in the way", dest.display()));
                continue;
            }
            dest = paths::free_name(&dest, |q| taken(store, q));
        }
        if dest != p.from_path {
            if p.from_path.is_file() {
                if let Err(e) = paths::move_file(&p.from_path, &dest) {
                    r.failed.push(e);
                    continue;
                }
                done.push((p.from_path.clone(), dest.clone()));
            } else {
                dest = p.from_path.clone(); // no file to carry: the row moves on
            }
        }
        moves.push(Move {
            track: p.track,
            from_stage: p.from_stage,
            to_stage: p.to_stage,
            from_path: p.from_path,
            to_path: dest,
        });
    }
    if moves.is_empty() {
        return Ok(());
    }
    if let Err(e) = store.stage_moves(&label(moves.len()), &moves) {
        paths::move_back(&done);
        return Err(e);
    }
    r.moved = moves.len();
    r.files = moves.into_iter().map(|m| m.to_path).collect();
    Ok(())
}

/// Takes the Collection rows `ids` to stage `to`. With [`Config::move_files`] and a root, each file goes to
/// `<root>/<stage name>/<pattern>` ([`rules::target`]), never over another file.
pub fn move_tracks(store: &Store, ids: &[i64], to: i64, cfg: &Config) -> R<Report> {
    let stages = store.stages()?;
    let target = stages.iter().find(|s| s.id == to).ok_or("no such stage")?;
    let carry = cfg.move_files && !cfg.root.as_os_str().is_empty();
    let (mut r, mut plans) = (Report::default(), Vec::new());
    for &id in ids {
        let here = store.stage_of(id)?;
        let role = here
            .as_ref()
            .and_then(|(s, _)| stages.iter().find(|st| st.id == *s))
            .map(|s| s.role);
        let (Some((from_stage, from_path)), Some(role), Some(t)) =
            (here, role, store.collection_track(id)?)
        else {
            r.skipped += 1;
            continue;
        };
        if from_stage == to || role == Role::Collection {
            r.skipped += 1;
            continue;
        }
        let wanted = if carry {
            rules::target(&cfg.root, &target.name, &cfg.pattern, &t)
        } else {
            from_path.clone()
        };
        plans.push(Plan {
            track: id,
            from_stage,
            from_path,
            to_stage: to,
            wanted,
            free: true,
        });
    }
    let name = target.name.clone();
    let label = |n: usize| format!("Move {n} track{} to {name}", if n == 1 { "" } else { "s" });
    apply(store, plans, label, &mut r)?;
    Ok(r)
}

/// Undoes move batch `action` ([`Store::recent_batches`]). The newest step is a plain undo; an older one moves each of its
/// tracks back to the stage and file it came from, as a new step (so Edit > Undo can take that back), leaving alone a track
/// that is not where the batch put it any more.
pub fn undo_batch(store: &Store, action: i64) -> R<Report> {
    let batch = store
        .recent_batches(usize::MAX >> 1)?
        .into_iter()
        .find(|b| b.action == action)
        .ok_or("that move is not in the journal any more (or was undone)")?;
    let mut r = Report::default();
    if batch.next {
        store.undo()?;
        r.moved = batch.tracks;
        return Ok(r);
    }
    let mut plans = Vec::new();
    for m in store.batch_moves(action)?.into_iter().rev() {
        if store.stage_of(m.track)? == Some((m.to_stage, m.to_path.clone())) {
            plans.push(Plan {
                track: m.track,
                from_stage: m.to_stage,
                from_path: m.to_path,
                to_stage: m.from_stage,
                wanted: m.from_path,
                free: false,
            });
        } else {
            r.skipped += 1;
        }
    }
    let label = format!("Undo: {}", batch.label);
    apply(store, plans, |_| label.clone(), &mut r)?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{inbox::DEFAULT_PATTERN, library::folder};
    use std::path::Path;

    fn setup(name: &str) -> (PathBuf, Store, Vec<i64>) {
        let d = std::env::temp_dir().join(format!("openfader-moves-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("in")).unwrap();
        let s = Store::in_memory().unwrap();
        let first = s.stages().unwrap()[0].id;
        let ids = ["a.mp3", "b.mp3", "c.mp3"]
            .iter()
            .map(|n| {
                std::fs::write(d.join("in").join(n), n).unwrap();
                s.collection_put_staged(&[folder::read_track(&d.join("in").join(n))], first)
                    .unwrap()[0]
            })
            .collect();
        (d, s, ids)
    }

    fn at(s: &Store, id: i64) -> (String, PathBuf) {
        let (stage, path) = s.stage_of(id).unwrap().unwrap();
        (
            s.stages()
                .unwrap()
                .into_iter()
                .find(|x| x.id == stage)
                .unwrap()
                .name,
            path,
        )
    }

    fn cfg(root: &Path, pattern: &str) -> Config {
        Config {
            folders: Vec::new(),
            move_files: true,
            root: root.to_path_buf(),
            pattern: pattern.into(),
        }
    }

    #[test]
    fn a_move_renames_files_never_overwrites_and_undo_of_the_batch_restores_every_file_and_row() {
        let (d, s, ids) = setup("batch");
        let st = s.stages().unwrap();
        let lib = d.join("lib");
        // A file already sits where the first track is going: the conflict gets a suffix, nothing is overwritten.
        std::fs::create_dir_all(lib.join("REVIEW")).unwrap();
        std::fs::write(lib.join("REVIEW/a.mp3"), "someone else's").unwrap();
        let r = move_tracks(&s, &ids, st[1].id, &cfg(&lib, "{name}.{ext}")).unwrap();
        assert_eq!((r.moved, r.skipped, r.failed.len()), (3, 0, 0));
        assert_eq!(
            std::fs::read_to_string(lib.join("REVIEW/a.mp3")).unwrap(),
            "someone else's"
        );
        assert_eq!(
            at(&s, ids[0]),
            ("REVIEW".into(), lib.join("REVIEW/a (2).mp3"))
        );
        assert_eq!(
            std::fs::read_to_string(lib.join("REVIEW/a (2).mp3")).unwrap(),
            "a.mp3"
        );
        assert!(!d.join("in/a.mp3").exists() && lib.join("REVIEW/c.mp3").is_file());
        assert_eq!(s.stage_counts().unwrap()[&st[1].id], 3);
        // Moving them on to the Collection with a renaming pattern.
        let batches = || s.recent_batches(10).unwrap();
        let r = move_tracks(&s, &ids, st[2].id, &cfg(&lib, "{artist}/{title} copy")).unwrap();
        assert_eq!(r.moved, 3);
        assert_eq!(
            at(&s, ids[1]),
            (
                "COLLECTION".into(),
                lib.join("COLLECTION/Unknown Artist/b copy.mp3")
            )
        );
        assert_eq!(s.collection_tracks().unwrap().len(), 3);
        // The first batch is undone while a later one is on top: its files go back, the later batch stays.
        let first = batches()[1].action;
        assert_eq!(batches()[0].label, "Move 3 tracks to COLLECTION");
        let r = undo_batch(&s, first).unwrap();
        assert_eq!(
            (r.moved, r.skipped),
            (0, 3),
            "the tracks moved on since: nothing to take back from that batch"
        );
        // Undo the newest batch (Edit > Undo): files and rows are as after the first batch.
        assert_eq!(
            s.undo().unwrap().as_deref(),
            Some("Move 3 tracks to COLLECTION")
        );
        assert_eq!(at(&s, ids[1]), ("REVIEW".into(), lib.join("REVIEW/b.mp3")));
        assert!(
            lib.join("REVIEW/b.mp3").is_file()
                && !lib.join("COLLECTION/Unknown Artist/b copy.mp3").exists()
        );
        assert!(s.collection_tracks().unwrap().is_empty());
        // Undo the first batch the same way: every file and row is back where the INBOX had it.
        assert_eq!(
            s.undo().unwrap().as_deref(),
            Some("Move 3 tracks to REVIEW")
        );
        for (id, n) in ids.iter().zip(["a.mp3", "b.mp3", "c.mp3"]) {
            assert_eq!(at(&s, *id), ("INBOX".into(), d.join("in").join(n)));
            assert_eq!(std::fs::read_to_string(d.join("in").join(n)).unwrap(), n);
        }
        assert_eq!(
            std::fs::read_to_string(lib.join("REVIEW/a.mp3")).unwrap(),
            "someone else's"
        );
        // And redo carries them across again.
        assert!(s.redo().unwrap().is_some());
        assert_eq!(at(&s, ids[2]), ("REVIEW".into(), lib.join("REVIEW/c.mp3")));
        assert!(lib.join("REVIEW/c.mp3").is_file() && !d.join("in/c.mp3").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_older_batch_is_undone_by_a_new_step_that_edit_undo_can_take_back() {
        let (d, s, ids) = setup("older");
        let st = s.stages().unwrap();
        let lib = d.join("lib");
        move_tracks(&s, &ids[..1], st[1].id, &cfg(&lib, DEFAULT_PATTERN)).unwrap();
        move_tracks(&s, &ids[1..], st[1].id, &cfg(&lib, DEFAULT_PATTERN)).unwrap();
        let older = s.recent_batches(10).unwrap()[1].action;
        let r = undo_batch(&s, older).unwrap();
        assert_eq!((r.moved, r.skipped), (1, 0));
        assert_eq!(at(&s, ids[0]), ("INBOX".into(), d.join("in/a.mp3")));
        assert!(d.join("in/a.mp3").is_file());
        assert_eq!(at(&s, ids[1]).0, "REVIEW", "the later batch stays");
        assert_eq!(
            s.undo().unwrap().as_deref(),
            Some("Undo: Move 1 track to REVIEW")
        );
        assert_eq!(
            at(&s, ids[0]),
            (
                "REVIEW".into(),
                lib.join("REVIEW/Unknown Artist/Unknown Album/a.mp3")
            )
        );
        assert!(
            lib.join("REVIEW/Unknown Artist/Unknown Album/a.mp3")
                .is_file()
                && !d.join("in/a.mp3").exists()
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn without_file_moves_only_the_stage_changes_and_the_collection_is_final() {
        let (d, s, ids) = setup("rows");
        let st = s.stages().unwrap();
        let none = Config {
            move_files: false,
            ..Config::default()
        };
        let r = move_tracks(&s, &ids, st[1].id, &none).unwrap();
        assert_eq!(r.moved, 3);
        assert_eq!(at(&s, ids[0]), ("REVIEW".into(), d.join("in/a.mp3")));
        assert_eq!(
            move_tracks(&s, &ids, st[1].id, &none).unwrap().skipped,
            3,
            "already there"
        );
        move_tracks(&s, &ids[..1], st[2].id, &none).unwrap();
        assert_eq!(
            move_tracks(&s, &ids[..1], st[0].id, &none).unwrap().skipped,
            1,
            "the Collection keeps its tracks"
        );
        assert!(d.join("in/a.mp3").is_file());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
