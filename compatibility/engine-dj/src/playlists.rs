//! `Playlist` and `PlaylistEntity`: the snapshot's tree, merged into the library by name path (libdjinterop
//! `v3/playlist_impl.cpp`, `v2/playlist_entity_table.cpp`).
//!
//! A `Playlist` row is a folder or a list alike; siblings form a chain by `nextListId` (0 ends it) which the table's triggers
//! keep when a row is inserted with `nextListId = 0` (appended after the last one). A list's tracks are a chain by
//! `nextEntityId` (0 ends it). A list that exists by path keeps its row and place and gets the snapshot's tracks; one that does
//! not is appended under its parent; lists the snapshot does not name stay as they are.

use crate::schema::now;
use openfader_plugin::library::Snapshot;
use rusqlite::{params, OptionalExtension, Transaction};

/// A title as Engine accepts it: not empty, no `;` (its path separator).
pub(crate) fn title(name: &str) -> String {
    let t = name.replace(';', ",");
    let t = t.trim();
    if t.is_empty() {
        "Untitled".into()
    } else {
        t.into()
    }
}

/// Writes the lists of `snap`; `ids[i]` is the library id of `snap.tracks[i]` (`None`: not written, skipped).
pub fn write(
    tx: &Transaction,
    uuid: &str,
    snap: &Snapshot,
    ids: &[Option<i64>],
) -> rusqlite::Result<()> {
    // Ids of the open folders by depth: the parent of a row is the one above its depth.
    let mut parents: Vec<i64> = Vec::new();
    for (list, members) in snap.playlists.iter().zip(&snap.members) {
        parents.truncate(list.depth);
        let parent = parents.last().copied().unwrap_or(0);
        let name = title(&list.name);
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM Playlist WHERE title = ?1 AND parentListId = ?2",
                params![name, parent],
                |r| r.get(0),
            )
            .optional()?;
        let id = match found {
            Some(id) => {
                tx.execute(
                    "UPDATE Playlist SET lastEditTime = ?2, isPersisted = 1 WHERE id = ?1",
                    params![id, now()],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO Playlist (title, parentListId, isPersisted, nextListId, lastEditTime, isExplicitlyExported) \
                     VALUES (?1, ?2, 1, 0, ?3, 1)",
                    params![name, parent, now()],
                )?;
                tx.last_insert_rowid()
            }
        };
        parents.push(id);
        if list.is_folder {
            continue;
        }
        tx.execute("DELETE FROM PlaylistEntity WHERE listId = ?1", [id])?;
        let mut seen = std::collections::HashSet::new();
        let tracks: Vec<i64> = members
            .iter()
            .filter_map(|&m| ids.get(m).copied().flatten())
            .filter(|t| seen.insert(*t))
            .collect();
        // Last first, so each entity can name the one after it.
        let mut next = 0;
        for track in tracks.into_iter().rev() {
            tx.execute(
                "INSERT INTO PlaylistEntity (listId, trackId, databaseUuid, nextEntityId, membershipReference) \
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![id, track, uuid, next],
            )?;
            next = tx.last_insert_rowid();
        }
    }
    Ok(())
}
