//! The stick checker: what `device_check` reads on a device (never writing, never recovering a change) and the fix it offers.
//!
//! Problems (ids are `serato:<what>`): `pending_change` (a journal of this plugin is on the device), `unreadable_library`
//! (`database V2` does not start with `vrsn` or is cut short; no fix: Serato keeps no backup copy of it), `audio_missing` (audio
//! the database lists is not on the stick; the host copies it again from the collection), `crate_unreadable` and
//! `crate_unknown_track` (crates that are damaged or name a track the database does not hold; no fix: a crate cannot be
//! rebuilt from the database, it is the user's own list) and `leftover_files` (staged `*.ofnew` files without a journal; fix
//! `serato:clean_leftovers`).

use crate::tag::{self, markers};
use crate::write::write_err;
use crate::{crate_file, database, LIBRARY, OWNER};
use openfader_plugin::library::{
    commit::{self, State},
    ActionAnswer, CheckReport, LibraryState, Problem, Severity,
};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Fix id of the leftover files.
pub const CLEAN: &str = "serato:clean_leftovers";

/// Tracks whose tags `device_check` reads for the cue count. A cue lives only in the audio file's tags, so counting them all
/// reads one file per track from the stick.
// shortcut: the count covers the first CUE_SAMPLE tracks of the library; read them all (or count lazily) when a bigger stick
// needs the exact number.
pub const CUE_SAMPLE: usize = 400;

fn problem(
    id: &str,
    severity: Severity,
    cause: String,
    consequence: &str,
    action: &str,
) -> Problem {
    Problem {
        id: format!("serato:{id}"),
        severity,
        library: LIBRARY.into(),
        cause,
        consequence: consequence.into(),
        action: action.into(),
        ..Problem::default()
    }
}

/// The staged files of an interrupted write below `_Serato_`.
fn leftovers(root: &Path) -> Vec<PathBuf> {
    let serato = root.join(database::FILE);
    let serato = serato.parent().unwrap_or(root);
    let mut out = Vec::new();
    for dir in [serato.to_path_buf(), root.join(crate_file::DIR)] {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_file() && p.extension().is_some_and(|x| x == commit::STAGED) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// What the device holds, read-only.
pub fn check(root: &Path) -> Result<CheckReport, String> {
    let db = root.join(database::FILE);
    let meta = fs::metadata(&db).ok().filter(|m| m.is_file());
    let mut lib = LibraryState {
        name: LIBRARY.into(),
        present: meta.is_some(),
        modified: meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
        ..LibraryState::default()
    };
    let mut problems = Vec::new();
    let journal = commit::pending(root);
    if let Some(j) = journal.as_ref().filter(|j| j.owner == OWNER) {
        let (severity, consequence) = if j.state == State::Switching {
            (Severity::Warning, "The library on the stick may be part old and part new until the change is finished; Serato could show missing tracks or crates.")
        } else {
            (
                Severity::Info,
                "The library on the stick is still the old, complete one; nothing was lost.",
            )
        };
        problems.push(problem(
            "pending_change",
            severity,
            "A change to the Serato library of this stick was interrupted before it finished (the stick was pulled or the app stopped).".into(),
            consequence,
            "Connect the stick to OpenFader again: it finishes or undoes the change by itself before anything else is written. Your audio files are not touched.",
        ));
    }
    if lib.present {
        library(root, &db, &mut lib, &mut problems);
    }
    let left = leftovers(root);
    if journal.is_none() && !left.is_empty() {
        let mut p = problem(
            "leftover_files",
            Severity::Info,
            format!(
                "{} unfinished temporary file(s) of an interrupted write are left on this stick.",
                left.len()
            ),
            "Players ignore them; they only take space.",
            "FIX THE STICK removes them; your library and audio files are not touched.",
        );
        p.fix = Some(CLEAN.into());
        problems.push(p);
    }
    Ok(CheckReport {
        libraries: vec![lib],
        problems,
    })
}

/// Reads `database V2` and the crates: the counts, and what is missing or broken.
fn library(root: &Path, db: &Path, lib: &mut LibraryState, problems: &mut Vec<Problem>) {
    let paths = match fs::read(db)
        .map_err(|e| e.to_string())
        .and_then(|b| database::open(&b))
    {
        Ok(p) => p,
        Err(_) => {
            problems.push(problem(
                "unreadable_library",
                Severity::Unreadable,
                "The Serato library of this stick (_Serato_/database V2) cannot be read: it is damaged or not a Serato database.".into(),
                "Serato DJ would show an empty stick, with no tracks or crates.",
                "OpenFader cannot repair it because Serato keeps no backup copy of this file. Rename the file on the stick (for example to database V2 old) and export to the stick again; your audio files are not touched.",
            ));
            return;
        }
    };
    lib.readable = true;
    lib.tracks = paths.len();
    let norm: Vec<String> = paths.iter().map(|p| database::norm(p)).collect();
    let missing: Vec<String> = norm
        .iter()
        .filter(|p| !p.is_empty() && !root.join(p).is_file())
        .cloned()
        .collect();
    lib.cues = norm
        .iter()
        .take(CUE_SAMPLE)
        .map(|p| {
            tag::read(&root.join(p))
                .ok()
                .and_then(|s| s.markers2)
                .map_or(0, |m| markers::cues(&m).len())
        })
        .sum();

    let mut stems: Vec<(String, PathBuf)> = fs::read_dir(root.join(crate_file::DIR))
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let p = e.ok()?.path();
            let is_crate = p.extension()?.to_str()?.eq_ignore_ascii_case("crate");
            let stem = p.file_stem()?.to_str()?.to_string();
            (is_crate && !stem.starts_with('.')).then_some((stem, p))
        })
        .collect();
    stems.sort();
    let known: HashSet<&str> = norm.iter().map(String::as_str).collect();
    let (mut broken, mut unknown, mut first_unknown) = (Vec::new(), 0, None);
    for (stem, path) in &stems {
        let members = fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| crate_file::paths(&b));
        let Ok(members) = members else {
            broken.push(stem.as_str());
            continue;
        };
        let n = members
            .iter()
            .filter(|m| !known.contains(database::norm(m).as_str()))
            .count();
        if n > 0 {
            unknown += n;
            first_unknown.get_or_insert(stem.as_str());
        }
        let below = format!("{stem}%%");
        let folder = stems.iter().any(|(o, _)| o.starts_with(&below));
        lib.playlists += usize::from(!folder || !members.is_empty());
    }

    if let Some(first) = missing.first() {
        let mut p = problem(
            "audio_missing",
            Severity::Error,
            format!("{} track(s) of the {LIBRARY} are listed but their audio file is missing (for example {first}).", missing.len()),
            "Players list these tracks but cannot play them.",
            "Export to this stick again: the missing files are copied from your collection.",
        );
        p.files = missing;
        problems.push(p);
    }
    if let Some(first) = broken.first() {
        problems.push(problem(
            "crate_unreadable",
            Severity::Warning,
            format!("{} crate file(s) of this stick cannot be read (for example {first}).", broken.len()),
            "Serato would show those crates empty or not at all.",
            "Export the playlists to this stick again to write them anew; your audio files are not touched.",
        ));
    }
    if let Some(first) = first_unknown {
        problems.push(problem(
            "crate_unknown_track",
            Severity::Warning,
            format!("{unknown} entries in the crates of this stick name tracks that the Serato library does not list (for example in the crate {first})."),
            "Serato could show those crate entries as missing.",
            "Export the playlists to this stick again so the library and the crates match; your audio files are not touched.",
        ));
    }
}

/// Runs the fix `fix` of a problem of [`check`].
pub fn fix(root: &Path, fix: &str) -> Result<ActionAnswer, String> {
    if fix != CLEAN {
        return Err(format!("unknown fix {fix}"));
    }
    // The staged files of a change in progress are the new library: they are the host's to finish, never to delete.
    if commit::pending(root).is_some() {
        return Err("A change to this stick is waiting to be finished, so nothing was removed. Connect the stick to OpenFader again so it can finish the change, then check the stick again.".into());
    }
    let left = leftovers(root);
    for p in &left {
        fs::remove_file(p).map_err(|e| write_err(format!("{}: {e}", p.display())))?;
    }
    let message = if left.is_empty() {
        "There are no unfinished files on this stick; nothing was changed.".to_string()
    } else {
        format!("Removed {} unfinished temporary file(s).", left.len())
    };
    Ok(ActionAnswer {
        message,
        confirm: None,
    })
}
