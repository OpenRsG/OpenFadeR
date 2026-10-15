//! Play histories of a device: the sessions the players logged are imported into the collection's play history
//! ([`crate::store`], shown as one entry per day), automatically when the device is connected or by hand, and optionally deleted
//! from the device afterwards. Tracks that are not in the collection are skipped.
//!
//! A device library lists its histories as playlists whose id starts with `history:` ([`is_history`]), with no date: the entry's
//! day is the one in its name when it has one (`YYYY-MM-DD`), else the day the library was last written
//! ([`DeviceInfo::modified`]; inferred: the sessions of a stick are written when it is ejected from the player). Within a day the
//! play order is kept. Deleting them is the plugin's job ([`RemoveTarget::Histories`]).

use crate::library::{Library, Track, is_history};
use crate::plugin::device::{Control, DeviceInfo, Provider, RemoveTarget};
use crate::store::{self, Store};
use chrono::{Local, NaiveDate, TimeZone};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// `chrono` format of a history's day.
const DAY: &str = "%Y-%m-%d";

/// One history listed on a device.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    /// [`Library::playlists`] id (`history:<raw>`): what [`Library::tracks`] takes.
    pub id: String,
    pub name: String,
}

/// What [`import`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    /// Histories imported now.
    pub histories: usize,
    /// Plays stored.
    pub plays: usize,
    /// Plays of tracks that are not in the collection.
    pub skipped: usize,
    /// Histories imported before (left alone).
    pub already: usize,
    /// Names of the histories imported now or before: what "delete from the device after importing" deletes.
    pub done: Vec<String>,
}

/// The histories of the device library `lib` (folders skipped).
pub fn list(lib: &dyn Library) -> Result<Vec<Listed>, String> {
    Ok(lib
        .playlists()?
        .into_iter()
        .filter(|p| is_history(&p.id) && !p.is_folder)
        .map(|p| Listed {
            id: p.id,
            name: p.name,
        })
        .collect())
}

/// `YYYY-MM-DD` inside `name`, if any.
fn day_in(name: &str) -> Option<String> {
    (0..name.len().saturating_sub(9))
        .filter_map(|i| name.get(i..i + 10))
        .find(|s| NaiveDate::parse_from_str(s, DAY).is_ok())
        .map(str::to_owned)
}

/// Identity of one history for "imported already": its name, length and first track (names repeat after the device clears them).
fn identity(name: &str, tracks: &[Track]) -> String {
    format!(
        "{name}|{}|{}",
        tracks.len(),
        tracks.first().map_or_else(String::new, store::key)
    )
}

/// Imports the histories `which` (ids of [`list`]; `None` = all) of the device `device_id`, whose library is `lib` and whose
/// plugin answered `info`, into `store`. `collection` holds the [`store::key`]s of the collection's tracks.
pub fn import(
    info: &DeviceInfo,
    device_id: &str,
    lib: &dyn Library,
    collection: &HashSet<String>,
    store: &Store,
    which: Option<&[String]>,
) -> Result<Imported, String> {
    let fallback = info
        .modified
        .and_then(|t| Local.timestamp_opt(t, 0).earliest())
        .unwrap_or_else(Local::now)
        .format(DAY)
        .to_string();
    let mut out = Imported::default();
    for h in list(lib)?
        .into_iter()
        .filter(|h| which.is_none_or(|w| w.contains(&h.id)))
    {
        let tracks = lib.tracks(Some(&h.id))?;
        let ident = identity(&h.name, &tracks);
        if store.history_imported(device_id, &ident)? {
            out.already += 1;
            out.done.push(h.name);
            continue;
        }
        let day = day_in(&h.name).unwrap_or_else(|| fallback.clone());
        // Midnight of the day in unix seconds; one second per play keeps the order.
        let base = NaiveDate::parse_from_str(&day, DAY)
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .and_then(|d| Local.from_local_datetime(&d).earliest())
            .map_or(0, |d| d.timestamp());
        let mut plays = Vec::new();
        for (i, t) in tracks.iter().enumerate() {
            let key = store::key(t);
            if collection.contains(&key) {
                plays.push((key, base + i as i64));
            } else {
                out.skipped += 1;
            }
        }
        store.history_append_many(&day, &plays)?;
        out.plays += plays.len();
        store.mark_history_imported(device_id, &ident)?;
        out.histories += 1;
        out.done.push(h.name);
    }
    Ok(out)
}

/// Deletes the histories named `names` from the device at `root`; how many the plugin deleted.
pub fn delete(
    provider: &Provider,
    root: &Path,
    names: &[String],
    config: &BTreeMap<String, String>,
    ctl: &Arc<Control>,
) -> Result<usize, String> {
    let target = RemoveTarget::Histories {
        names: names.to_vec(),
    };
    Ok(provider
        .remove(root, &target, false, config, ctl)?
        .histories)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_is_taken_from_the_name() {
        assert_eq!(
            day_in("HISTORY 2026-03-04 (2)").as_deref(),
            Some("2026-03-04")
        );
        assert_eq!(day_in("HISTORY 001"), None);
        assert_eq!(day_in("2026-13-45"), None);
    }
}
