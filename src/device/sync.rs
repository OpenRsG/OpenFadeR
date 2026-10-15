//! Sync Manager: the playlists ticked for a device are exported to it "always in the latest state", i.e. [`Provider::export`] with
//! the snapshot narrowed to those playlists: new tracks are copied, tracks the device has are refreshed, the playlists get the
//! collection's current content. Which playlists are ticked per device lives in the store
//! ([`crate::store::Store::sync_playlists`]); the app syncs a device when the user presses its arrow, or at start-up when
//! "Synchronize playlists with a device" is on for it.

use crate::library::snapshot::Snapshot;
use crate::plugin::device::{Control, ExportOptions, ExportReport, Provider};
use std::path::Path;
use std::sync::Arc;

/// Exports the playlists `ticked` (Snapshot playlist ids) of `snap` to the device at `dest`.
pub fn sync(
    provider: &Provider,
    snap: &Snapshot,
    ticked: &[String],
    dest: &Path,
    o: &ExportOptions,
    ctl: &Arc<Control>,
) -> Result<ExportReport, String> {
    if ticked.is_empty() {
        return Err("No playlist is set to synchronize: tick some in the Sync Manager".into());
    }
    let ids: Vec<&str> = ticked.iter().map(String::as_str).collect();
    let Some(narrowed) = snap.narrowed(&ids) else {
        return Err("The playlists you selected are not in the collection any more. Please set the Sync Manager settings again".into());
    };
    provider.export(dest, &narrowed, o, ctl)
}
