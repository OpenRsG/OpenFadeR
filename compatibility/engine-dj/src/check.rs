//! `device_check` and `device_fix`: what a stick holds of the Engine library, found without writing anything (the library is opened
//! read-only and `immutable`, no journal is recovered), and the repairs that need no collection: removing what an interrupted
//! change left behind, and finishing it. Audio files the library lists but the stick lacks are reported with their paths; the host
//! copies them again from the collection.

use crate::{database, reader, schema, LIBRARY, OWNER};
use openfader_plugin::library::{
    commit::{self, Commit, Recovered},
    ActionAnswer, CheckReport, LibraryState, Problem, Severity,
};
use std::path::{Path, PathBuf};

pub const CLEAN_LEFTOVERS: &str = "engine-dj:clean_leftovers";
pub const REMOVE_SIDECARS: &str = "engine-dj:remove_sidecars";
pub const FINISH_CHANGE: &str = "engine-dj:finish_change";

fn problem(
    id: &str,
    severity: Severity,
    cause: String,
    consequence: &str,
    action: &str,
) -> Problem {
    Problem {
        id: format!("engine-dj:{id}"),
        severity,
        library: LIBRARY.into(),
        cause,
        consequence: consequence.into(),
        action: action.into(),
        ..Default::default()
    }
}

fn mtime(p: &Path) -> Option<i64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// The `-wal` and `-shm` files next to `db` that are older than it: they belong to the file the library replaced, and a reader
/// would replay them onto the new one. A newer one is a player at work and is left alone.
fn stale_sidecars(db: &Path) -> Vec<PathBuf> {
    let Some(at) = mtime(db) else { return vec![] };
    ["-wal", "-shm"]
        .iter()
        .map(|s| schema::sidecar(db, s))
        .filter(|p| mtime(p).is_some_and(|m| m < at))
        .collect()
}

/// What the library holds, or why it cannot be read.
struct Found {
    tracks: usize,
    playlists: usize,
    cues: usize,
    missing: Vec<String>,
    damaged: Option<String>,
}

fn read_library(root: &Path, db: &Path) -> Result<Found, String> {
    let conn = schema::open_immutable(db).map_err(|e| e.to_string())?;
    let (major, minor, patch): (i64, i64, i64) = conn
        .query_row(
            "SELECT schemaVersionMajor, schemaVersionMinor, schemaVersionPatch FROM Information",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    let version = (major, minor, patch);
    if !schema::EXTENDED.contains(&version) {
        return Err(format!("schema version {}", schema::text(version)));
    }
    let quick: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    drop(conn);
    let src = reader::open_file(db, true)?;
    let tracks = src.tracks(None)?;
    let mut found = Found {
        tracks: tracks.len(),
        playlists: src.playlists()?.iter().filter(|p| !p.is_folder).count(),
        cues: 0,
        missing: Vec::new(),
        damaged: (quick != "ok").then_some(quick),
    };
    for t in &tracks {
        found.cues += src.analysis(t).map_or(0, |a| a.cues.len());
        if let Some(f) = t.file.as_deref().filter(|f| !f.exists()) {
            if let Ok(rel) = f.strip_prefix(root) {
                let rel: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
                found.missing.push(rel.join("/"));
            }
        }
    }
    Ok(found)
}

pub fn check(root: &Path) -> Result<CheckReport, String> {
    let db = database(root);
    let old_layout = root.join(LIBRARY).join("m.db").is_file();
    let mut state = LibraryState {
        name: LIBRARY.into(),
        present: db.is_file() || old_layout,
        modified: mtime(&db),
        ..Default::default()
    };
    let mut problems = Vec::new();

    if commit::pending(root).is_some_and(|j| j.owner == OWNER) {
        let mut p = problem(
            "change_pending",
            Severity::Warning,
            "An export to this stick was interrupted before it finished.".into(),
            "The library may lack the last changes until the export is completed or undone; OpenFader does that the next time it opens this stick.",
            "FIX THE STICK completes or undoes the export now; your audio files are not touched.",
        );
        p.fix = Some(FINISH_CHANGE.into());
        problems.push(p);
    }

    if db.is_file() {
        match read_library(root, &db) {
            Ok(f) => {
                state.readable = true;
                (state.tracks, state.playlists, state.cues) = (f.tracks, f.playlists, f.cues);
                if let Some(d) = f.damaged {
                    problems.push(problem(
                        "damaged",
                        Severity::Error,
                        format!("The Engine library of this stick is damaged ({d})."),
                        "Players may show wrong or missing tracks, or refuse the stick.",
                        "Format the stick and export your collection to it again; your collection and the original audio files are not touched.",
                    ));
                }
                if !f.missing.is_empty() {
                    let mut p = problem(
                        "missing_audio",
                        Severity::Error,
                        format!(
                            "The Engine library lists audio files that are missing from the stick ({}).",
                            f.missing.len()
                        ),
                        "Those tracks cannot be played from this stick.",
                        "Export them again from your collection: OpenFader copies the missing audio files back and the library is not changed.",
                    );
                    p.files = f.missing;
                    problems.push(p);
                }
            }
            Err(e) => problems.push(problem(
                "unreadable",
                Severity::Unreadable,
                format!("The Engine library of this stick cannot be read ({e})."),
                "Players would show an empty or damaged stick, and nothing can be added to it from here.",
                "Format the stick and export your collection to it again, or open the stick in Engine DJ; your audio files are not touched.",
            )),
        }
    } else if old_layout {
        problems.push(problem(
            "old_layout",
            Severity::Unreadable,
            "The Engine library of this stick uses the old two-file format.".into(),
            "This plugin cannot read or extend it, so the stick cannot be checked or filled from here.",
            "Open the stick in Engine DJ once to convert it; your audio files are not touched.",
        ));
    }

    let stale = stale_sidecars(&db);
    if !stale.is_empty() {
        let names: Vec<String> = stale
            .iter()
            .filter_map(|p| Some(p.file_name()?.to_string_lossy().into_owned()))
            .collect();
        let mut p = problem(
            "stale_sidecar",
            Severity::Warning,
            format!(
                "Leftover journal files ({}) sit next to the Engine library and are older than it.",
                names.join(", ")
            ),
            "They belong to an earlier version of the library; a program that applies them could show wrong or missing tracks.",
            "FIX THE STICK removes these files; the library and your audio files are not touched.",
        );
        p.fix = Some(REMOVE_SIDECARS.into());
        problems.push(p);
    }

    if commit::pending(root).is_none() && Commit::staged_path(&db).exists() {
        let mut p = problem(
            "leftover",
            Severity::Info,
            "A half-written copy of the Engine library (m.db.ofnew) was left on the stick by an interrupted export.".into(),
            "The library in use is not affected; the copy only takes up space.",
            "FIX THE STICK removes it; your library and audio files are not touched.",
        );
        p.fix = Some(CLEAN_LEFTOVERS.into());
        problems.push(p);
    }

    Ok(CheckReport {
        libraries: vec![state],
        problems,
    })
}

fn answer(message: &str) -> Result<ActionAnswer, String> {
    Ok(ActionAnswer {
        message: message.into(),
        ..Default::default()
    })
}

pub fn fix(root: &Path, fix: &str) -> Result<ActionAnswer, String> {
    let db = database(root);
    match fix {
        CLEAN_LEFTOVERS => {
            if commit::pending(root).is_some() {
                return Err("An export is still pending on this stick, so its files were kept. Nothing was removed; complete the export first.".into());
            }
            let _ = std::fs::remove_file(Commit::staged_path(&db));
            answer("The half-written copy of the library was removed. Your library and audio files were not touched.")
        }
        REMOVE_SIDECARS => {
            for p in stale_sidecars(&db) {
                std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            answer("The leftover journal files were removed. Your library and audio files were not touched.")
        }
        FINISH_CHANGE => answer(match commit::recover(root)? {
            Some(Recovered::Finished { .. }) => {
                "The interrupted export was completed: the new library is in place."
            }
            Some(Recovered::RolledBack { .. }) => {
                "The interrupted export was undone: the library is as it was before."
            }
            None => "No interrupted export was found. Nothing was changed.",
        }),
        other => Err(format!(
            "This plugin has no fix called {other}. Nothing was changed."
        )),
    }
}
