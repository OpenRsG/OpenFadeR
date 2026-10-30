//! The export: plan what goes where, copy the audio, tag it, stage `database V2` and the crates, read them back and switch them.
//! A dry run ([`ExportOptions::dry_run`]) stops after the plan and writes nothing, not even a folder. The guarantees are in the
//! crate doc.

use crate::tag::{self, markers, Edit, Tags};
use crate::write::{write_err, Writer, CANCELLED};
use crate::{crate_file, database, OWNER};
use openfader_plugin::library::{
    audio_slot,
    commit::{self, Commit},
    host, track_key, ExportOptions, ExportPlan, ExportReport, PlanItem, Snapshot, Track,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Where a track's private copy lives (below the drive root), see the crate doc.
const PRIVATE: &str = "_Serato_/Audio";

fn private_rel(rel: &str) -> String {
    format!("{PRIVATE}/{}", rel.strip_prefix("Contents/").unwrap_or(rel))
}

/// The private copy of the audio at `rel` (`rel` itself when it already is one).
fn private_of(rel: &str) -> String {
    if rel.starts_with("_Serato_/") {
        rel.to_string()
    } else {
        private_rel(rel)
    }
}

/// Why a track's tags are not written.
enum Why {
    Type,
    Io(String),
    /// The private copy would be over the file size the stick holds.
    Big(u64),
}

impl Why {
    fn warning(&self, title: &str) -> String {
        match self {
            Why::Type => format!("{title}: its file type cannot hold Serato tags, so its cues, loops and grid are not written and the track plays without them. Use an MP3, FLAC, M4A or WAV file if you need them in Serato."),
            Why::Io(e) => format!("{title}: its audio file could not be read for tagging ({e}), so its cues, loops and grid are not written. Check that the file is there and export again."),
            Why::Big(max) => format!("{title}: a tagged private copy would be over the {} this stick's file system holds, so its cues, loops and grid are not written. Convert the track to a smaller file or use an exFAT stick.", size_text(*max)),
        }
    }
}

/// How a track's tags reach Serato.
enum Route {
    /// Edits of the shared copy, which keeps its size.
    Shared(Vec<Edit>),
    /// Into the track's private copy (made first when `copy`).
    Private { copy: bool },
    /// The shared copy stays as it is.
    Plain(Why),
}

/// `src` is the file whose head tells whether the tags fit: the shared copy, or the host's file before it is copied.
fn route(
    root: &Path,
    rel: &str,
    src: &Path,
    tags: &Tags,
    size: u64,
    max_file: Option<u64>,
) -> Route {
    if rel.starts_with("_Serato_/") || root.join(private_rel(rel)).is_file() {
        return Route::Private { copy: false };
    }
    match tag::plan(src, tags, true) {
        Ok(edits) => Route::Shared(edits),
        Err(tag::Error::NoRoom) => {
            // The private copy is the audio plus what the tags add to it.
            let grown = tag::plan(src, tags, false).map_or(size as i64, |edits| {
                size as i64
                    + edits
                        .iter()
                        .map(|e| e.data.len() as i64 - (e.end - e.start) as i64)
                        .sum::<i64>()
            });
            match max_file.filter(|&m| grown > m as i64) {
                Some(max) => Route::Plain(Why::Big(max)),
                None => Route::Private { copy: true },
            }
        }
        Err(tag::Error::Unsupported) => Route::Plain(Why::Type),
        Err(tag::Error::Io(e)) => Route::Plain(Why::Io(e)),
    }
}

/// Unix seconds of the file's last change.
fn mtime(p: &Path) -> u32 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as u32)
}

/// Size as a person reads it: "4.3 GiB", "312.0 MiB".
fn size_text(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MiB", b as f64 / (1u64 << 20) as f64)
    }
}

/// Names the file systems of Windows and FAT refuse whatever the characters (`clean_name` cannot repair them).
fn reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit() && c != '0')
}

const UNREADABLE_DB: &str = "The Serato library on this stick (_Serato_/database V2) cannot be read, so the export did not change anything. Rename the file on the stick (for example to database V2 old), then export again; your audio files are not touched.";

fn mismatch(what: impl std::fmt::Display) -> String {
    format!("The new Serato library did not read back as written ({what}). Nothing on the stick was changed. Export again; if this repeats, check the stick for errors.")
}

/// One track that goes into the library.
struct Item {
    /// Index in the snapshot.
    i: usize,
    /// Where the shared copy of the audio is (or will be) below the root.
    rel: String,
    /// The shared copy is on the device already, or an earlier track of this run copies it.
    there: bool,
    size: u64,
}

/// What an export would do, from the device as it is.
struct Plan {
    items: Vec<Item>,
    /// The tags of each snapshot track and how many memory cues Serato cannot hold.
    tags: Vec<(Tags, usize)>,
    warnings: Vec<String>,
    /// What stops the export or leaves tracks out, one text each: the cause, what it means, what to do.
    blocked: Vec<String>,
    /// The blocked text that stops the whole export; the other lines only leave tracks out.
    fatal: Option<String>,
    old_db: Option<Vec<u8>>,
    old_paths: HashSet<String>,
    max_file: Option<u64>,
    /// Audio bytes to copy (the shared copies and the private ones).
    copy_bytes: u64,
    /// Tags, `database V2` and crates (an estimate).
    write_bytes: u64,
    /// Files created or rewritten (an estimate).
    files: usize,
    /// Tracks that get a private copy.
    privates: usize,
    /// The library path each snapshot track gets, as the plan sees it.
    target: Vec<Option<String>>,
    /// `(path, snapshot index, bytes to copy)` per library path (the last track of a path wins).
    paths: Vec<(String, usize, u64)>,
}

/// The audio and tags of every track, the crates and the room: reads the device, writes nothing.
fn plan(root: &Path, snap: &Snapshot, opts: &ExportOptions) -> Result<Plan, String> {
    let colors: HashMap<usize, u8> = snap.colors.iter().copied().collect();
    let tags: Vec<(Tags, usize)> = snap
        .tracks
        .iter()
        .enumerate()
        .map(|(i, (_, a))| Tags::new(a.as_ref(), colors.get(&i).copied()))
        .collect();
    let mut p = Plan {
        items: Vec::new(),
        tags,
        warnings: Vec::new(),
        blocked: Vec::new(),
        fatal: None,
        old_db: None,
        old_paths: HashSet::new(),
        max_file: opts.max_file,
        copy_bytes: 0,
        write_bytes: 0,
        files: 0,
        privates: 0,
        target: vec![None; snap.tracks.len()],
        paths: Vec::new(),
    };

    match fs::read(root.join(database::FILE)) {
        Ok(b) if !b.is_empty() => match database::open(&b) {
            Ok(paths) => (p.old_paths, p.old_db) = (paths.into_iter().collect(), Some(b)),
            Err(_) => p.fatal = Some(UNREADABLE_DB.into()),
        },
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => p.fatal = Some(UNREADABLE_DB.into()),
        _ => {}
    }

    let mut copying = HashSet::new();
    let mut entries: Vec<(String, usize, u64)> = Vec::new();
    for (i, (t, _)) in snap.tracks.iter().enumerate() {
        let missing = format!("{}: audio file missing, track left out", t.title);
        let Some(file) = &t.file else {
            p.warnings.push(missing);
            continue;
        };
        let (rel, mut there) = match audio_slot(root, t) {
            Ok(v) => v,
            Err(_) => {
                p.warnings.push(missing);
                continue;
            }
        };
        let src = if there { root.join(&rel) } else { file.clone() };
        let size = match fs::metadata(&src) {
            Ok(m) => m.len(),
            Err(_) => {
                p.warnings.push(missing);
                continue;
            }
        };
        // Two tracks of one audio file: the first copies it.
        if !there && !copying.insert(rel.clone()) {
            there = true;
        }
        if !there {
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(bad) = rel.split('/').find(|c| reserved(c)) {
                p.warnings.push(format!(
                    "{}: the file name is reserved by the file system of the stick; track left out",
                    t.title
                ));
                p.blocked.push(format!("The file system of this stick cannot hold a file called {bad}: the name is reserved. It would be left out of the export; rename the file."));
                continue;
            }
            if let Some(max) = opts.max_file.filter(|&m| size > m) {
                p.warnings.push(format!("{}: the audio file is {}, over the {} this stick's file system holds; track left out", t.title, size_text(size), size_text(max)));
                p.blocked.push(format!("{name} is {} but the file system of this stick holds files up to {}. It would be left out of the export; convert it to FLAC or use an exFAT stick.", size_text(size), size_text(max)));
                continue;
            }
        }

        let tag_set = &p.tags[i].0;
        let r = route(root, &rel, &src, tag_set, size, opts.max_file);
        let sum = |e: &[Edit]| e.iter().map(|e| e.data.len() as u64).sum::<u64>();
        let (path, tag_bytes, copy_private) = match &r {
            Route::Shared(e) => (rel.clone(), sum(e), false),
            Route::Private { copy } => {
                let at = if *copy {
                    src.clone()
                } else {
                    root.join(private_of(&rel))
                };
                let bytes = tag::plan(&at, tag_set, false).map_or(0, |e| sum(&e));
                (private_of(&rel), bytes, *copy)
            }
            Route::Plain(why) => {
                p.warnings.push(why.warning(&t.title));
                (rel.clone(), 0, false)
            }
        };
        let bytes = if there { 0 } else { size } + if copy_private { size } else { 0 };
        p.copy_bytes += bytes;
        p.write_bytes += tag_bytes;
        p.files += usize::from(!there) + usize::from(copy_private) + usize::from(tag_bytes > 0);
        p.privates += usize::from(copy_private);
        p.target[i] = Some(path.clone());
        entries.push((path, i, bytes));
        p.items.push(Item {
            i,
            rel,
            there,
            size,
        });
    }

    // One library path, one record: the last track of a path wins, the bytes of all of them are copied.
    let mut at: HashMap<String, usize> = HashMap::new();
    for (path, i, bytes) in entries {
        match at.get(&path) {
            Some(&k) => p.paths[k] = (path, i, p.paths[k].2 + bytes),
            None => {
                at.insert(path.clone(), p.paths.len());
                p.paths.push((path, i, bytes));
            }
        }
    }

    let crates = crates(root, snap, &p.target);
    p.files += crates.len() + 1;
    p.write_bytes += crates.iter().map(|c| c.bytes.len() as u64).sum::<u64>();
    // The new database holds the old one, its `vrsn` header and a record per path (an over-estimate: records of paths it holds
    // are replaced; the 64 bytes per record cover the size and time texts, which are not known before the files are written).
    p.write_bytes += 256
        + p.old_db.as_ref().map_or(0, |b| b.len() as u64)
        + p.paths
            .iter()
            .map(|(path, i, _)| 64 + database::track(path, &snap.tracks[*i].0, 0, 0).len() as u64)
            .sum::<u64>();

    let need = p.copy_bytes + p.write_bytes;
    if let Some(free) = opts.free.filter(|&f| need > f) {
        let text = format!("Not enough space on the stick: about {} are needed and {} are free. Nothing is written; free some space or export fewer tracks.", size_text(need), size_text(free));
        p.blocked.push(text.clone());
        p.fatal.get_or_insert(text);
    }
    if let Some(f) = p.fatal.clone().filter(|f| !p.blocked.contains(f)) {
        p.blocked.push(f);
    }
    Ok(p)
}

/// Notes the user should read after an export.
fn notes(privates: usize, memory_cues: usize) -> Vec<String> {
    let mut notes = Vec::new();
    if privates > 0 {
        notes.push(format!(
            "{privates} tracks got a private copy under {PRIVATE}: their cues, loops and grid did not fit the shared copy without changing its size"
        ));
    }
    if memory_cues > 0 {
        notes.push(format!(
            "{memory_cues} memory cues were not written: Serato has no memory cues"
        ));
    }
    notes
}

/// A crate file the export writes.
struct Crate {
    rel: String,
    members: Vec<String>,
    bytes: Vec<u8>,
}

/// The crate files that change: one per playlist, an empty one for a folder that has none; a crate the device holds as it
/// would be written is left out.
fn crates(root: &Path, snap: &Snapshot, target: &[Option<String>]) -> Vec<Crate> {
    let mut all: Vec<Crate> = Vec::new();
    for (i, name) in crate_file::names(&snap.playlists).iter().enumerate() {
        let rel = format!("{}/{name}.crate", crate_file::DIR);
        let folder = snap.playlists[i].is_folder;
        if folder && (root.join(&rel).is_file() || all.iter().any(|c| c.rel == rel)) {
            continue;
        }
        let members: Vec<String> = if folder {
            Vec::new()
        } else {
            snap.members
                .get(i)
                .map_or(&[][..], |m| m)
                .iter()
                .filter_map(|&m| target.get(m)?.clone())
                .collect()
        };
        let bytes = crate_file::build(&members.iter().map(String::as_str).collect::<Vec<_>>());
        let c = Crate {
            rel,
            members,
            bytes,
        };
        match all.iter().position(|o| o.rel == c.rel) {
            Some(k) => all[k] = c,
            None => all.push(c),
        }
    }
    all.retain(|c| fs::read(root.join(&c.rel)).ok().as_deref() != Some(&c.bytes));
    all
}

pub fn run(root: &Path, snap: &Snapshot, opts: &ExportOptions) -> Result<ExportReport, String> {
    if opts.dry_run {
        return Ok(dry_run(snap, plan(root, snap, opts)?));
    }
    let had_dir = root.join(commit::DIR).is_dir();
    // An interrupted change is recovered before the library is read.
    let commit = Commit::new(root, OWNER).map_err(write_err)?;
    let plan = plan(root, snap, opts)?;
    if let Some(fatal) = plan.fatal.clone() {
        return Err(fatal);
    }
    let mut w = Writer::new(root, commit, had_dir);
    let out = execute(root, snap, &plan, &mut w);
    if out.is_err() && !w.switching {
        w.undo();
    }
    out
}

/// What [`run`] would do: the plan and the counts it would reach, nothing written.
fn dry_run(snap: &Snapshot, p: Plan) -> ExportReport {
    let mut out = ExportPlan {
        playlists: snap.playlists.len(),
        copy_bytes: p.copy_bytes,
        write_bytes: p.write_bytes,
        files: p.files,
        blocked: p.blocked,
        ..ExportPlan::default()
    };
    for (path, i, bytes) in &p.paths {
        let t = &snap.tracks[*i].0;
        let item = PlanItem {
            key: track_key(t),
            title: t.title.clone(),
            artist: t.artist.clone(),
            bytes: *bytes,
        };
        if p.old_paths.contains(path) {
            out.update.push(item);
        } else {
            out.add.push(item);
        }
    }
    let memory: usize = p.items.iter().map(|it| p.tags[it.i].1).sum();
    ExportReport {
        tracks: p.paths.len(),
        added: out.add.len(),
        updated: out.update.len(),
        playlists: snap.playlists.len(),
        warnings: p.warnings,
        notes: notes(p.privates, memory),
        plan: Some(out),
        ..ExportReport::default()
    }
}

fn step(phase: &str, done: usize, total: usize) -> Result<(), String> {
    if host::progress(phase, done, total) {
        return Err(CANCELLED.into());
    }
    Ok(())
}

/// Copies the audio, tags it, stages and checks the library files and switches them.
fn execute(root: &Path, snap: &Snapshot, p: &Plan, w: &mut Writer) -> Result<ExportReport, String> {
    host::set_bytes_total(p.copy_bytes + p.write_bytes);
    let total = p.items.len();

    for (n, it) in p.items.iter().enumerate() {
        step("Copying audio", n, total)?;
        if !it.there {
            w.copy_audio(&snap.tracks[it.i].0)?;
        }
    }

    let (mut entries, mut dropped_paths, mut privates, mut memory_cues) =
        (Vec::new(), Vec::new(), 0, 0);
    let mut target: Vec<Option<String>> = vec![None; snap.tracks.len()];
    // path → (cues and loops, grid) the tags written there must read back as.
    let mut tagged: HashMap<String, (usize, bool)> = HashMap::new();
    for (n, it) in p.items.iter().enumerate() {
        step("Writing analysis files", n, total)?;
        let t = &snap.tracks[it.i].0;
        let (tag_set, memory) = &p.tags[it.i];
        memory_cues += memory;
        let shared = root.join(&it.rel);
        let (path, wrote) = match route(root, &it.rel, &shared, tag_set, it.size, p.max_file) {
            Route::Shared(edits) => {
                w.edit(&shared, edits)?;
                (it.rel.clone(), true)
            }
            Route::Plain(_) => (it.rel.clone(), false),
            Route::Private { copy } => {
                let path = private_of(&it.rel);
                let dest = root.join(&path);
                if copy {
                    w.copy_new(&shared, &dest)?;
                    privates += 1;
                }
                w.tag_file(&dest, tag_set, false)?;
                if path != it.rel {
                    dropped_paths.push(it.rel.clone());
                }
                (path, true)
            }
        };
        if wrote && tag_set.touch {
            let want = (
                tag_set.cues.len() + tag_set.loops.len(),
                tag_set.grid.is_some(),
            );
            tagged.insert(path.clone(), want);
        } else {
            tagged.remove(&path);
        }
        let file = root.join(&path);
        let size = fs::metadata(&file).map_or(0, |m| m.len());
        entries.push((
            path.clone(),
            it.i,
            database::track(&path, t, size, mtime(&file)),
        ));
        target[it.i] = Some(path);
    }
    // One record per path: the last track of a path wins.
    let mut seen = HashSet::new();
    entries.reverse();
    entries.retain(|(p, ..)| seen.insert(p.clone()));
    entries.reverse();

    step("Writing the library", 0, 1)?;
    let recs: Vec<(String, Vec<u8>)> = entries
        .iter()
        .map(|(path, _, rec)| (path.clone(), rec.clone()))
        .collect();
    let (db, replaced) = database::merge(p.old_db.as_deref(), &recs, &dropped_paths)
        .map_err(|_| UNREADABLE_DB.to_string())?;
    let crates = crates(root, snap, &target);
    w.make_dirs(&root.join(crate_file::DIR))?;
    for c in &crates {
        w.stage(&root.join(&c.rel), &c.bytes)?;
    }
    w.stage(&root.join(database::FILE), &db)?;

    step("Checking the library", 0, 1)?;
    let written: Vec<(&str, &Track)> = entries
        .iter()
        .map(|(path, i, _)| (path.as_str(), &snap.tracks[*i].0))
        .collect();
    verify(
        root,
        w,
        &written,
        &p.old_paths,
        &dropped_paths,
        &tagged,
        &crates,
    )?;

    w.switching = true;
    let commit = w.commit.take().ok_or_else(|| write_err("no commit"))?;
    commit.commit().map_err(|e| format!("The new Serato library could not be switched in completely ({e}). The stick finishes the change by itself the next time it is connected to OpenFader; until then the library may be incomplete. Connect it again before you play from it."))?;
    Ok(ExportReport {
        tracks: entries.len(),
        added: entries.len() - replaced,
        updated: replaced,
        playlists: snap.playlists.len(),
        warnings: p.warnings.clone(),
        notes: notes(privates, memory_cues),
        verified: Some(true),
        ..ExportReport::default()
    })
}

/// Reads the staged files and the tags back with the plugin's own parsers: the database lists the paths it should (and their
/// titles, artists and albums), every file is there, each crate holds its members, and the tags of each track give back the
/// cues and loops that were encoded. A difference is an `Err`; nothing has been switched yet.
fn verify(
    root: &Path,
    w: &Writer,
    written: &[(&str, &Track)],
    old_paths: &HashSet<String>,
    dropped: &[String],
    tagged: &HashMap<String, (usize, bool)>,
    crates: &[Crate],
) -> Result<(), String> {
    let commit = w.commit.as_ref().ok_or_else(|| write_err("no commit"))?;
    let dropped: HashSet<&str> = dropped.iter().map(String::as_str).collect();
    let mut want: HashSet<&str> = old_paths
        .iter()
        .map(String::as_str)
        .filter(|p| !dropped.contains(p))
        .collect();
    want.extend(written.iter().map(|(p, _)| *p));
    for (target, staged) in commit.staged() {
        let bytes = fs::read(staged).map_err(|e| mismatch(format!("{}: {e}", staged.display())))?;
        if target.ends_with(database::FILE) {
            let got = database::open(&bytes).map_err(mismatch)?;
            let got_set: HashSet<&str> = got.iter().map(String::as_str).collect();
            if got_set != want {
                return Err(mismatch(format!(
                    "{} tracks instead of {}",
                    got_set.len(),
                    want.len()
                )));
            }
            let read: HashMap<String, Track> = database::read(&bytes)
                .map_err(mismatch)?
                .into_iter()
                .collect();
            for (path, t) in written {
                let ok = read.get(*path).is_some_and(|r| {
                    (&r.title, &r.artist, &r.album) == (&t.title, &t.artist, &t.album)
                });
                if !ok {
                    return Err(mismatch(format!("the record of {path} differs")));
                }
            }
        } else if let Some(c) = crates.iter().find(|c| target.ends_with(&c.rel)) {
            let got = crate_file::paths(&bytes).map_err(mismatch)?;
            if got != c.members || c.members.iter().any(|m| !want.contains(m.as_str())) {
                return Err(mismatch(format!("the crate {} differs", c.rel)));
            }
        }
    }
    for (path, _) in written {
        if !root.join(path).is_file() {
            return Err(mismatch(format!("{path} is missing")));
        }
    }
    for (path, &(cues, grid)) in tagged {
        let stored = tag::read(&root.join(path))
            .map_err(|e| mismatch(format!("the tags of {path}: {e:?}")))?;
        let read = stored
            .markers2
            .as_deref()
            .map_or(0, |m| markers::cues(m).len());
        if read != cues || (grid && stored.grid.is_none()) {
            return Err(mismatch(format!(
                "{path} holds {read} cues and loops instead of {cues}"
            )));
        }
    }
    Ok(())
}
