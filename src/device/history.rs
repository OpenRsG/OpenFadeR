//! Play histories of a device: the sessions the players logged are imported into the collection's play history
//! ([`crate::store`], shown as one entry per day), automatically when the device is connected or by hand, and optionally deleted
//! from the device afterwards. Tracks that are not in the collection are skipped. [`plan`] reads what the device holds and lays out
//! one [`Item`] per history without writing anything; [`apply`] imports the ticked ones and keeps the way back ([`super::undo`]).
//!
//! A device library lists its histories as playlists whose id starts with `history:` ([`is_history`]), with no date: the entry's
//! day is the one in its name when it has one (`YYYY-MM-DD`), else the day the library was last written
//! ([`DeviceInfo::modified`]; inferred: the sessions of a stick are written when it is ejected from the player). Within a day the
//! play order is kept. Deleting them is the plugin's job ([`RemoveTarget::Histories`]).

use super::link::PrintIndex;
use super::undo::{self, Entry, Play};
use crate::library::{is_history, Library, Track};
use crate::plugin::device::{Control, DeviceInfo, Provider, RemoveTarget};
use crate::store::{self, Store};
use chrono::{Local, NaiveDate, TimeZone};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
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

/// One history found on a device, as [`plan`] lays it out.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// [`Library::playlists`] id.
    pub id: String,
    pub name: String,
    /// `YYYY-MM-DD`: the day it is filed under.
    pub day: String,
    /// Tracks the history lists.
    pub tracks: usize,
    /// Of them, tracks that are not in the collection (not imported).
    pub unknown: usize,
    /// Imported before: left alone.
    pub already: bool,
    ident: String,
    /// `(track key, unix s)` of the plays that are collection tracks.
    plays: Vec<(String, i64)>,
}

/// The histories a device holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub items: Vec<Item>,
}

/// What [`apply`] did.
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
    /// The kept way back ([`undo::run`]); `None` when nothing was imported.
    pub undo: Option<i64>,
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

/// Reads the histories `which` (ids of [`list`]; `None` = all) of the device `device_id`, whose library is `lib` and whose plugin
/// answered `info`, and lays them out; nothing is written. `collection` holds the [`store::key`]s of the collection's tracks; a
/// history track that is not one of them but is a copy of a collection file by content counts as that file.
pub fn plan(
    info: &DeviceInfo,
    device_id: &str,
    lib: &dyn Library,
    collection: &HashSet<String>,
    store: &Store,
    which: Option<&[String]>,
) -> Result<Plan, String> {
    let fallback = info
        .modified
        .and_then(|t| Local.timestamp_opt(t, 0).earliest())
        .unwrap_or_else(Local::now)
        .format(DAY)
        .to_string();
    let (mut prints, mut found): (Option<PrintIndex>, HashMap<PathBuf, Option<String>>) =
        (None, HashMap::new());
    let mut out = Plan::default();
    for h in list(lib)?
        .into_iter()
        .filter(|h| which.is_none_or(|w| w.contains(&h.id)))
    {
        let tracks = lib.tracks(Some(&h.id))?;
        let ident = identity(&h.name, &tracks);
        let day = day_in(&h.name).unwrap_or_else(|| fallback.clone());
        let already = store.history_imported(device_id, &ident)?;
        // Midnight of the day in unix seconds; one second per play keeps the order.
        let base = NaiveDate::parse_from_str(&day, DAY)
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .and_then(|d| Local.from_local_datetime(&d).earliest())
            .map_or(0, |d| d.timestamp());
        let mut plays = Vec::new();
        for (i, t) in tracks.iter().enumerate().filter(|_| !already) {
            let key = store::key(t);
            let key = if collection.contains(&key) {
                Some(key)
            } else {
                // Another name on the device: the same bytes as a collection file?
                t.file.as_ref().and_then(|f| {
                    found
                        .entry(f.clone())
                        .or_insert_with(|| {
                            if prints.is_none() {
                                prints = PrintIndex::new(store).ok();
                            }
                            let id = prints.as_ref()?.find(f)?;
                            Some(store::id_key(id)).filter(|k| collection.contains(k))
                        })
                        .clone()
                })
            };
            if let Some(key) = key {
                plays.push((key, base + i as i64));
            }
        }
        out.items.push(Item {
            id: h.id,
            name: h.name,
            day,
            tracks: tracks.len(),
            unknown: if already {
                0
            } else {
                tracks.len() - plays.len()
            },
            already,
            ident,
            plays,
        });
    }
    Ok(out)
}

/// Imports the items of `plan` that are ticked (`ticked[i]` for item `i`) into `store` as the plays of their day, marks them as
/// imported from `device_id`, and keeps the way back called `label`. An item imported before is counted, not imported again.
pub fn apply(
    plan: &Plan,
    ticked: &[bool],
    device_id: &str,
    label: &str,
    store: &Store,
    now: i64,
) -> Result<Imported, String> {
    let mut out = Imported::default();
    let mut entry = Entry::default();
    for (i, h) in plan.items.iter().enumerate() {
        if h.already {
            out.already += 1;
            out.done.push(h.name.clone());
        } else if ticked.get(i).copied().unwrap_or(false)
            && !store.history_imported(device_id, &h.ident)?
        {
            store.history_append_many(&h.day, &h.plays)?;
            store.mark_history_imported(device_id, &h.ident)?;
            entry.plays.extend(h.plays.iter().map(|(key, at)| Play {
                day: h.day.clone(),
                key: key.clone(),
                at: *at,
            }));
            entry
                .histories
                .push((device_id.to_owned(), h.ident.clone()));
            out.plays += h.plays.len();
            out.skipped += h.unknown;
            out.histories += 1;
            out.done.push(h.name.clone());
        }
    }
    out.undo = undo::keep(store, label, device_id, &entry, now)?;
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

    fn item(name: &str, already: bool, plays: &[(&str, i64)]) -> Item {
        Item {
            id: format!("history:{name}"),
            name: name.into(),
            day: "2026-02-03".into(),
            tracks: plays.len() + 1,
            unknown: 1,
            already,
            ident: format!("{name}|1|x"),
            plays: plays.iter().map(|(k, t)| ((*k).to_owned(), *t)).collect(),
        }
    }

    #[test]
    fn only_ticked_histories_are_imported_and_taken_back_whole() {
        let s = Store::in_memory().unwrap();
        s.mark_history_imported("dev", "old|1|x").unwrap();
        let plan = Plan {
            items: vec![
                item("one", false, &[("t1", 10), ("t2", 11)]),
                item("two", false, &[("t3", 20)]),
                item("old", true, &[]),
            ],
        };
        let r = apply(
            &plan,
            &[true, false, true],
            "dev",
            "Histories of KEY",
            &s,
            5,
        )
        .unwrap();
        assert_eq!((r.histories, r.plays, r.skipped, r.already), (1, 2, 1, 1));
        assert_eq!(r.done, ["one", "old"]);
        assert_eq!(s.history_day("2026-02-03").unwrap(), ["t1", "t2"]);
        assert!(s.history_imported("dev", "one|1|x").unwrap());
        assert!(
            !s.history_imported("dev", "two|1|x").unwrap(),
            "unticked: still to import"
        );
        // The same plan again imports nothing twice.
        assert_eq!(
            apply(&plan, &[true, true, true], "dev", "again", &s, 6)
                .unwrap()
                .histories,
            1
        );
        let u = undo::run(&s, r.undo.unwrap()).unwrap();
        assert_eq!(u.plays, 2);
        assert_eq!(
            s.history_day("2026-02-03").unwrap(),
            ["t3"],
            "only what the first import added"
        );
        assert!(!s.history_imported("dev", "one|1|x").unwrap());
    }
}
