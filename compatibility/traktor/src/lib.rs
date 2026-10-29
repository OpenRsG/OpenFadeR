//! A Traktor collection on a USB stick, written next to the other libraries the stick holds: one `Traktor/collection.nml`
//! (Traktor's NML, `VERSION` 19 as written by Traktor Pro 3 and read by Pro 4) whose `LOCATION`s point at the one copy of
//! each audio file the stick holds (`Contents/<artist>/<album>/<file>`, placed by the SDK's `place_audio`, shared with every
//! other format written there).
//!
//! * Where: `Traktor/collection.nml` at the stick's root. Traktor has no fixed place for a collection on a drive (its own
//!   sits in the Traktor root folder on the computer and is imported with right-click Collection > Import another
//!   Collection, playlists with Import Playlist); the file name is Traktor's own, and the folder keeps it out of `Contents/`,
//!   which the audio and the other formats own.
//! * What: `ENTRY` per track with tags, tempo, key, grid marker, hot cues, memory cues and loops ([`entry`]), and the
//!   playlist tree ([`playlists`]). The layout is the one of Mixxx (`src/library/traktor/traktorfeature.cpp`), the
//!   traktor-nml-utils fixture (`tests/fixtures/collection.nml`) and polamjag/traktor-nml (`spec/traktor/traktor-nml-demo.nml`);
//!   no official schema exists. The cue records are the ones of Traktor's MP3 tag (hellricer.github.io "Decoding the
//!   TRAKTOR4 field").
//! * Merge: the file is read first (tolerantly, [`xml`]) and everything in it stays: entries of other files with their cues,
//!   other playlists, folders, unknown elements. A track of the snapshot replaces the entry with the same folder and file
//!   name (the volume is not compared, so a stick written by Traktor under another drive letter is not duplicated); with no
//!   analysis in the snapshot the replaced entry keeps its cues, tempo, key and analysis data. A playlist replaces the one
//!   of the same name in the same folder. Nothing is deleted. A file that is not an NML is not touched and the export fails
//!   (the dry run lists it as blocked).
//! * All or nothing: the one library file, the collection, is staged as `Traktor/collection.nml.ofnew` in a
//!   [`commit::Commit`] (journal `/.openfader/journal.json`, owner `traktor`) and renamed over the old one only after it was
//!   read back, so the stick holds the old collection or the new one, never a mix; a change cut by a pulled stick is finished
//!   or undone by the next [`commit::recover`]. The audio goes first, flushed, through `place_audio`. A cancel or an error
//!   before the switch removes every file and folder the run made (the audio, `Contents/…` folders, `Traktor/`, the host's
//!   `.openfader/`) and leaves the stick byte-identical, also a stick folder that did not exist. The format has no files
//!   beside the collection (no analysis files, no settings), so the audio and this one file are all an export writes.
//! * Read-back: the staged file is parsed again with this plugin's own reader ([`check::Summary`]: entries by key with their
//!   cue counts, folders, playlists and their member keys) and compared with what was serialised; a difference is an error
//!   and the old collection stays. A clean export reports `verified`.
//! * Dry run (`ExportOptions::dry_run`): the same planning code as the export ([`plan`]) answers the `ExportPlan` (tracks to
//!   add or update, audio bytes, an estimate of the bytes and files written, what is blocked) and the counts the export would
//!   reach; nothing is written, recovered or created. The export fails with the same text as a blocked line that stops it
//!   (no room per `options.free`, an unreadable collection). Tracks over `max_file` and names FAT cannot hold (`CON.wav`)
//!   are left out whole with a warning and a blocked line. `analyse` is 0: the format has no analysis files. A track whose
//!   audio cannot be copied during the real run (stick full, pulled) is only known then and becomes a warning.
//! * Check and fix: `device_check` reads the collection without writing or recovering anything: tracks, playlists and cues,
//!   audio the entries point at that the stick lacks (`traktor:missing_audio`, the host copies it again), a collection
//!   that cannot be parsed (`traktor:unreadable`; no fix, the format keeps no backup copy on the stick), a pending journal
//!   of this plugin (`traktor:interrupted`) and staged or temporary leftovers (`traktor:leftovers`). `traktor:clean_leftovers`
//!   finishes or undoes an interrupted change and deletes the leftovers. Only entries of this stick are checked for audio:
//!   its `VOLUME`, or a folder below `Contents/`.
//! * Phases: "Copying audio", "Writing the library" (serialise and stage; the bytes of the file are counted),
//!   "Checking the library" (read-back). The byte total announced is the audio to copy plus an estimate of the file.
//! * Volume: Traktor locates a file by `VOLUME` plus path. `VOLUME` is the stick's drive letter on Windows (`E:`) and the
//!   name of the mount folder elsewhere, which is the volume label on macOS (`/Volumes/<label>`) and on the usual Linux
//!   automounts (`/run/media/<user>/<label>`); `VOLUMEID` repeats it (all that real macOS collections hold). Observed
//!   on a real Windows collection: `VOLUME="G:"` with `VOLUMEID` the drive's serial number, which a plugin cannot read.
//!   Limit: a stick written on one system and opened on another has its entries under the writer's volume name; Traktor then
//!   shows the tracks missing until the collection is exported again from the system it is used on (or relocated in Traktor).
//! * Limits: one grid marker per track (the first downbeat; the tempo is the track's average), Traktor 4's per-marker tempo
//!   is not written; no cue colours (Traktor has none); hot cues beyond the 8 pads are left out with a warning.
//! * [inferred]: that Traktor and other readers ignore the `Traktor/collection.nml.ofnew` of an interrupted export (nobody
//!   opens a file by that name; the stick checker offers to remove it); the size estimate of the file for the space check (2 KiB
//!   per new entry, 160 bytes per playlist member, the old file once more for the staged copy); that a file name FAT refuses
//!   is one of `CON PRN AUX NUL COM1-9 LPT1-9` (folders of that name are not checked); that Traktor keeps no backup of a
//!   collection on a stick (its own `Backup/` folder sits in Traktor's root folder on the computer, not on the stick).

mod check;
mod entry;
mod playlists;
mod read;
#[cfg(test)]
mod tests;
mod xml;

use chrono::Local;
use openfader_plugin::library::{
    audio_slot,
    commit::{self, Commit},
    host, place_audio, track_key, ActionAnswer, CheckReport, DeviceInfo, ExportOptions, ExportPlan,
    ExportReport, Feature, LibraryPlugin, PlanItem, Probe, Snapshot, Source,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;
use xml::El;

/// The plugin's name: the owner of its journal and the prefix of its problem and fix ids.
const OWNER: &str = "traktor";
/// Name [`DeviceInfo::libraries`] lists.
const LIBRARY: &str = "Traktor collection";
/// The collection file, relative to the device root.
pub const FILE: &str = "Traktor/collection.nml";
/// `NML VERSION` written (a file that already has a higher one keeps it).
const VERSION: u32 = 19;

pub struct Plugin;

impl LibraryPlugin for Plugin {
    fn probe(path: &Path) -> Option<Probe> {
        read::probe(path)
    }

    fn open(path: &Path, _settings: &Value) -> Result<Box<dyn Source>, String> {
        read::open(path)
    }

    fn analysis_ref(
        reference: &str,
        _settings: &Value,
    ) -> Option<openfader_plugin::library::Analysis> {
        read::analysis_ref(reference)
    }

    fn device_probe(root: &Path, _settings: &Value) -> Result<DeviceInfo, String> {
        let modified = modified(root);
        Ok(DeviceInfo {
            libraries: if modified.is_some() {
                vec![LIBRARY.into()]
            } else {
                Vec::new()
            },
            title: "Traktor".into(),
            about: "Traktor Pro 3 and 4 on a computer: Collection > Import another Collection"
                .into(),
            can_write: root.is_dir(),
            features: vec![Feature::Export, Feature::Plan, Feature::Check],
            modified,
            ..Default::default()
        })
    }

    fn export(
        root: &Path,
        snapshot: &Snapshot,
        options: &ExportOptions,
        _settings: &Value,
    ) -> Result<ExportReport, String> {
        export(root, snapshot, options)
    }

    fn device_check(
        root: &Path,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<CheckReport, String> {
        Ok(check::check(root))
    }

    fn device_fix(
        root: &Path,
        fix: &str,
        _config: &BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<ActionAnswer, String> {
        check::fix(root, fix)
    }

    /// Evidence (module doc, Limits): eight hot cue pads, one grid marker per track (no tempo changes).
    fn capabilities(_settings: &Value) -> openfader_plugin::library::Capabilities {
        openfader_plugin::library::Capabilities {
            hot_cues: Some(8),
            tempo_changes: false,
            ..Default::default()
        }
    }
}

/// `VOLUME` of the device mounted at `root`: its drive letter (`E:`) when the path starts with one, else the name of the
/// mount folder.
pub fn volume(root: &Path) -> String {
    let s = root.to_string_lossy();
    match s.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => {
            format!("{}:", (*d as char).to_ascii_uppercase())
        }
        _ => root
            .components()
            .rfind(|c| matches!(c, Component::Normal(_)))
            .map_or_else(
                || "USB".into(),
                |c| c.as_os_str().to_string_lossy().into_owned(),
            ),
    }
}

/// Unix seconds the collection of the device at `root` was last written; `None`: it holds none.
fn modified(root: &Path) -> Option<i64> {
    std::fs::metadata(root.join(FILE)).ok().map(|m| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64)
    })
}

/// The collection file of the device, or an empty one when it holds none; `Err` for a file that is no NML (it is never
/// overwritten).
fn load(path: &Path) -> Result<El, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(fresh()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let text = String::from_utf8_lossy(&bytes);
    let nml = xml::parse(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if nml.name != "NML" {
        return Err(format!(
            "{}: not a Traktor collection (<{}>)",
            path.display(),
            nml.name
        ));
    }
    Ok(nml)
}

fn fresh() -> El {
    El::new("NML")
        .attr("VERSION", VERSION)
        .with(
            El::new("HEAD")
                .attr("COMPANY", "www.native-instruments.com")
                .attr("PROGRAM", "Traktor"),
        )
        .with(El::new("MUSICFOLDERS"))
        .with(El::new("COLLECTION").attr("ENTRIES", 0))
        .with(El::new("SETS").attr("ENTRIES", 0))
        .with(El::new("PLAYLISTS"))
        .with(El::new("INDEXING"))
}

/// What a replaced entry keeps when the snapshot has no analysis for its track: Traktor's analysis data, the grid lock, the
/// cues, and tempo and key where the new entry has none.
fn keep_analysis(old: &El, new: &mut El) {
    for k in ["AUDIO_ID", "LOCK", "LOCK_MODIFICATION_TIME"] {
        if let Some(v) = old.get(k) {
            new.set(k, v);
        }
    }
    for kid in &old.kids {
        let kept = match kid.name.as_str() {
            "LOUDNESS" | "CUE_V2" => true,
            "TEMPO" | "MUSICAL_KEY" => new.child(&kid.name).is_none(),
            _ => false,
        };
        if kept {
            new.kids.push(kid.clone());
        }
    }
}

/// `(DIR, FILE)` of an entry.
fn location(e: &El) -> Option<(String, String)> {
    let l = e.child("LOCATION")?;
    Some((l.get("DIR")?.into(), l.get("FILE")?.into()))
}

/// Size as a person reads it: "4.3 GiB", "312 MiB".
fn size_text(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MiB", bytes.div_ceil(1 << 20))
    }
}

/// Names Windows and FAT refuse whatever the characters (`clean_name` cannot repair them).
fn reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit() && c != '0')
}

/// One snapshot track the export puts on the stick.
struct Item {
    /// Index in the snapshot.
    i: usize,
    size: u64,
    /// The audio is copied (the stick does not hold it yet).
    copy: bool,
    /// The collection has an entry for it already.
    exists: bool,
}

/// What an export does, from the stick as it is (see [`plan`]).
struct Planned {
    nml: El,
    items: Vec<Item>,
    copy_bytes: u64,
    write_bytes: u64,
    files: usize,
    /// What stops the export or leaves tracks out, one text each: cause, consequence, action.
    blocked: Vec<String>,
    /// The blocked text that stops the whole export (no room); the others only leave tracks out.
    fatal: Option<String>,
}

/// Bytes one new `ENTRY` takes in the file, and one playlist member (estimates for the space check).
const ENTRY_BYTES: u64 = 2048;
const MEMBER_BYTES: u64 = 160;

/// The tracks that can go on the stick, where, and what that takes. A track that cannot (no audio, over `max_file`, a name FAT
/// refuses) is left out with a warning and a blocked text. `Err`: the stick's collection cannot be read, so it cannot be
/// merged into. Reads the stick, writes nothing.
fn plan(
    root: &Path,
    snap: &Snapshot,
    options: &ExportOptions,
    rep: &mut ExportReport,
) -> Result<Planned, String> {
    let path = root.join(FILE);
    let nml = load(&path).map_err(|e| {
        format!(
            "The Traktor collection of this stick cannot be read ({e}). Exporting over it would lose what it holds, so nothing is written and the file stays as it is; restore it from a backup or move it out of the Traktor folder and export again."
        )
    })?;
    let mut known: HashSet<(String, String)> = nml
        .child("COLLECTION")
        .map(|c| c.kids.iter().filter_map(location).collect())
        .unwrap_or_default();
    // Lowercased slots a new track fills, so two tracks of one file share the one copy as they do when copied.
    let mut slots: HashSet<String> = HashSet::new();
    let (mut items, mut blocked) = (Vec::new(), Vec::new());
    for (i, (t, _)) in snap.tracks.iter().enumerate() {
        let Some(file) = t.file.as_deref() else {
            rep.warnings.push(format!(
                "{}: the track has no audio file. It is left out of the stick; relink the file in your collection.",
                t.title
            ));
            continue;
        };
        let name = file
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let size = match std::fs::metadata(file) {
            Ok(m) => m.len(),
            Err(e) => {
                rep.warnings.push(format!(
                    "{name}: the audio file is missing or cannot be read ({e}). The track is left out of the stick; check the file in your collection."
                ));
                continue;
            }
        };
        if let Some(max) = options.max_file.filter(|&m| size > m) {
            rep.warnings.push(format!(
                "{name}: the audio file is {}, over the {} this stick's file system holds; track left out",
                size_text(size),
                size_text(max)
            ));
            blocked.push(format!(
                "{name} is {} but the file system of this stick holds files up to {}. It would be left out of the export; convert it to FLAC or use an exFAT stick.",
                size_text(size),
                size_text(max)
            ));
            continue;
        }
        let (rel, there) = match audio_slot(root, t) {
            Ok(slot) => slot,
            Err(e) => {
                rep.warnings.push(e);
                continue;
            }
        };
        let slot_name = rel.rsplit('/').next().unwrap_or(&rel);
        if !there && reserved(slot_name) {
            rep.warnings.push(format!(
                "{name}: the file name is reserved by the file system of the stick; track left out"
            ));
            blocked.push(format!(
                "The file system of this stick cannot hold a file called {slot_name}: the name is reserved. It would be left out of the export; rename the file."
            ));
            continue;
        }
        items.push(Item {
            i,
            size,
            copy: !there && slots.insert(rel.to_lowercase()),
            exists: !known.insert(entry::dir_file(&rel)),
        });
    }
    let copy_bytes: u64 = items.iter().filter(|p| p.copy).map(|p| p.size).sum();
    let members: usize = snap.members.iter().map(Vec::len).sum();
    let write_bytes = std::fs::metadata(&path).map_or(0, |m| m.len())
        + items.iter().filter(|p| !p.exists).count() as u64 * ENTRY_BYTES
        + members as u64 * MEMBER_BYTES
        + 4096;
    let need = copy_bytes + write_bytes;
    let fatal = options.free.filter(|&f| need > f).map(|free| {
        format!(
            "Not enough space on the stick: about {} are needed and {} are free. Nothing is written; free some space or export fewer tracks.",
            size_text(need),
            size_text(free)
        )
    });
    blocked.extend(fatal.clone());
    Ok(Planned {
        files: items.iter().filter(|p| p.copy).count() + 1,
        nml,
        items,
        copy_bytes,
        write_bytes,
        blocked,
        fatal,
    })
}

/// What [`export`] would do, nothing written: the plan and the counts a real export reaches.
fn dry_run(root: &Path, snap: &Snapshot, options: &ExportOptions) -> ExportReport {
    let mut rep = ExportReport::default();
    let mut out = ExportPlan {
        playlists: snap.playlists.len(),
        ..ExportPlan::default()
    };
    match plan(root, snap, options, &mut rep) {
        Ok(p) => {
            for it in &p.items {
                let (t, _) = &snap.tracks[it.i];
                let item = PlanItem {
                    key: track_key(t),
                    title: t.title.clone(),
                    artist: t.artist.clone(),
                    bytes: if it.copy { it.size } else { 0 },
                };
                if it.exists {
                    &mut out.update
                } else {
                    &mut out.add
                }
                .push(item);
            }
            (out.copy_bytes, out.write_bytes, out.files, out.blocked) =
                (p.copy_bytes, p.write_bytes, p.files, p.blocked);
        }
        Err(e) => out.blocked.push(e),
    }
    (rep.added, rep.updated) = (out.add.len(), out.update.len());
    rep.tracks = rep.added + rep.updated;
    rep.playlists = out.playlists;
    rep.plan = Some(out);
    rep
}

/// Records the folders above `file` that do not exist yet (outermost first), so an export that fails removes them again.
fn note_dirs(created: &mut Vec<PathBuf>, file: &Path) {
    let mut missing: Vec<PathBuf> = file
        .ancestors()
        .skip(1)
        .take_while(|d| !d.as_os_str().is_empty() && !d.exists())
        .map(Path::to_path_buf)
        .collect();
    missing.reverse();
    created.extend(missing);
}

fn export(root: &Path, snap: &Snapshot, options: &ExportOptions) -> Result<ExportReport, String> {
    if options.dry_run {
        return Ok(dry_run(root, snap, options));
    }
    let had_journal_dir = root.join(commit::DIR).is_dir();
    let mut created = Vec::new();
    let result = run(root, snap, options, &mut created);
    // Once the switching journal is on the stick the new collection counts as made (a recovery finishes it): keep what it needs.
    let switching = commit::pending(root)
        .is_some_and(|j| j.owner == OWNER && j.state == commit::State::Switching);
    if result.is_err() && !switching {
        if !had_journal_dir {
            let _ = std::fs::remove_dir(root.join(commit::DIR));
        }
        for p in created.iter().rev() {
            let _ = if p.is_dir() {
                std::fs::remove_dir(p)
            } else {
                std::fs::remove_file(p)
            };
        }
    }
    result
}

/// Copies the audio, builds the collection, stages it, reads it back and switches it in. `created`: every file and folder this
/// run made, oldest first, for [`export`] to remove again on an error.
fn run(
    root: &Path,
    snap: &Snapshot,
    options: &ExportOptions,
    created: &mut Vec<PathBuf>,
) -> Result<ExportReport, String> {
    // An interrupted change left on the stick is recovered before the collection is read.
    let mut commit = Commit::new(root, OWNER).map_err(|e| {
        format!("The export an earlier run left unfinished on this stick could not be completed ({e}). Nothing is written now; reconnect the stick and try again, or use FIX THE STICK.")
    })?;
    let mut rep = ExportReport::default();
    let planned = plan(root, snap, options, &mut rep)?;
    if let Some(fatal) = planned.fatal {
        return Err(fatal);
    }
    host::set_bytes_total(planned.copy_bytes + planned.write_bytes);
    let mut nml = planned.nml;
    let vol = volume(root);
    let now = Local::now().naive_local();
    let colors: HashMap<usize, u8> = snap.colors.iter().copied().collect();
    let mut keys: Vec<Option<String>> = vec![None; snap.tracks.len()];
    let coll = nml.child_mut("COLLECTION");
    let mut at: HashMap<(String, String), usize> = coll
        .kids
        .iter()
        .enumerate()
        .filter_map(|(i, e)| Some((location(e)?, i)))
        .collect();

    let total = planned.items.len();
    for (n, it) in planned.items.iter().enumerate() {
        if host::progress("Copying audio", n, total) {
            return Err("cancelled".into());
        }
        let (t, a) = &snap.tracks[it.i];
        if let (true, Ok((rel, _))) = (it.copy, audio_slot(root, t)) {
            note_dirs(created, &root.join(rel));
        }
        let rel = match place_audio(root, t) {
            Ok((rel, copied)) => {
                if copied {
                    created.push(root.join(&rel));
                }
                rel
            }
            Err(e) if e == "cancelled" => return Err(e),
            Err(e) => {
                rep.warnings.push(format!(
                    "{}: the audio could not be copied to the stick ({e}). The track is left out; check the free space on the stick and export again.",
                    t.title
                ));
                continue;
            }
        };
        let mut e = entry::entry(
            &vol,
            &rel,
            it.size,
            t,
            a.as_ref(),
            colors.get(&it.i).copied(),
            now,
            &mut rep.warnings,
        );
        let loc = entry::dir_file(&rel);
        keys[it.i] = Some(format!("{vol}{}{}", loc.0, loc.1));
        match at.get(&loc) {
            Some(&k) => {
                if a.is_none() {
                    keep_analysis(&coll.kids[k], &mut e);
                }
                coll.kids[k] = e;
                rep.updated += 1;
            }
            None => {
                at.insert(loc, coll.kids.len());
                coll.kids.push(e);
                rep.added += 1;
            }
        }
    }
    if host::progress("Writing the library", total, total) {
        return Err("cancelled".into());
    }
    coll.set(
        "ENTRIES",
        coll.kids.iter().filter(|e| e.name == "ENTRY").count(),
    );
    rep.playlists = playlists::merge(&mut nml, snap, &keys);
    if nml
        .get("VERSION")
        .and_then(|v| v.parse::<u32>().ok())
        .is_none_or(|v| v < VERSION)
    {
        nml.set("VERSION", VERSION);
    }
    let (target, doc) = (root.join(FILE), nml.document());
    note_dirs(created, &target);
    commit.stage(&target, doc.as_bytes()).map_err(|e| {
        format!("The Traktor collection could not be written to the stick ({e}). Nothing was changed on it; check that it is not full or write-protected, then export again.")
    })?;
    if host::add_bytes(doc.len() as u64) || host::progress("Checking the library", total, total) {
        return Err("cancelled".into());
    }
    check::verify(&Commit::staged_path(&target), &nml)?;
    commit.commit().map_err(|e| {
        format!("The Traktor collection could not be put in place on the stick ({e}). The change is finished or undone the next time the stick is used, so it never holds half of it; leave it connected and use FIX THE STICK.")
    })?;

    rep.tracks = rep.added + rep.updated;
    rep.verified = Some(true);
    rep.notes.push(format!(
        "Traktor: right-click Collection > Import another Collection and pick {FILE} on the device (volume \"{vol}\")"
    ));
    Ok(rep)
}
