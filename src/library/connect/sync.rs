//! Source → Collection. Per track the source says, a hash (`sig`) of its fields and of the cues and beat grid it holds is
//! compared with the one of the last sync (`source_map`): new tracks are added, a changed one is updated by the
//! connection's rule (`wins`), an unchanged one is left alone (so edits made here stay). A track that left the source only
//! loses its map row: the Collection never deletes a track by itself. The playlists of the source sit under a folder named
//! after the connection and are rebuilt in place. Everything is written as the journal's system ([`Store::system`]): a sync
//! is not an undo step.

use super::{foreign_id, Connection, Wins, ROOT};
use crate::analysis::cache::fnv1a;
use crate::library::{is_history, Library, Playlist, Track};
use crate::store::{self, NodeKind, Store, R};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// What a sync changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub added: usize,
    pub updated: usize,
    /// Tracks that left the source (they stay in the Collection).
    pub removed: usize,
    /// Playlists and folders made or refilled.
    pub playlists: usize,
}

impl Report {
    pub fn text(&self) -> String {
        format!(
            "{} added, {} updated, {} left the source, {} playlist{} synced",
            self.added,
            self.updated,
            self.removed,
            self.playlists,
            if self.playlists == 1 { "" } else { "s" }
        )
    }
}

/// Hash of everything the source says about `t` that the Collection keeps (the file is the key and is not in it).
fn sig(t: &Track, a: Option<&crate::analysis::Analysis>) -> i64 {
    let mut b: Vec<u8> = Vec::new();
    for s in [
        &t.title,
        &t.artist,
        &t.album,
        &t.key,
        &t.genre,
        &t.label,
        &t.comment,
        &t.date_added,
    ] {
        b.extend(s.as_bytes());
        b.push(0);
    }
    b.extend(t.analysis_ref.as_deref().unwrap_or("").as_bytes());
    b.extend(t.bpm_x100.to_le_bytes());
    b.extend(t.length_s.to_le_bytes());
    b.extend(t.bitrate.to_le_bytes());
    b.extend(t.sample_rate.to_le_bytes());
    b.push(t.rating);
    if let Some(a) = a {
        for c in &a.cues {
            b.extend(c.hot_cue.to_le_bytes());
            b.extend(c.time_ms.to_le_bytes());
            b.extend(c.loop_end_ms.unwrap_or(0).to_le_bytes());
            b.extend(c.color.unwrap_or([0; 3]));
            b.extend(c.comment.as_deref().unwrap_or("").as_bytes());
        }
        b.push(1);
        for g in &a.beats {
            b.extend(g.time_ms.to_le_bytes());
            b.extend(g.bpm_x100.to_le_bytes());
            b.extend(g.bar_pos.to_le_bytes());
        }
    }
    fnv1a(b) as i64
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The source's cues and grid become our edits where we have none (cues CUE Analysis made count as none).
fn import_edits(
    store: &Store,
    key: &str,
    t: &Track,
    a: Option<&crate::analysis::Analysis>,
    force: bool,
) -> R<()> {
    let ours = store.edits(key)?;
    if t.rating > 0 && (force || ours.rating.is_none()) {
        store.set_rating(key, Some(t.rating))?;
    } else if force && t.rating == 0 {
        store.set_rating(key, None)?;
    }
    // Forced (the source wins): what the source lacks clears ours too. No analysis (`None`) is unknown, not empty.
    if force || (!t.comment.is_empty() && ours.comment.is_none()) {
        store.set_comment(key, Some(&t.comment))?;
    }
    if let Some(a) = a {
        if force || (!a.cues.is_empty() && (ours.cues.is_none() || store.cues_are_auto(key)?)) {
            store.set_cues(key, Some(&a.cues))?;
        }
        if force || (!a.beats.is_empty() && ours.grid.is_none()) {
            store.set_grid(key, &a.beats, &a.beats)?;
        }
    }
    Ok(())
}

/// Opens the source of `conn` and syncs it into `store`; `progress(done, total)` per track.
pub fn sync(store: &Store, conn: &Connection, progress: &dyn Fn(usize, usize)) -> R<Report> {
    let stamp = super::stamp(conn);
    let src = super::open(conn)?;
    run(store, conn, &*src, &stamp, progress)
}

/// Syncs `src` (the opened source of `conn`) into `store`; `stamp` is the source's state read before it was opened.
pub fn run(
    store: &Store,
    conn: &Connection,
    src: &dyn Library,
    stamp: &str,
    progress: &dyn Fn(usize, usize),
) -> R<Report> {
    store.system(|| run_inner(store, conn, src, stamp, progress))
}

fn run_inner(
    store: &Store,
    conn: &Connection,
    src: &dyn Library,
    stamp: &str,
    progress: &dyn Fn(usize, usize),
) -> R<Report> {
    let tracks = src.tracks(None)?;
    let tree = src.playlists()?;
    let reader = src.analysis_reader();
    let map = store.source_map(conn.id)?;
    let hidden = store.removed_ids()?;
    let moved = if conn.last_sync == 0 {
        store.relocations()?
    } else {
        HashMap::new()
    };
    // Newest wins: when the user last edited each track since the last write-back / sync.
    let edited_at: HashMap<String, i64> = if conn.wins == Wins::Newest {
        let mut m = store.last_edits_since(conn.last_seq)?;
        for (_, k) in store.changed_since(conn.last_seq)? {
            if let Some(k) = k {
                m.entry(k).or_insert(i64::MAX);
            }
        }
        m
    } else {
        Default::default()
    };
    let lib_mtime = if conn.plugin.is_empty() {
        0
    } else {
        mtime(&conn.path)
    };
    let mut r = Report::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut fresh: Vec<(usize, String, i64, Track)> = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        progress(i, tracks.len());
        let Some(fid) = foreign_id(t) else { continue };
        if hidden.contains(&fid) {
            continue;
        }
        seen.insert(fid.clone());
        let analysis = reader.as_ref().and_then(|f| f(t));
        let sig = sig(t, analysis.as_ref());
        match map.get(&fid) {
            Some(&(_, old)) if old == sig => {}
            None => {
                let mut row = Track {
                    id: None,
                    date_added: if t.date_added.is_empty() {
                        crate::library::today()
                    } else {
                        t.date_added.clone()
                    },
                    ..t.clone()
                };
                if let Some(f) = row.file.as_ref().and_then(|f| moved.get(f)) {
                    row.file = Some(f.clone());
                }
                // A file the Collection already holds is linked, not overwritten (its edits and tags stay ours).
                let known = match &row.file {
                    Some(f) => store.id_of_path(f)?,
                    None => None,
                };
                match known {
                    Some(id) => {
                        import_edits(store, &store::id_key(id), t, analysis.as_ref(), false)?;
                        store.map_put(conn.id, &fid, id, sig)?;
                    }
                    None => fresh.push((i, fid, sig, row)),
                }
            }
            Some(&(id, _)) => {
                let key = store::id_key(id);
                let src_time = if conn.plugin.is_empty() {
                    t.file.as_deref().map_or(0, mtime)
                } else {
                    lib_mtime
                };
                let take = match conn.wins {
                    Wins::Source => true,
                    Wins::Ours => false,
                    // an edit without a time (undo) counts as newest
                    Wins::Newest => edited_at.get(&key).is_none_or(|&at| src_time > at),
                };
                if take {
                    if let Some(ours) = store.collection_track(id)? {
                        // Our path stays: the user may have relocated the file since.
                        let row = Track {
                            id: Some(id),
                            file: ours.file,
                            date_added: ours.date_added,
                            ..t.clone()
                        };
                        store.collection_upsert(&row, Some(id))?;
                        import_edits(store, &key, t, analysis.as_ref(), true)?;
                        r.updated += 1;
                    }
                }
                store.map_put(conn.id, &fid, id, sig)?;
            }
        }
    }
    // New rows go in as one batch: tracks sharing a legacy edit key all adopt its edits before it is removed.
    // ponytail: the analysis of a new track is read again below (not held for the whole source); keep it if that read is slow.
    let ids =
        store.collection_upsert_many(&fresh.iter().map(|f| (None, &f.3)).collect::<Vec<_>>())?;
    r.added += ids.len();
    for ((i, fid, sig, _), id) in fresh.iter().zip(ids) {
        let t = &tracks[*i];
        let analysis = reader.as_ref().and_then(|f| f(t));
        import_edits(store, &store::id_key(id), t, analysis.as_ref(), false)?;
        store.map_put(conn.id, fid, id, *sig)?;
    }
    progress(tracks.len(), tracks.len());
    let gone: Vec<&String> = map.keys().filter(|k| !seen.contains(*k)).collect();
    r.removed = gone.len();
    store.map_drop(conn.id, &gone)?;
    r.playlists = playlists(store, conn, src, &tree)?;
    // Import-only: nothing waits to be written back, so the journal position moves on.
    let seq = (conn.mode == super::Mode::Import)
        .then(|| store.journal_seq())
        .transpose()?;
    store.connection_synced(conn.id, stamp, now(), seq)?;
    Ok(r)
}

/// `tree` under a folder named after the connection: made when missing (not for a source with no playlists), renamed and
/// refilled when changed, pruned when the source dropped them. Returns how many nodes were made or changed.
fn playlists(store: &Store, conn: &Connection, src: &dyn Library, tree: &[Playlist]) -> R<usize> {
    let tree: Vec<&Playlist> = tree.iter().filter(|p| !is_history(&p.id)).collect();
    let by_node = store.source_nodes(conn.id)?;
    if tree.is_empty() && !by_node.contains_key(ROOT) {
        return Ok(0);
    }
    let ids = store.source_map(conn.id)?;
    let mut n = 0;
    let root = match by_node.get(ROOT) {
        Some(&node) => node,
        None => {
            let node = store.add_node(None, &conn.label, NodeKind::Folder)?;
            store.node_map_put(conn.id, ROOT, node)?;
            n += 1;
            node
        }
    };
    let nodes = store.nodes()?;
    let name_of = |id: i64| nodes.iter().find(|x| x.id == id).map(|x| x.name.clone());
    if name_of(root).is_some_and(|x| x != conn.label) {
        store.rename_node(root, &conn.label)?;
    }
    let mut parents: Vec<i64> = vec![root];
    // How many children each parent has got so far: the position the next one takes.
    let mut next: HashMap<i64, i64> = HashMap::new();
    for p in &tree {
        parents.truncate(p.depth + 1);
        let parent = parents[p.depth.min(parents.len() - 1)];
        let kind = if p.is_folder {
            NodeKind::Folder
        } else {
            NodeKind::Playlist
        };
        let slot = next.entry(parent).or_insert(0);
        let pos = *slot;
        *slot += 1;
        let node = match by_node.get(&p.id) {
            Some(&node) => {
                if name_of(node).is_some_and(|x| x != p.name) {
                    store.rename_node(node, &p.name)?;
                    n += 1;
                }
                // Moved in the source: follow it before the folders it left are pruned (they take their children along).
                if nodes
                    .iter()
                    .find(|x| x.id == node)
                    .is_some_and(|x| x.parent != Some(parent) || x.pos != pos)
                {
                    store.move_node(node, Some(parent), pos)?;
                    n += 1;
                }
                node
            }
            None => {
                let node = store.add_node(Some(parent), &p.name, kind)?;
                store.node_map_put(conn.id, &p.id, node)?;
                n += 1;
                node
            }
        };
        if p.is_folder {
            parents.push(node);
            continue;
        }
        let keys: Vec<String> = src
            .tracks(Some(&p.id))?
            .iter()
            .filter_map(foreign_id)
            .filter_map(|f| ids.get(&f))
            .map(|&(id, _)| store::id_key(id))
            .collect();
        if store.entries(node)? != keys {
            store.set_entries(node, &keys)?;
            n += 1;
        }
    }
    let keep: Vec<&str> = tree.iter().map(|p| p.id.as_str()).chain([ROOT]).collect();
    n += store.node_map_prune(conn.id, &keep)?;
    Ok(n)
}
