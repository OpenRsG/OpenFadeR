//! Collection → source file (two-way connections of a plugin that can rewrite its library file). The source is read as a whole
//! ([`Snapshot::take`]), each track the connection maps gets our version (tags, rating, comment, cues, beat grid), the result
//! is fitted to what the format can hold ([`crate::library::translate::fit`]) and the plugin writes it. [`plan`] is the dry
//! run (what would change, what the format cannot keep); [`apply`] copies the source file to the backups folder first and
//! never writes when that copy fails; the new file is written beside the old one and renamed over it.

use super::{foreign_id, stamp, Connection, Mode};
use crate::library::{snapshot::Snapshot, Library};
use crate::store::{self, Store, R};
use openfader_plugin::library as wire;
use std::path::{Path, PathBuf};

/// What a write-back would do.
pub struct Plan {
    pub conn: Connection,
    /// Tracks of the source whose data differs from ours.
    pub changed: usize,
    /// Tracks the source holds.
    pub tracks: usize,
    /// What the format cannot keep of our data ([`crate::library::translate::Loss`] as text), empty when all fits.
    pub loss: String,
    snapshot: wire::Snapshot,
    /// The journal position this plan covers.
    seq: i64,
    /// The source's [`stamp`] when the plan was made: a source that changed since is not overwritten.
    stamp: String,
}

impl Plan {
    /// The confirm dialog's text.
    pub fn text(&self) -> String {
        let mut t = format!(
            "{} of {} tracks in {} change.",
            self.changed, self.tracks, self.conn.label
        );
        if !self.loss.is_empty() {
            t += &format!("\nThe format cannot keep: {}.", self.loss);
        }
        t + "\nThe file is copied to the backups folder first."
    }
}

/// The write-back of `conn`, `None` when nothing is pending (nothing edited since the last one, or nothing differs).
pub fn plan(store: &Store, conn: &Connection) -> R<Option<Plan>> {
    if conn.mode != Mode::TwoWay || conn.plugin.is_empty() {
        return Ok(None);
    }
    let caps = crate::plugin::library::capabilities_of(&conn.plugin);
    if !caps.write_back {
        return Err(format!("{} cannot be written back", conn.label));
    }
    if !conn.path.is_file() {
        return Err(format!(
            "{}: write-back needs a library file",
            conn.path.display()
        ));
    }
    let map = store.source_map(conn.id)?;
    let touched = store.journal_touched_since(conn.last_seq)?;
    if !map.values().any(|(id, _)| touched.contains(id)) {
        return Ok(None);
    }
    let seq = store.journal_seq()?;
    let source_stamp = stamp(conn);
    let src = super::open(conn)?;
    let mut snap = Snapshot::take(&*src as &dyn Library)?.to_wire();
    let mut changed = 0;
    for (t, a) in &mut snap.tracks {
        let Some(&(id, _)) = foreign_id(t).and_then(|f| map.get(&f)) else {
            continue;
        };
        let Some(ours) = store.collection_track(id)? else {
            continue;
        };
        let e = store.edits(&store::id_key(id))?;
        let was = (t.clone(), a.clone().map(|a| (a.cues, a.beats)));
        *t = wire::Track {
            id: None,
            file: t.file.clone(),
            analysis_ref: t.analysis_ref.clone(),
            artwork: t.artwork.clone(),
            remote: t.remote.clone(),
            rating: e.rating.unwrap_or(ours.rating),
            comment: e.comment.unwrap_or(ours.comment.clone()),
            ..ours
        };
        let mut an = a.take().unwrap_or_default();
        if let Some(c) = e.cues {
            an.cues = c;
        }
        if let Some(g) = e.grid {
            an.beats = g;
        }
        let now = (t.clone(), Some((an.cues.clone(), an.beats.clone())));
        if was.0 != now.0 || was.1.unwrap_or_default() != now.1.clone().unwrap_or_default() {
            changed += 1;
        }
        *a = Some(an);
    }
    if changed == 0 {
        store.connection_synced(conn.id, &conn.stamp, conn.last_sync, Some(seq))?;
        return Ok(None);
    }
    let tracks = snap.tracks.len();
    let loss = crate::library::translate::fit(&mut snap, &caps);
    Ok(Some(Plan {
        conn: conn.clone(),
        changed,
        tracks,
        loss: loss.to_string(),
        snapshot: snap,
        seq,
        stamp: source_stamp,
    }))
}

/// Where the copy of `file` made before a write-back goes: `<data dir>/backups/<connection id>/<unix s>-<file name>`.
fn backup_path(conn: i64, file: &Path) -> R<PathBuf> {
    let dir = crate::paths::data_dir()
        .ok_or("no data directory (set XDG_DATA_HOME or HOME)")?
        .join("backups")
        .join(conn.to_string());
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(dir.join(format!(
        "{at}-{}",
        file.file_name().unwrap_or_default().to_string_lossy()
    )))
}

/// Writes `plan` to the source file: backup, temporary file, rename. Returns the number of tracks written.
pub fn apply(store: &Store, plan: Plan) -> R<usize> {
    if stamp(&plan.conn) != plan.stamp {
        return Err(format!(
            "{} changed since the plan was made; plan the write-back again",
            plan.conn.label
        ));
    }
    let conn = &plan.conn;
    let backup = backup_path(conn.id, &conn.path)?;
    if let Some(dir) = backup.parent() {
        crate::paths::at(std::fs::create_dir_all(dir), dir)?;
    }
    crate::paths::at(std::fs::copy(&conn.path, &backup), &backup)?;
    let tmp = conn.path.with_extension("openfader-tmp");
    let n = crate::plugin::library::write_collection(&conn.plugin, &tmp, &plan.snapshot)?;
    if let Err(e) = std::fs::rename(&tmp, &conn.path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", conn.path.display()));
    }
    store.connection_synced(conn.id, &stamp(conn), now(), Some(plan.seq))?;
    Ok(n)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
