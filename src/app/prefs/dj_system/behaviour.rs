//! The DEVICE tab (Preferences only): how the host treats a device whatever its plugin, the flags of
//! [`Behaviour`], read by the device code (play histories, playlist removal, backups). Row keys `djs.beh.<name>`.

use super::{Done, Row, done};
use openfader::store::{Store, device_settings::Behaviour};

pub fn rows(store: &Store) -> Vec<Row> {
    let b = Behaviour::load(store);
    vec![
        Row::header(tr!("DELETE TRACKS")),
        Row::check(
            "djs.beh.delete_tracks",
            tr!("Delete playlist tracks from device if playlist is deleted"),
            b.delete_tracks,
        ),
        Row::header(tr!("HISTORY")),
        Row::check(
            "djs.beh.import_history",
            tr!("Import the play history automatically"),
            b.import_history,
        ),
        Row::check(
            "djs.beh.delete_history_after_import",
            tr!("Delete from the device after importing the play history"),
            b.delete_history_after_import,
        ),
        Row::header(tr!("CUE SYNC")),
        Row::check(
            "djs.beh.sync_cues",
            tr!(
                "Keep cues, grid and phrases the same on the collection and the device copy of a track"
            ),
            b.sync_cues,
        ),
        Row::info(
            tr!("How"),
            tr!(
                "Plugging a device in fills the copy that has none; loading a track makes the copy you loaded win"
            ),
        ),
        Row::header(tr!("WRITING TO A DEVICE")),
        Row::check(
            "djs.beh.preview",
            tr!("Show what an export will write before it starts"),
            b.preview,
        ),
        Row::check(
            "djs.beh.sync_ask",
            tr!("Ask before syncing playlists to a device that is plugged in"),
            b.sync_ask,
        ),
        Row::check(
            "djs.beh.review_imports",
            tr!("Show what a device would change in the collection before importing it"),
            b.review_imports,
        ),
        Row::header(tr!("DEVICE LIBRARY BACKUP")),
        Row::check(
            "djs.beh.backup_before_write",
            tr!("Back up the device library before it is rewritten"),
            b.backup_before_write,
        ),
        Row::check(
            "djs.beh.auto_backup",
            tr!("Automatically backup device library upon removal"),
            b.auto_backup,
        ),
        Row::check(
            "djs.beh.check_on_eject",
            tr!("Check the stick before it is ejected and ask when it needs fixing"),
            b.check_on_eject,
        ),
        Row::info(
            tr!("Backups"),
            tr!("Backup now and the list of backups are on each device's panel"),
        ),
        Row::header(tr!("THIS COMPUTER")),
        Row::text("djs.beh.computer_name", tr!("Computer Name"), &b.computer()),
        Row::chips(
            "djs.reset",
            tr!("Reset to defaults"),
            &[tr!("RESET")],
            usize::MAX,
        ),
    ]
}

/// The click on option `i` of the check row `beh.<name>`.
pub fn choose(store: &Store, key: &str, i: usize) -> Done {
    let name = key.strip_prefix("beh.").ok_or("unknown row")?;
    done(Behaviour::set(store, name, &i.min(1).to_string()))
}
