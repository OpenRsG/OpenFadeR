//! VirtualDJ's library of a removable drive as a device plugin: `export` writes the drive's own database and its lists, so a
//! stick that carries a collection opens in VirtualDJ with its tracks, cues, loops and playlists.
//!
//! Written on the drive (below its root):
//!
//! | file | content |
//! |---|---|
//! | `VirtualDJ/database.xml` | one `<Song>` per track: tags, tempo, key, grid anchor, hot and memory cues, saved loops ([`song`]) |
//! | `VirtualDJ/MyLists/<folders>/<name>.vdjfolder` | one list per snapshot playlist; `VirtualDJ/Folders/…` on a stick that only has that folder ([`lists`]) |
//! | `Contents/<artist>/<album>/<file>` | the audio, shared with the other formats written to the stick (`place_audio`) |
//!
//! Where VirtualDJ looks: every drive that holds media has `VirtualDJ/database.xml` at its root, VirtualDJ merges the databases
//! of all connected drives while it runs, and the one in the home folder only serves the system drive (Phantom Deejay, staff of
//! virtualdj.com: forum threads 236400, 244461 and 252597; `VirtualDJ Local Database v6.xml` at the root is the format of
//! VirtualDJ 7 and older). `FilePath` is the absolute path with the drive letter (`E:\Contents\…`, `/Volumes/NAME/…` on macOS);
//! VirtualDJ replaces the drive letter or volume name by the real one when the drive is plugged in, so the database moves
//! between computers and between Windows and macOS as it is (forum threads 91468, 193485 and 242242), but a list file is not as
//! portable (the same threads). The format details are in [`song`] and [`lists`].
//!
//! Merge: songs of the drive that the snapshot does not hold are kept byte for byte, as is every other top-level element; a
//! song the snapshot holds (same file, found by the path without the drive name) keeps its play counts, colours and automix
//! points and takes the snapshot's tags, tempo, key, grid, cues and loops; a list replaces the file of the same name. The
//! database is merged in memory and written as described under "A stick you can trust".
//!
//! Limits: the grid is VirtualDJ's constant one (anchor and tempo); `Size` of a loop counts beats (inferred, see [`song`]); lists
//! hold the path with the drive letter the database uses, VirtualDJ 2024 and later link them to the drive, older versions need
//! the same letter on the next computer.
//!
//! Reading (`read.rs`): `probe` and `open` take a `database.xml`, or a folder that holds `VirtualDJ/database.xml` (a drive root)
//! or `database.xml` (the `VirtualDJ` folder); the lists come from `MyLists`, `Folders` and `Playlists` next to the database.
//! A track's audio is looked for below the drive root when its `FilePath` names another drive.
//!
//! # A stick you can trust
//!
//! - **One change, all or nothing.** An export copies the audio first (`place_audio`: a `.part` file flushed and renamed), then
//!   stages every library file it writes (the database and all the `.vdjfolder` lists) in ONE [`Commit`] (owner `virtualdj`,
//!   journal `/.openfader/journal.json`): each file is written next to its target as `<name>.ofnew`, flushed, read back, and only
//!   then renamed over the old one. A pulled stick or a crash leaves the whole old set or, once the host has finished the change,
//!   the whole new set. An error or a cancel before the switch removes every file and folder the run made (also `.openfader` if it
//!   was not there): the stick is byte-identical to what it was. No file that existed before is changed in place, so there is
//!   nothing else to restore.
//! - **Read-back.** The staged files are read with this crate's own reader (from a scratch copy in the computer's temporary
//!   folder, never on the stick): song count, `<Poi>` cue and loop count of every song written with an analysis, list count and
//!   the songs of each list. A mismatch is an error and the old files stay; success sets `ExportReport::verified`.
//! - **Dry run.** `ExportOptions::dry_run` answers `ExportReport::plan` from the same code (`plan`, `merge`, `lists::build`) and
//!   writes nothing, not even a folder or a scratch file; it never recovers a journal. `analyse` is always 0 (this format has no
//!   analysis files); `write_bytes` is the size of the library text that would be staged. Not enough room (`free`), a database that
//!   cannot be read and a missing stick folder are `blocked` with the text the real export returns as its error; a track over
//!   `max_file` or with a name FAT reserves (`CON`, `NUL`, ...) is left out whole with a warning and a `blocked` line. Two new
//!   tracks that would share one device path are planned at the same path (the real export gives the second a ` (2)` name).
//! - **Phases.** "Copying audio", "Writing the library" (staging, `add_bytes` with the lengths), "Checking the library".
//! - **Check and fix.** `device_check` reads the database with [`read`] and reports songs, lists and cues, an unreadable database,
//!   audio the database lists that is not on the stick (`files`; the host copies it again), lists that do not parse, a pending
//!   journal of this plugin and `*.ofnew` leftovers below `VirtualDJ` (fix `virtualdj:clean_leftovers`, which removes only
//!   those and refuses while a journal is pending). Nothing is recovered or written by the check. No restore fix is offered: a
//!   stick carries no backup copy of the database (the `Backup` folder VirtualDJ keeps is on the computer; [inferred]).
//! - **[inferred]** The `Backup` folder is not on a stick; VirtualDJ ignores `*.ofnew` files; the order of the renames inside a
//!   commit does not matter to VirtualDJ because it reads the database and the lists only when the drive is plugged in.

pub mod song;
pub mod xml;

mod check;
mod lists;
mod read;

use openfader_plugin::library::commit::{self, Commit};
use openfader_plugin::library::{
    ActionAnswer, CheckReport, DeviceInfo, ExportOptions, ExportPlan, ExportReport, Feature,
    LibraryPlugin, PlanItem, Probe, Snapshot, Source, audio_slot, host, place_audio, track_key,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `database.xml` of a drive, relative to its root.
pub const DATABASE: &str = "VirtualDJ/database.xml";
/// The library name of [`DeviceInfo`].
pub const LIBRARY: &str = "VirtualDJ database";
const ROOT: &str = "VirtualDJ_Database";

pub struct Plugin;

impl LibraryPlugin for Plugin {
    /// A `database.xml` of VirtualDJ, or a folder holding one (a drive root or the `VirtualDJ` folder).
    fn probe(path: &Path) -> Option<Probe> {
        read::probe(path)
    }

    fn open(path: &Path, _settings: &Value) -> Result<Box<dyn Source>, String> {
        Ok(Box::new(read::Reader::open(path)?))
    }

    fn analysis_ref(
        reference: &str,
        _settings: &Value,
    ) -> Option<openfader_plugin::library::Analysis> {
        read::analysis_ref(reference)
    }

    fn device_probe(root: &Path, _settings: &Value) -> Result<DeviceInfo, String> {
        let modified = std::fs::metadata(root.join(DATABASE)).ok().map(|m| {
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64)
        });
        Ok(DeviceInfo {
            libraries: if modified.is_some() {
                vec![LIBRARY.into()]
            } else {
                vec![]
            },
            title: "VirtualDJ".into(),
            about: "VirtualDJ 8 and newer on Windows and macOS: opens the drive's own database when it is plugged in".into(),
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
        _config: &std::collections::BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<CheckReport, String> {
        Ok(check::check(root))
    }

    fn device_fix(
        root: &Path,
        fix: &str,
        _config: &std::collections::BTreeMap<String, String>,
        _settings: &Value,
    ) -> Result<ActionAnswer, String> {
        check::fix(root, fix)
    }

    /// Evidence (module doc, Limits): the grid is VirtualDJ's constant one, anchor and tempo; the eight hot cue slots are
    /// the ones [`song::fill`] writes.
    fn capabilities(_settings: &Value) -> openfader_plugin::library::Capabilities {
        openfader_plugin::library::Capabilities {
            hot_cues: Some(8),
            tempo_changes: false,
            ..Default::default()
        }
    }
}

/// The plugin's name in the journal of a change.
const OWNER: &str = "virtualdj";

/// A track of the snapshot as it is (or will be) on the stick: its path from the root and its size.
struct Slot {
    rel: String,
    /// The audio is on the stick already (it is not copied).
    there: bool,
    size: u64,
}

fn cancelled(phase: &str, done: usize, total: usize) -> Result<(), String> {
    if host::progress(phase, done, total) {
        Err("cancelled".into())
    } else {
        Ok(())
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

fn mb(bytes: u64) -> u64 {
    bytes.div_ceil(1 << 20)
}

fn no_folder(root: &Path) -> String {
    format!(
        "{} is not a folder, so there is no stick to write to. Nothing is written; check that the stick is plugged in and choose it again.",
        root.display()
    )
}

/// What the audio of the snapshot needs: where each track goes (`None`: left out, with a warning), what the sentences of a
/// dry run say is blocked, and the bytes to copy. `fatal` is the blocked sentence that stops a real export.
struct Planned {
    slots: Vec<Option<Slot>>,
    blocked: Vec<String>,
    need: u64,
    fatal: Option<String>,
}

/// Reads only: the dry run and the real export both start here.
fn plan(
    root: &Path,
    snapshot: &Snapshot,
    options: &ExportOptions,
    report: &mut ExportReport,
) -> Planned {
    let mut p = Planned {
        slots: Vec::new(),
        blocked: Vec::new(),
        need: 0,
        fatal: None,
    };
    for (t, _) in &snapshot.tracks {
        let slot = audio_slot(root, t).and_then(|(rel, there)| {
            let at = if there {
                root.join(&rel)
            } else {
                t.file.clone().unwrap_or_default()
            };
            let size = std::fs::metadata(&at)
                .map_err(|e| format!("{}: {e}", at.display()))?
                .len();
            Ok(Slot { rel, there, size })
        });
        let slot = match slot {
            Ok(s) => s,
            Err(e) => {
                report.warnings.push(format!("skipped: {e}"));
                p.slots.push(None);
                continue;
            }
        };
        let name = slot.rel.rsplit('/').next().unwrap_or_default().to_owned();
        if !slot.there {
            if let Some(max) = options.max_file.filter(|m| slot.size > *m) {
                report.warnings.push(format!(
                    "{name}: the file is {} MB and the file system of this stick holds files up to {} MB; track left out",
                    mb(slot.size),
                    mb(max)
                ));
                p.blocked.push(format!(
                    "{name} is {} MB but the file system of this stick holds files up to {} MB. It would be left out of the export; convert it to FLAC or use an exFAT stick.",
                    mb(slot.size),
                    mb(max)
                ));
                p.slots.push(None);
                continue;
            }
            if slot.rel.split('/').any(reserved) {
                report.warnings.push(format!(
                    "{name}: the file name is reserved by the file system of the stick; track left out"
                ));
                p.blocked.push(format!(
                    "The file system of this stick cannot hold a file called {name}: the name is reserved. It would be left out of the export; rename the file."
                ));
                p.slots.push(None);
                continue;
            }
            p.need += slot.size;
        }
        p.slots.push(Some(slot));
    }
    if let Some(free) = options.free.filter(|f| p.need > *f) {
        let text = format!(
            "Not enough space on the stick: {} MB of tracks have to be copied and {} MB are free. Nothing is written; free some space or export fewer tracks.",
            mb(p.need),
            free >> 20
        );
        p.blocked.push(text.clone());
        p.fatal = Some(text);
    }
    p
}

/// Files and folders a run made on the stick: removed again (newest first) when it fails or is cancelled, so the stick is as
/// it was.
struct Undo {
    root: PathBuf,
    created: Vec<PathBuf>,
    /// The stick had the host's `.openfader` folder before the run (it stays).
    had_dir: bool,
}

impl Undo {
    /// Records the folders of `dir` that do not exist yet (oldest first).
    fn made(&mut self, dir: &Path) {
        let mut missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| !d.exists())
            .map(Path::to_path_buf)
            .collect();
        missing.reverse();
        self.created.extend(missing);
    }

    fn undo(&mut self) {
        for p in self.created.iter().rev() {
            let _ = if p.is_dir() {
                std::fs::remove_dir(p)
            } else {
                std::fs::remove_file(p)
            };
        }
        if !self.had_dir {
            let _ = std::fs::remove_dir(self.root.join(commit::DIR));
        }
    }
}

/// The database of the drive: the one it holds, else an empty one.
fn load(root: &Path) -> Result<xml::Doc, String> {
    let path = root.join(DATABASE);
    let bad = |why: String| {
        format!(
            "The VirtualDJ database of this stick cannot be read ({}: {why}). Nothing is written so that it is not made worse; restore VirtualDJ/database.xml from a backup (VirtualDJ keeps copies in the Backup folder of its computer) and export again.",
            path.display()
        )
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes).map_err(|_| bad("not UTF-8".into()))?;
            let doc = xml::split(&text).map_err(|e| bad(e.to_string()))?;
            if doc.root != ROOT {
                return Err(bad("not a VirtualDJ database".into()));
            }
            Ok(doc)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(xml::Doc {
            head: format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<{ROOT} Version=\"8.5\">"),
            items: Vec::new(),
            root: ROOT.into(),
        }),
        Err(e) => Err(bad(e.to_string())),
    }
}

/// Puts the tracks into `doc`: a song of the same file is refreshed in place, any other is appended. Returns the `FilePath`
/// and size of each track, and for each track whether the database held its song already.
#[allow(clippy::type_complexity)]
fn merge(
    doc: &mut xml::Doc,
    root: &Path,
    snapshot: &Snapshot,
    placed: &[Option<Slot>],
    report: &mut ExportReport,
) -> Result<(Vec<Option<(String, u64)>>, Vec<bool>), String> {
    let style = doc
        .items
        .iter()
        .find_map(|i| song::Style::of(i.file_path.as_deref()?))
        .unwrap_or_else(|| song::Style::of_root(root));
    let mut at: HashMap<String, usize> = HashMap::new();
    for (n, item) in doc.items.iter().enumerate() {
        if let Some(p) = &item.file_path {
            at.entry(song::identity(p)).or_insert(n);
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (mut files, mut was) = (Vec::new(), Vec::new());
    for ((t, a), p) in snapshot.tracks.iter().zip(placed) {
        let Some(p) = p else {
            files.push(None);
            was.push(false);
            continue;
        };
        let path = style.path(&p.rel);
        let entry = song::Entry {
            track: t,
            analysis: a.as_ref(),
            size: p.size,
            now,
        };
        let known = at.get(&song::identity(&path)).copied();
        let mut el = match known {
            Some(n) => xml::parse(&doc.items[n].raw).map_err(|e| format!("{path}: {e}"))?,
            None => xml::El::new("Song"),
        };
        song::fill(&mut el, &path, &entry, &mut report.warnings);
        let mut raw = String::from("\n");
        el.write(1, &mut raw);
        was.push(known.is_some());
        match known {
            Some(n) => {
                doc.items[n].raw = raw;
                report.updated += 1;
            }
            None => {
                at.insert(song::identity(&path), doc.items.len());
                doc.items.push(xml::Item {
                    raw,
                    file_path: Some(path.clone()),
                });
                report.added += 1;
            }
        }
        files.push(Some((path, p.size)));
    }
    Ok((files, was))
}

/// What an export puts on the stick, and what reading it back must find ([`check::verify`]).
struct Composed {
    /// Library files to write: the lists, then the database.
    files: Vec<(PathBuf, String)>,
    /// Folders of the snapshot (empty ones too).
    dirs: Vec<PathBuf>,
    /// Each list file and how many songs it holds.
    lists: Vec<(PathBuf, usize)>,
    /// `FilePath` of each song written with an analysis and its number of cues and loops.
    cues: Vec<(String, usize)>,
    /// Songs the database holds after the export.
    songs: usize,
    /// Per snapshot track: the database held its song already.
    known: Vec<bool>,
}

/// Merges the snapshot into `doc` and builds every library file, in memory: the dry run and the real export share it.
fn compose(
    root: &Path,
    snapshot: &Snapshot,
    doc: &mut xml::Doc,
    slots: &[Option<Slot>],
    report: &mut ExportReport,
) -> Result<Composed, String> {
    let (files, known) = merge(doc, root, snapshot, slots, report)?;
    report.tracks = report.added + report.updated;
    report.playlists = snapshot.playlists.len();
    let list_dir = lists::dir(root);
    let (dirs, mut out, counts) = lists::build(snapshot, &list_dir, &files, &mut report.warnings);
    let lists = out.iter().map(|(f, _)| f.clone()).zip(counts).collect();
    if report.tracks > 0 {
        let mut text = doc.head.clone();
        text.extend(doc.items.iter().map(|i| i.raw.as_str()));
        text.push_str(&format!("\n</{ROOT}>\n"));
        out.push((root.join(DATABASE), text));
    }
    if !snapshot.playlists.is_empty() {
        report.notes.push(format!(
            "lists were written to VirtualDJ/{} on the device",
            list_dir.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    let cues = files
        .iter()
        .zip(&snapshot.tracks)
        .filter_map(|(f, (_, a))| {
            let n = a.as_ref()?.cues.iter().filter(|c| c.hot_cue <= 8).count();
            Some((f.as_ref()?.0.clone(), n))
        })
        .collect();
    Ok(Composed {
        files: out,
        dirs,
        lists,
        cues,
        songs: doc.items.iter().filter(|i| i.file_path.is_some()).count(),
        known,
    })
}

/// Rough size of the files an export writes besides the audio, for the job's byte total.
fn estimate(root: &Path, snapshot: &Snapshot) -> u64 {
    let old = std::fs::metadata(root.join(DATABASE)).map_or(0, |m| m.len());
    old + snapshot.tracks.len() as u64 * 4096 + snapshot.playlists.len() as u64 * 1024
}

fn export(
    root: &Path,
    snapshot: &Snapshot,
    options: &ExportOptions,
) -> Result<ExportReport, String> {
    if options.dry_run {
        return dry_run(root, snapshot, options);
    }
    if !root.is_dir() {
        return Err(no_folder(root));
    }
    let mut undo = Undo {
        root: root.to_owned(),
        created: Vec::new(),
        had_dir: root.join(commit::DIR).is_dir(),
    };
    let result = run(root, snapshot, options, &mut undo);
    // A change the journal says is switching counts as made: recovery finishes it, so what the run made must stay.
    let switching = commit::pending(root).is_some_and(|j| j.state == commit::State::Switching);
    if result.is_err() && !switching {
        undo.undo();
    }
    result
}

/// The real export: copy the audio, stage every library file in one commit, read them back, switch them.
fn run(
    root: &Path,
    snapshot: &Snapshot,
    options: &ExportOptions,
    undo: &mut Undo,
) -> Result<ExportReport, String> {
    let mut report = ExportReport::default();
    let mut commit = Commit::new(root, OWNER)?;
    let mut doc = load(root)?;
    let mut planned = plan(root, snapshot, options, &mut report);
    if let Some(fatal) = planned.fatal.take() {
        return Err(fatal);
    }
    host::set_bytes_total(planned.need + estimate(root, snapshot));
    let total = planned.slots.iter().flatten().count();
    let mut done = 0;
    for ((t, _), slot) in snapshot.tracks.iter().zip(&mut planned.slots) {
        let Some(slot) = slot else { continue };
        cancelled("Copying audio", done, total)?;
        done += 1;
        if !slot.there {
            if let Some(dir) = root.join(&slot.rel).parent() {
                undo.made(dir);
            }
            let (rel, copied) = place_audio(root, t)?;
            if copied {
                undo.created.push(root.join(&rel));
            }
            let size = std::fs::metadata(root.join(&rel))
                .map_err(|e| format!("{rel}: {e}"))?
                .len();
            *slot = Slot {
                rel,
                there: true,
                size,
            };
        }
    }
    cancelled("Writing the library", 0, 1)?;
    let composed = compose(root, snapshot, &mut doc, &planned.slots, &mut report)?;
    for dir in &composed.dirs {
        undo.made(dir);
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    for (path, text) in &composed.files {
        if let Some(dir) = path.parent() {
            undo.made(dir);
        }
        commit.stage(path, text.as_bytes())?;
        if host::add_bytes(text.len() as u64) {
            return Err("cancelled".into());
        }
    }
    if composed.files.is_empty() {
        return Ok(report);
    }
    cancelled("Checking the library", 0, 1)?;
    check::verify(root, &commit, &composed)?;
    commit.commit()?;
    report.verified = Some(true);
    Ok(report)
}

/// What `export` would do, from the same planning and merging; reads the stick and writes nothing.
fn dry_run(
    root: &Path,
    snapshot: &Snapshot,
    options: &ExportOptions,
) -> Result<ExportReport, String> {
    let mut report = ExportReport::default();
    let mut plan_out = ExportPlan::default();
    let doc = if root.is_dir() {
        load(root)
    } else {
        Err(no_folder(root))
    };
    let mut doc = match doc {
        Ok(d) => d,
        Err(e) => {
            plan_out.blocked.push(e);
            report.plan = Some(plan_out);
            return Ok(report);
        }
    };
    let planned = plan(root, snapshot, options, &mut report);
    let composed = compose(root, snapshot, &mut doc, &planned.slots, &mut report)?;
    for (((t, _), slot), known) in snapshot
        .tracks
        .iter()
        .zip(&planned.slots)
        .zip(&composed.known)
    {
        let Some(slot) = slot else { continue };
        let item = PlanItem {
            key: track_key(t),
            title: t.title.clone(),
            artist: t.artist.clone(),
            bytes: if slot.there { 0 } else { slot.size },
        };
        if *known {
            plan_out.update.push(item);
        } else {
            plan_out.add.push(item);
        }
    }
    plan_out.playlists = report.playlists;
    plan_out.copy_bytes = planned.need;
    plan_out.write_bytes = composed.files.iter().map(|(_, t)| t.len() as u64).sum();
    plan_out.files = composed.files.len();
    plan_out.blocked = planned.blocked;
    report.plan = Some(plan_out);
    Ok(report)
}
