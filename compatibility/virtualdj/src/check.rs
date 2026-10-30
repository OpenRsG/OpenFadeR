//! What the plugin reads back from a stick: the read-back of a staged export ([`verify`]), `device_check` ([`check`]) and its fix
//! ([`fix`]). Nothing here writes to the stick except the fix, and the fix writes no library file.

use crate::read::Reader;
use crate::{Composed, DATABASE, LIBRARY, OWNER, ROOT};
use openfader_plugin::library::commit::{self, Commit, State};
use openfader_plugin::library::{
    ActionAnswer, CheckReport, LibraryState, Problem, Severity, Source,
};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

fn bad(detail: &str) -> String {
    format!(
        "The VirtualDJ library written to the stick did not read back as written ({detail}). Nothing was switched, so the stick still holds its old library and your audio files are not touched. Export again; if it fails again, check the stick for errors."
    )
}

/// Reads the staged files of `commit` back with [`Reader`] (from a scratch copy that has the layout of the stick) and compares
/// what it finds with what the export put in them: songs, cues and loops per song, lists and the songs of each list.
pub(crate) fn verify(root: &Path, commit: &Commit, want: &Composed) -> Result<(), String> {
    let tmp = tempfile::Builder::new()
        .prefix("openfader-virtualdj-")
        .tempdir()
        .map_err(|e| bad(&e.to_string()))?;
    let mut have_db = false;
    for (target, staged) in commit.staged() {
        let rel = target.strip_prefix(root).unwrap_or(target);
        let to = tmp.path().join(rel);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| bad(&e.to_string()))?;
        }
        std::fs::copy(staged, &to).map_err(|e| bad(&format!("{}: {e}", staged.display())))?;
        have_db |= target == root.join(DATABASE);
    }
    if !have_db {
        let to = tmp.path().join(DATABASE);
        let stub = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<{ROOT} Version=\"8.5\">\n</{ROOT}>\n"
        );
        std::fs::create_dir_all(to.parent().unwrap_or(tmp.path()))
            .and_then(|()| std::fs::write(&to, stub))
            .map_err(|e| bad(&e.to_string()))?;
    }
    let r = Reader::open(tmp.path()).map_err(|e| bad(&e))?;
    if have_db && r.song_count() != want.songs {
        return Err(bad(&format!(
            "{} songs instead of {}",
            r.song_count(),
            want.songs
        )));
    }
    for (path, n) in &want.cues {
        let got = r.cue_count(path);
        if got != *n {
            return Err(bad(&format!("{path}: {got} cues instead of {n}")));
        }
    }
    let lists = r.playlists().map_err(|e| bad(&e))?;
    let found = lists.iter().filter(|p| !p.is_folder).count();
    if found != want.lists.len() {
        return Err(bad(&format!(
            "{found} lists instead of {}",
            want.lists.len()
        )));
    }
    for (file, n) in &want.lists {
        let there = tmp.path().join(file.strip_prefix(root).unwrap_or(file));
        let id = r
            .list_files()
            .find(|(_, f)| *f == there)
            .map(|(id, _)| id.to_owned())
            .ok_or_else(|| bad(&format!("{}: list missing", file.display())))?;
        let got = r.tracks(Some(&id)).map_err(|e| bad(&e))?.len();
        if got != *n {
            return Err(bad(&format!(
                "{}: {got} songs instead of {n}",
                file.display()
            )));
        }
    }
    Ok(())
}

/// `*.ofnew` files below the `VirtualDJ` folder: staged files nothing will finish.
fn leftovers(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 8 {
                leftovers(&p, depth + 1, out);
            }
        } else if p.extension().is_some_and(|x| x == commit::STAGED) {
            out.push(p);
        }
    }
}

fn rel(root: &Path, p: &Path) -> String {
    let r = p.strip_prefix(root).unwrap_or(p);
    r.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The state of the VirtualDJ database of the stick at `root`; reads only.
pub(crate) fn check(root: &Path) -> CheckReport {
    let db = root.join(DATABASE);
    let meta = std::fs::metadata(&db).ok();
    let mut lib = LibraryState {
        name: LIBRARY.into(),
        present: meta.is_some(),
        modified: meta.and_then(|m| {
            let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some(t.as_secs() as i64)
        }),
        ..Default::default()
    };
    let mut problems = Vec::new();
    let problem = |id: &str, severity, cause: String, consequence: &str, action: &str| Problem {
        id: format!("virtualdj:{id}"),
        severity,
        library: LIBRARY.into(),
        cause,
        consequence: consequence.into(),
        action: action.into(),
        ..Default::default()
    };
    if lib.present {
        match Reader::open(root) {
            Err(e) => problems.push(problem(
                "unreadable",
                Severity::Unreadable,
                format!("The VirtualDJ database of this stick cannot be read ({e})."),
                "VirtualDJ would show no tracks from this stick.",
                "Restore VirtualDJ/database.xml from a backup (VirtualDJ keeps copies in the Backup folder of its computer), or export the collection to the stick again after moving the damaged file away. Your audio files are not touched.",
            )),
            Ok(r) => {
                lib.readable = true;
                let tracks = r.tracks(None).unwrap_or_default();
                lib.tracks = tracks.len();
                lib.cues = tracks
                    .iter()
                    .map(|t| r.analysis(t).map_or(0, |a| a.cues.len()))
                    .sum();
                let missing: Vec<String> = tracks
                    .iter()
                    .filter_map(|t| t.file.as_deref())
                    .filter(|f| f.strip_prefix(root).is_ok() && !f.is_file())
                    .map(|f| rel(root, f))
                    .collect();
                if !missing.is_empty() {
                    let mut p = problem(
                        "missing_audio",
                        Severity::Error,
                        format!(
                            "The VirtualDJ database of this stick lists {} tracks whose audio file is not on the stick.",
                            missing.len()
                        ),
                        "VirtualDJ shows them as missing, so they cannot be played at a gig.",
                        "Export the collection to this stick again: the missing audio files are copied again from the collection.",
                    );
                    p.files = missing;
                    problems.push(p);
                }
                let lists = r.playlists().unwrap_or_default();
                lib.playlists = lists.iter().filter(|p| !p.is_folder).count();
                let broken: Vec<&str> = lists
                    .iter()
                    .filter(|p| !p.is_folder && r.tracks(Some(&p.id)).is_err())
                    .map(|p| p.name.as_str())
                    .collect();
                if let Some(first) = broken.first() {
                    problems.push(problem(
                        "list_unreadable",
                        Severity::Warning,
                        format!(
                            "{} lists of this stick cannot be read, for example {first}.",
                            broken.len()
                        ),
                        "VirtualDJ would skip them, so those playlists would be missing at a gig.",
                        "Export the collection to this stick again: the lists are written anew. Your audio files are not touched.",
                    ));
                }
            }
        }
    }
    let journal = commit::pending(root).filter(|j| j.owner == OWNER);
    if let Some(j) = journal {
        let (severity, consequence) = if j.state == State::Switching {
            (
                Severity::Warning,
                "The library files of the stick may be half old and half new until the change is finished.",
            )
        } else {
            (
                Severity::Info,
                "The library on the stick is still the old one; the unfinished change is dropped.",
            )
        };
        problems.push(problem(
            "pending",
            severity,
            "An export to this stick was interrupted before it finished.".into(),
            consequence,
            "Plug the stick in to OpenFader again: it finishes or undoes the change itself before the next export. Your audio files are not touched.",
        ));
    } else {
        let mut found = Vec::new();
        leftovers(&root.join("VirtualDJ"), 0, &mut found);
        if !found.is_empty() {
            let mut p = problem(
                "leftovers",
                Severity::Info,
                format!(
                    "An interrupted export left {} temporary files on this stick.",
                    found.len()
                ),
                "They take a little room and are not used; VirtualDJ ignores them.",
                "CLEAN UP removes them. Your library and audio files are not touched.",
            );
            p.fix = Some("virtualdj:clean_leftovers".into());
            problems.push(p);
        }
    }
    CheckReport {
        libraries: vec![lib],
        problems,
    }
}

/// Runs the fix a problem of [`check`] named. `clean_leftovers` removes staged files; no library file is written.
pub(crate) fn fix(root: &Path, fix: &str) -> Result<ActionAnswer, String> {
    if fix != "virtualdj:clean_leftovers" {
        return Err(format!("{fix}: this plugin has no such fix"));
    }
    if root.join(commit::JOURNAL).exists() {
        return Err("A change of this stick is still unfinished, so its temporary files are still needed. Nothing was removed; plug the stick in to OpenFader again so it finishes the change, then check the stick once more.".into());
    }
    let mut found = Vec::new();
    leftovers(&root.join("VirtualDJ"), 0, &mut found);
    for f in &found {
        std::fs::remove_file(f).map_err(|e| format!("{}: {e}", f.display()))?;
    }
    Ok(ActionAnswer {
        message: format!(
            "Removed {} leftover files of an interrupted export. Your library and audio files were not touched.",
            found.len()
        ),
        confirm: None,
    })
}
