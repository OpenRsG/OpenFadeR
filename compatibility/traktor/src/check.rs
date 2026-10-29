//! What the plugin reads back without writing: the read-back of a staged collection before it is switched in ([`verify`]), and
//! `device_check` / `device_fix` for a stick ([`check`], [`fix`]).
//!
//! Everything here reads the collection with the plugin's own reader ([`crate::load`], [`crate::read`]).

use crate::read::{key_of, lists, parts};
use crate::xml::El;
use crate::{load, modified, volume, FILE, LIBRARY, OWNER};
use openfader_plugin::library::{
    commit::{self, Commit, Recovered, State},
    ActionAnswer, CheckReport, LibraryState, Problem, Severity,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The fix of every problem that has one.
const CLEAN: &str = "traktor:clean_leftovers";

/// What a collection holds, as the reader sees it.
#[derive(Debug, PartialEq)]
pub struct Summary {
    /// Key (`VOLUME + DIR + FILE`) and cue count (loops included, the grid marker not) of every entry, in file order.
    pub entries: Vec<(String, usize)>,
    /// Id, whether it is a folder, and member keys of every folder and playlist, depth first.
    pub lists: Vec<(String, bool, Vec<String>)>,
}

impl Summary {
    pub fn of(nml: &El) -> Self {
        let entries = nml
            .child("COLLECTION")
            .map(|c| {
                c.kids
                    .iter()
                    .filter(|e| e.name == "ENTRY")
                    .filter_map(|e| {
                        let cues = e
                            .kids
                            .iter()
                            .filter(|k| k.name == "CUE_V2" && k.get("TYPE") != Some("4"))
                            .count();
                        Some((key_of(e)?, cues))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let lists = lists(nml)
            .into_iter()
            .map(|(p, keys)| (p.id, p.is_folder, keys))
            .collect();
        Self { entries, lists }
    }

    fn playlists(&self) -> usize {
        self.lists.iter().filter(|l| !l.1).count()
    }

    fn cues(&self) -> usize {
        self.entries.iter().map(|e| e.1).sum()
    }
}

const TAIL: &str = "A player would show a wrong or incomplete stick. Nothing on the stick was changed; try again, and use another stick if it happens again.";

/// Reads the staged collection `staged` back and compares it with `want`, the one that was serialised into it: the same
/// entries (keys, cue counts) and the same folders and playlists with the same members. `Err`: it does not, in cause,
/// consequence and action form.
pub fn verify(staged: &Path, want: &El) -> Result<(), String> {
    let want = Summary::of(want);
    let got = match load(staged) {
        Ok(nml) => Summary::of(&nml),
        Err(e) => {
            return Err(format!(
                "The {LIBRARY} just written cannot be read back ({e}). {TAIL}"
            ));
        }
    };
    if want == got {
        return Ok(());
    }
    let members = |s: &Summary| s.lists.iter().map(|l| l.2.len()).sum::<usize>();
    Err(if want.entries.len() != got.entries.len() {
        format!(
            "The {LIBRARY} just written lists {} tracks instead of {}. {TAIL}",
            got.entries.len(),
            want.entries.len()
        )
    } else if want.lists.len() != got.lists.len() {
        format!(
            "The {LIBRARY} just written holds {} playlists and folders instead of {}. {TAIL}",
            got.lists.len(),
            want.lists.len()
        )
    } else if members(&want) != members(&got) {
        format!(
            "The {LIBRARY} just written lists {} playlist entries instead of {}. {TAIL}",
            members(&got),
            members(&want)
        )
    } else {
        format!(
            "The {LIBRARY} just written does not match what was exported (its content differs). {TAIL}"
        )
    })
}

/// Audio files (relative to `root`, `/` separators) the entries of `nml` that belong to the stick at `root` point at and the stick
/// does not hold. An entry belongs to the stick when its `VOLUME` is the stick's or its folder is below `Contents/` (where
/// the audio of every format written there lives); other entries (a foreign collection merged in) are not checked.
fn missing(root: &Path, nml: &El) -> Vec<String> {
    let vol = volume(root);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let entries = nml.child("COLLECTION").into_iter().flat_map(|c| &c.kids);
    for e in entries.filter(|e| e.name == "ENTRY") {
        let Some(l) = e.child("LOCATION") else {
            continue;
        };
        let (Some(dir), Some(file)) = (l.get("DIR"), l.get("FILE")) else {
            continue;
        };
        let dir = parts(dir);
        if l.get("VOLUME") != Some(vol.as_str()) && dir.first() != Some(&"Contents") {
            continue;
        }
        let rel = dir
            .iter()
            .chain([&file])
            .copied()
            .collect::<Vec<_>>()
            .join("/");
        if !root.join(&rel).is_file() && seen.insert(rel.clone()) {
            out.push(rel);
        }
    }
    out
}

/// The files an export left beside the collection: its staged copy, and the temporary file older versions of the plugin wrote.
fn leftovers(root: &Path) -> Vec<PathBuf> {
    let file = root.join(FILE);
    [Commit::staged_path(&file), file.with_extension("nml.part")]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

/// The collection of the stick at `root` and what is wrong with it. Reads only: nothing is recovered, written or removed.
pub fn check(root: &Path) -> CheckReport {
    let path = root.join(FILE);
    let mut lib = LibraryState {
        name: LIBRARY.into(),
        present: path.is_file(),
        modified: modified(root),
        ..LibraryState::default()
    };
    let mut problems = Vec::new();
    let problem = |id: &str, severity, cause: String, consequence: &str, action: &str| Problem {
        id: format!("{OWNER}:{id}"),
        severity,
        library: LIBRARY.into(),
        cause,
        consequence: consequence.into(),
        action: action.into(),
        ..Problem::default()
    };
    if lib.present {
        match load(&path) {
            Ok(nml) => {
                let s = Summary::of(&nml);
                (lib.readable, lib.tracks, lib.playlists, lib.cues) =
                    (true, s.entries.len(), s.playlists(), s.cues());
                let files = missing(root, &nml);
                if !files.is_empty() {
                    problems.push(Problem {
                        files: files.clone(),
                        ..problem(
                            "missing_audio",
                            Severity::Error,
                            format!(
                                "{} track(s) of the {LIBRARY} are listed but their audio file is missing (for example {}).",
                                files.len(),
                                files[0]
                            ),
                            "Players list these tracks but cannot play them.",
                            "Export to this stick again: the missing files are copied from your collection.",
                        )
                    });
                }
            }
            Err(e) => problems.push(problem(
                "unreadable",
                Severity::Unreadable,
                format!("The {LIBRARY} of this stick cannot be read ({e})."),
                "Players that read it would show an empty stick or refuse the stick.",
                "Restore Traktor/collection.nml from a backup, or move it out of the Traktor folder and export again. Your audio files are not touched.",
            )),
        }
    }
    match commit::pending(root).filter(|j| j.owner == OWNER) {
        Some(j) if j.state == State::Switching => problems.push(Problem {
            fix: Some(CLEAN.into()),
            ..problem(
                "interrupted",
                Severity::Warning,
                "An export to this stick was interrupted while it switched to the new Traktor collection.".into(),
                "The stick still holds the collection it had before; the new one is waiting next to it.",
                "FIX THE STICK finishes the change in a few seconds; your audio files are not touched.",
            )
        }),
        Some(_) => problems.push(Problem {
            fix: Some(CLEAN.into()),
            ..problem(
                "interrupted",
                Severity::Info,
                "An export to this stick was interrupted before it changed anything.".into(),
                "The stick holds the Traktor collection it had before; unfinished files are left on it.",
                "FIX THE STICK removes them; your library and audio files are not touched.",
            )
        }),
        None if !leftovers(root).is_empty() => problems.push(Problem {
            fix: Some(CLEAN.into()),
            ..problem(
                "leftovers",
                Severity::Info,
                format!(
                    "{} unfinished temporary file(s) of an interrupted write are left on this stick.",
                    leftovers(root).len()
                ),
                "Players ignore them; they only take space.",
                "FIX THE STICK removes them; your library and audio files are not touched.",
            )
        }),
        None => {}
    }
    CheckReport {
        libraries: vec![lib],
        problems,
    }
}

/// Runs the fix `id` of a [`Problem::fix`]: an interrupted change is finished or undone, then the unfinished files go.
pub fn fix(root: &Path, id: &str) -> Result<ActionAnswer, String> {
    if id != CLEAN {
        return Err(format!("unknown fix {id}"));
    }
    let recovered = commit::recover(root).map_err(|e| {
        format!("The change an earlier export left unfinished on this stick could not be completed ({e}). Nothing was changed; check that the stick is not write-protected and try again.")
    })?;
    let files = leftovers(root);
    for p in &files {
        std::fs::remove_file(p).map_err(|e| {
            format!("An unfinished file next to the Traktor collection could not be removed ({}: {e}). It only takes space on the stick; check that the stick is not write-protected and try again.", p.display())
        })?;
    }
    let message = match recovered {
        Some(Recovered::Finished { .. }) => "Finished the change that was interrupted: the stick holds the new Traktor collection. Your audio files are not touched.".to_owned(),
        Some(Recovered::RolledBack { .. }) => "Undid the change that was interrupted: the stick holds the Traktor collection it had before. Your audio files are not touched.".to_owned(),
        None if !files.is_empty() => format!("Removed {} unfinished temporary file(s).", files.len()),
        None => "There was nothing to clean up. Your audio files are not touched.".to_owned(),
    };
    Ok(ActionAnswer {
        message,
        ..ActionAnswer::default()
    })
}
