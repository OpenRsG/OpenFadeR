//! "Update Collection": what was edited on the player (cue and loop points, hot cues, beat grids, colours, ratings, comments) is
//! read back from the device by its plugin ([`Provider::update`]) and applied here to the collection, i.e. to our store
//! ([`crate::store`]).
//!
//! A device track is a collection track when both have the same [`store::key`](crate::store::key) (file name and size); tracks
//! exported from another computer, or no longer in the collection, are not updated. Collection edits made after the export must
//! not be undone by a device that was not touched, so each export records baselines (the plugin's signature of what the device
//! held then, [`Store::device_sigs`]): the plugin only reports tracks whose signature changed since.
//!
//! The collection is a [`Snapshot`] of the (overlaid) library, so the work can run on a thread: its tracks carry the store's
//! rating and comment, its analysis the store's cues and grid.

use crate::library::snapshot::Snapshot;
use crate::plugin::device::{Control, Provider, UpdateReport};
use crate::store::Store;
use std::path::Path;
use std::sync::Arc;

/// What [`update_collection`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Updated {
    /// Device tracks that are collection tracks.
    pub matched: usize,
    /// Matched tracks with at least one change imported.
    pub changed: usize,
    /// Device tracks not in the collection (not updated).
    pub unmatched: usize,
    pub ratings: usize,
    pub comments: usize,
    pub colors: usize,
    pub cues: usize,
    pub grids: usize,
}

/// Asks `provider` what changed on the player and imports it into `store`; the baselines of the changed tracks are kept for the
/// next time.
pub fn update_collection(
    provider: &Provider,
    root: &Path,
    device_id: &str,
    collection: &Snapshot,
    store: &Store,
    ctl: &Arc<Control>,
) -> Result<Updated, String> {
    let baselines = store.device_sigs(device_id)?.into_iter().collect();
    apply(
        &provider.update(root, collection, &baselines, ctl)?,
        device_id,
        store,
    )
}

/// Writes the edits of `report` to `store` and remembers its baselines for `device_id`.
fn apply(report: &UpdateReport, device_id: &str, store: &Store) -> Result<Updated, String> {
    let mut u = Updated {
        matched: report.matched,
        unmatched: report.unmatched,
        ..Updated::default()
    };
    for e in &report.edits {
        let before = (u.ratings, u.comments, u.colors, u.cues, u.grids);
        if let Some(r) = e.rating {
            store.set_rating(&e.key, Some(r))?;
            u.ratings += 1;
        }
        if let Some(c) = &e.comment {
            store.set_comment(&e.key, Some(c))?;
            u.comments += 1;
        }
        if let Some(c) = e.color {
            store.set_color(&e.key, Some(c))?;
            u.colors += 1;
        }
        if let Some(c) = &e.cues {
            store.set_cues(&e.key, Some(c))?;
            u.cues += 1;
        }
        if let Some((now, was)) = &e.grid {
            store.set_grid(&e.key, now, was)?;
            u.grids += 1;
        }
        u.changed += usize::from(before != (u.ratings, u.comments, u.colors, u.cues, u.grids));
    }
    store.set_device_sigs(device_id, &report.baselines)?;
    Ok(u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::device::TrackEdit;

    #[test]
    fn edits_reach_the_store_and_baselines_are_kept() {
        let dir = std::env::temp_dir().join(format!("openfader-update-{}", std::process::id()));
        let store = Store::open_at(&dir.join("s.db")).unwrap();
        let report = UpdateReport {
            matched: 2,
            unmatched: 1,
            edits: vec![
                TrackEdit {
                    key: "a|1".into(),
                    rating: Some(4),
                    comment: Some("hi".into()),
                    ..TrackEdit::default()
                },
                TrackEdit {
                    key: "b|2".into(),
                    ..TrackEdit::default()
                },
            ],
            baselines: vec![("a|1".into(), 7)],
        };
        let u = apply(&report, "dev", &store).unwrap();
        assert_eq!(
            u,
            Updated {
                matched: 2,
                changed: 1,
                unmatched: 1,
                ratings: 1,
                comments: 1,
                ..Updated::default()
            }
        );
        let e = store.edits("a|1").unwrap();
        assert_eq!((e.rating, e.comment.as_deref()), (Some(4), Some("hi")));
        assert_eq!(store.device_sigs("dev").unwrap().get("a|1"), Some(&7));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
