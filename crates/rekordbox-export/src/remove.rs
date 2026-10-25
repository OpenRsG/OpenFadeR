//! Deleting from a device library (rekordbox: select in [Devices], press Delete; spec p105-106, p233): playlists and folders
//! with their subtree, tracks and play histories. Deleting a playlist keeps its tracks unless `delete_tracks` is set (Preferences >
//! DJ System > Device > Delete Tracks): then the tracks that no remaining playlist lists go too.
//!
//! The library files are written first, the audio, analysis and cover files afterwards, so a failure in between leaves a valid
//! library with some orphan files, never a track row without its file. Only paths under `Contents/` and `PIONEER/` are touched.

use super::pdb::{
    db::{Db, Tags},
    read,
};
use super::settings::DeviceConfig;
use super::{
    copy_db, onelibrary::image_variant, open_keyed, verify, write_libraries, ONE, OWNER, PDB,
};
use openfader_plugin::library::{
    commit::{self, Commit},
    RemoveReport, RemoveTarget,
};
use rekordbox_common::at;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// `rel` (a path as the library stores it) under `dest`, only below `Contents/` or `PIONEER/` and without `..`.
fn inside(dest: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim_start_matches('/');
    let ok = !rel.is_empty()
        && !rel.split('/').any(|c| c == ".." || c == ".")
        && (rel.starts_with("Contents/") || rel.starts_with("PIONEER/"));
    ok.then(|| dest.join(rel))
}

/// Ids of the node `root` and everything below it, in tree order.
fn subtree(db: &Db, root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        out.extend(db.nodes.iter().filter(|n| n.parent == parent).map(|n| n.id));
        i += 1;
    }
    out
}

/// Deletes what `target` names; `key` is the OneLibrary key. A playlist goes with `delete_tracks` also taking the tracks no
/// other playlist lists.
pub fn run(
    dest: &Path,
    target: &RemoveTarget,
    delete_tracks: bool,
    config: &DeviceConfig,
    key: &str,
) -> Result<RemoveReport, String> {
    match target {
        RemoveTarget::Playlist { path } => {
            let node = find_playlist(dest, path, config)?;
            playlist(dest, node, delete_tracks, config, key)
        }
        RemoveTarget::Track { file } => {
            let id = find_track(dest, file, config)?;
            tracks(dest, &[id], config, key)
        }
        RemoveTarget::Histories { names } => Ok(RemoveReport {
            histories: histories(dest, names, config, key)?,
            ..RemoveReport::default()
        }),
    }
}

/// Deletes the playlist or folder `node` (and what is under it); with `delete_tracks`, also the tracks no other playlist lists.
pub fn playlist(
    dest: &Path,
    node: u32,
    delete_tracks: bool,
    config: &DeviceConfig,
    key: &str,
) -> Result<RemoveReport, String> {
    commit::recover(dest)?;
    let (mut db, tags) = read::open(dest, config)?;
    if !db.nodes.iter().any(|n| n.id == node) {
        return Err(format!("playlist {node} is not on the device"));
    }
    let gone: HashSet<u32> = subtree(&db, node).into_iter().collect();
    let candidates: HashSet<u32> = db
        .nodes
        .iter()
        .filter(|n| gone.contains(&n.id))
        .flat_map(|n| n.tracks.iter().copied())
        .collect();
    db.nodes.retain(|n| !gone.contains(&n.id));
    let listed: HashSet<u32> = db
        .nodes
        .iter()
        .flat_map(|n| n.tracks.iter().copied())
        .collect();
    let tracks: Vec<u32> = if delete_tracks {
        candidates
            .into_iter()
            .filter(|t| !listed.contains(t))
            .collect()
    } else {
        Vec::new()
    };
    let mut r = finish(dest, db, tags, &tracks, key)?;
    r.playlists = gone.len();
    Ok(r)
}

/// Deletes tracks by device id: from every playlist and history, MyTag, and the files.
pub fn tracks(
    dest: &Path,
    ids: &[u32],
    config: &DeviceConfig,
    key: &str,
) -> Result<RemoveReport, String> {
    commit::recover(dest)?;
    let (db, tags) = read::open(dest, config)?;
    finish(dest, db, tags, ids, key)
}

/// The Device Library id of the playlist or folder at `path` (names from the top level down): ids of a OneLibrary written by
/// rekordbox differ from `export.pdb`'s, names do not.
pub fn find_playlist(dest: &Path, path: &[String], config: &DeviceConfig) -> Result<u32, String> {
    let (db, _) = read::open(dest, config)?;
    let mut parent = 0;
    for name in path {
        parent = db
            .nodes
            .iter()
            .find(|n| n.parent == parent && &n.name == name)
            .map(|n| n.id)
            .ok_or_else(|| format!("{} is not on the device", path.join(" / ")))?;
    }
    Ok(parent)
}

/// The Device Library id of the track whose audio file is `file` (a path under `dest`).
pub fn find_track(dest: &Path, file: &Path, config: &DeviceConfig) -> Result<u32, String> {
    let (db, _) = read::open(dest, config)?;
    let rel = file
        .strip_prefix(dest)
        .map_err(|_| format!("{} is not on this device", file.display()))?
        .to_string_lossy()
        .to_lowercase();
    db.tracks
        .iter()
        .find(|t| t.file_path.trim_start_matches('/').to_lowercase() == rel)
        .map(|t| t.id)
        .ok_or_else(|| format!("{} is not in the device library", file.display()))
}

/// Deletes the play histories called `names` from `export.pdb` and, when the device has a OneLibrary, from it too (rows of its
/// `history` / `history_content`; encrypted with `key`). Nothing else is rewritten, so the OneLibrary's own content is kept as is.
/// Returns how many histories `export.pdb` lost.
pub fn histories(
    dest: &Path,
    names: &[String],
    config: &DeviceConfig,
    key: &str,
) -> Result<usize, String> {
    let mut c = Commit::new(dest, OWNER)?;
    let (mut db, _) = read::open(dest, config)?;
    let before = db.histories.len();
    db.histories.retain(|h| !names.contains(&h.name));
    let one = dest.join(ONE);
    if one.is_file() {
        let staged = Commit::staged_path(&one);
        copy_db(&one, &staged)?;
        let edit = || -> Result<(), String> {
            let conn = open_keyed(&staged, key)?;
            for n in names {
                at(conn.execute("delete from history_content where history_id in (select history_id from history where name = ?1)", [n]), &one)?;
                at(
                    conn.execute("delete from history where name = ?1", [n]),
                    &one,
                )?;
            }
            Ok(())
        };
        edit()
            .and_then(|()| c.stage_built(&one))
            .inspect_err(|_| drop(std::fs::remove_file(&staged)))?;
    }
    let pdb = dest.join(PDB);
    c.stage(&pdb, &db.encode()?)?;
    verify::pdb_file(&Commit::staged_path(&pdb), &db)?;
    c.commit()?;
    Ok(before - db.histories.len())
}

fn finish(
    dest: &Path,
    mut db: Db,
    mut tags: Tags,
    ids: &[u32],
    key: &str,
) -> Result<RemoveReport, String> {
    let gone: HashSet<u32> = ids.iter().copied().collect();
    let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut db.tracks)
        .into_iter()
        .partition(|t| gone.contains(&t.id));
    db.tracks = kept;
    for n in &mut db.nodes {
        n.tracks.retain(|t| !gone.contains(t));
    }
    for h in &mut db.histories {
        h.tracks.retain(|t| !gone.contains(t));
    }
    tags.assigned.retain(|(t, ..)| !gone.contains(t));
    write_libraries(Commit::new(dest, OWNER)?, dest, &db, &tags, key, false)?;

    let mut r = RemoveReport {
        tracks: removed.len(),
        ..RemoveReport::default()
    };
    let still_used: HashSet<&str> = db
        .tracks
        .iter()
        .filter_map(|t| t.artwork.as_deref())
        .collect();
    for t in &removed {
        let mut files: Vec<PathBuf> = inside(dest, &t.file_path).into_iter().collect();
        files.extend(inside(dest, &t.analyze_path).and_then(|p| p.parent().map(Path::to_path_buf)));
        if let Some(a) = t.artwork.as_deref().filter(|a| !still_used.contains(a)) {
            let b = image_variant(a);
            files.extend(
                [
                    a.to_owned(),
                    a.replace(".jpg", "_m.jpg"),
                    b.clone(),
                    b.replace(".jpg", "_m.jpg"),
                ]
                .iter()
                .filter_map(|p| inside(dest, p)),
            );
        }
        for f in files {
            let res = if f.is_dir() {
                std::fs::remove_dir_all(&f)
            } else {
                std::fs::remove_file(&f)
            };
            match res {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    r.warnings.push(format!("{}: {e}", f.display()))
                }
                _ => {}
            }
        }
        // Empty <artist>/<album> folders go with their last track.
        if let Some(album) =
            inside(dest, &t.file_path).and_then(|p| p.parent().map(Path::to_path_buf))
        {
            let _ = std::fs::remove_dir(&album);
            if let Some(artist) = album.parent() {
                let _ = std::fs::remove_dir(artist);
            }
        }
    }
    Ok(r)
}
