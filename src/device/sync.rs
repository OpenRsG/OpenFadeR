//! Sync Manager: the playlists ticked for a device are exported to it "always in the latest state", i.e. [`super::export`] of
//! the snapshot narrowed to those playlists ([`narrow`]), in every format picked for the device: new tracks are copied, tracks the
//! device has are refreshed, the playlists get the collection's current content. Which playlists are ticked per device lives in the
//! store ([`crate::store::Store::sync_playlists`]); the app syncs a device when the user presses its arrow, or at start-up when
//! "Synchronize playlists with a device" is on for it.

use crate::library::snapshot::Snapshot;

/// The part of `snap` the playlists `ticked` (Snapshot playlist ids) hold.
pub fn narrow(snap: &Snapshot, ticked: &[String]) -> Result<Snapshot, String> {
    if ticked.is_empty() {
        return Err("No playlist is set to synchronize: tick some in the Sync Manager".into());
    }
    let ids: Vec<&str> = ticked.iter().map(String::as_str).collect();
    snap.narrowed(&ids).ok_or_else(|| "The playlists you selected are not in the collection any more. Please set the Sync Manager settings again".into())
}
