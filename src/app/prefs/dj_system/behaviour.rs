//! The DEVICE tab (Preferences only): how the host treats a device whatever its plugin, the flags of
//! [`Behaviour`], read by the device code (play histories, playlist removal, backups). Row keys `djs.beh.<name>`.

use super::{Done, Row, done};
use openfader::store::{Store, device_settings::Behaviour};

pub fn rows(store: &Store) -> Vec<Row> {
    let b = Behaviour::load(store);
    vec![
        Row::header("DELETE TRACKS"),
        Row::check(
            "djs.beh.delete_tracks",
            "Delete playlist tracks from device if playlist is deleted",
            b.delete_tracks,
        ),
        Row::header("HISTORY"),
        Row::check(
            "djs.beh.import_history",
            "Import the play history automatically",
            b.import_history,
        ),
        Row::check(
            "djs.beh.delete_history_after_import",
            "Delete from the device after importing the play history",
            b.delete_history_after_import,
        ),
        Row::header("DEVICE LIBRARY BACKUP"),
        Row::check(
            "djs.beh.auto_backup",
            "Automatically backup device library upon removal",
            b.auto_backup,
        ),
        Row::info(
            "Backups",
            "Backup now and the list of backups are on each device's panel",
        ),
        Row::header("THIS COMPUTER"),
        Row::text("djs.beh.computer_name", "Computer Name", &b.computer()),
        Row::chips("djs.reset", "Reset to defaults", &["RESET"], usize::MAX),
    ]
}

/// The click on option `i` of the check row `beh.<name>`.
pub fn choose(store: &Store, key: &str, i: usize) -> Done {
    let name = key.strip_prefix("beh.").ok_or("unknown row")?;
    done(Behaviour::set(store, name, &i.min(1).to_string()))
}
