//! Devices in the store: the ones seen before (name, whether the Sync Manager keeps them in sync), the Sync Manager's ticked
//! playlists, the library backups taken of them, the play histories already imported from them and the export baselines
//! "Update Collection" compares against ([`Store::device_sigs`]). Tables: `device`, `sync_playlist`, `device_backup`,
//! `history_import`, `device_sig`.

use super::{R, Sql, Store, column, exists, query_all, scalar};
use rusqlite::params;
use std::collections::HashMap;

/// One backed-up device library ([`Store::add_backup`]): what the backup list shows (date and time,
/// computer name, device name, backup destination, file system, device background colour).
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceBackup {
    pub id: i64,
    /// [`crate::device::Device::id`] of the device it came from.
    pub device: String,
    /// Unix seconds.
    pub taken_at: i64,
    pub computer: String,
    pub name: String,
    /// Folder holding the copy (a device folder and `BACKUP.txt`).
    pub path: String,
    pub fs: String,
    pub bg: u8,
}

/// The columns of a backup [`Store::add_backup`] records (the others, `id`, the store makes).
pub struct NewBackup<'a> {
    pub device: &'a str,
    pub taken_at: i64,
    pub computer: &'a str,
    pub name: &'a str,
    pub path: &'a str,
    pub fs: &'a str,
    pub bg: u8,
}

impl Store {
    /// Remembers a device (and its latest label).
    pub fn device_seen(&self, id: &str, name: &str, now: i64) -> R<()> {
        self.run(
            "INSERT INTO device(id, name, last_seen) VALUES (?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET name = ?2, last_seen = ?3",
            params![id, name, now],
        )
    }

    /// Whether the Sync Manager syncs this device ("Synchronize playlists with a device").
    pub fn device_sync_on(&self, id: &str) -> R<bool> {
        Ok(scalar(&self.db(), "SELECT sync_on FROM device WHERE id = ?1", [id])?.unwrap_or(false))
    }

    pub fn set_device_sync_on(&self, id: &str, on: bool) -> R<()> {
        self.run(
            "INSERT INTO device(id, sync_on) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET sync_on = ?2",
            params![id, on],
        )
    }

    /// The playlists ticked in the Sync Manager for `device`: [`crate::library::snapshot::Snapshot`] playlist ids.
    pub fn sync_playlists(&self, device: &str) -> R<Vec<String>> {
        column(
            &self.db(),
            "SELECT playlist FROM sync_playlist WHERE device = ?1 ORDER BY playlist",
            [device],
        )
    }

    pub fn set_sync_playlist(&self, device: &str, playlist: &str, on: bool) -> R<()> {
        self.run(
            if on {
                "INSERT OR IGNORE INTO sync_playlist(device, playlist) VALUES (?1, ?2)"
            } else {
                "DELETE FROM sync_playlist WHERE device = ?1 AND playlist = ?2"
            },
            params![device, playlist],
        )
    }

    /// Records a backup taken of a device; returns its id.
    pub fn add_backup(&self, b: &NewBackup) -> R<i64> {
        self.insert(
            "INSERT INTO device_backup(device, taken_at, computer, name, path, fs, bg) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![b.device, b.taken_at, b.computer, b.name, b.path, b.fs, b.bg],
        )
    }

    /// Every backup, newest first.
    pub fn backups(&self) -> R<Vec<DeviceBackup>> {
        query_all(
            &self.db(),
            "SELECT id, device, taken_at, computer, name, path, fs, bg FROM device_backup ORDER BY taken_at DESC, id DESC",
            [],
            |r| {
                Ok(DeviceBackup {
                    id: r.get(0)?,
                    device: r.get(1)?,
                    taken_at: r.get(2)?,
                    computer: r.get(3)?,
                    name: r.get(4)?,
                    path: r.get(5)?,
                    fs: r.get(6)?,
                    bg: r.get(7)?,
                })
            },
        )
    }

    /// Forgets a backup; the caller deletes the folder.
    pub fn delete_backup(&self, id: i64) -> R<()> {
        self.run("DELETE FROM device_backup WHERE id = ?1", [id])
    }

    /// Whether the history `history` of `device` was imported already.
    pub fn history_imported(&self, device: &str, history: &str) -> R<bool> {
        exists(
            &self.db(),
            "SELECT 1 FROM history_import WHERE device = ?1 AND history = ?2",
            params![device, history],
        )
    }

    pub fn mark_history_imported(&self, device: &str, history: &str) -> R<()> {
        self.run(
            "INSERT OR IGNORE INTO history_import(device, history) VALUES (?1, ?2)",
            params![device, history],
        )
    }

    /// Signatures of what the device held for each track (by [`super::key`]) when the collection last agreed with it.
    pub fn device_sigs(&self, device: &str) -> R<HashMap<String, i64>> {
        query_all(
            &self.db(),
            "SELECT key, sig FROM device_sig WHERE device = ?1",
            [device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn set_device_sigs(&self, device: &str, sigs: &[(String, i64)]) -> R<()> {
        self.write(|tx| {
            let mut put = tx
                .prepare_cached(
                    "INSERT INTO device_sig(device, key, sig) VALUES (?1, ?2, ?3) ON CONFLICT(device, key) DO UPDATE SET sig = ?3",
                )
                .s()?;
            for (key, sig) in sigs {
                put.execute(params![device, key, sig]).s()?;
            }
            Ok(())
        })
    }
}
