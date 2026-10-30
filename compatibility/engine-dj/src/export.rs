//! The export in the order it runs: [`plan`] (where each track's audio goes, what fits, what is new: the one piece of code the
//! dry run and the real export share), copy the audio, build the new `m.db` beside the old one, read it back, switch it in with
//! one [`Commit`]. A dry run stops after the plan and touches nothing, not even by opening the library in a way that writes.
//!
//! What a run puts on the device before the commit (the audio, the folders for it, the staged database, the host's
//! `.openfader` folder) is recorded by [`Writer`] and removed again, newest first, when the run fails or is cancelled.

use crate::schema::{self, Old, Version};
use crate::{database, playlists, reader, tracks, OWNER, SCHEMA_OPTION};
use openfader_plugin::library::{
    audio_slot,
    commit::{self, Commit},
    host, place_audio, track_key, ExportOptions, ExportPlan, ExportReport, PlanItem, Snapshot,
    Track,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Megabytes, rounded up, for a message.
fn mb(bytes: u64) -> u64 {
    bytes.div_ceil(1 << 20)
}

/// `Err("cancelled")` when the user cancelled the job.
fn tick(phase: &str, done: usize, total: usize) -> Result<(), String> {
    if host::progress(phase, done, total) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

/// Where one track's audio goes.
struct Slot {
    /// Relative to the root, `/` separators.
    rel: String,
    /// The device holds the audio already (or an earlier track of this run brings it).
    there: bool,
    /// Audio bytes to copy.
    bytes: u64,
    /// Sample rate, 0 when unknown.
    rate: u32,
}

#[derive(Default)]
struct Planned {
    /// One per snapshot track; `None`: left out.
    slots: Vec<Option<Slot>>,
    warnings: Vec<String>,
    /// Why the export cannot run as it is, the fatal reason first.
    blocked: Vec<String>,
    /// The reason the real export stops with.
    fatal: Option<String>,
    copy_bytes: u64,
    /// Estimate of the library file written.
    write_bytes: u64,
    want: Version,
    old: Option<Old>,
}

/// Names Windows and FAT file systems keep for devices (`CON.wav` cannot be created there).
fn reserved(rel: &str) -> bool {
    rel.split('/').any(|c| {
        let stem = c
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end()
            .to_ascii_uppercase();
        matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    })
}

fn stop(mut p: Planned, reason: String) -> Planned {
    p.blocked.insert(0, reason.clone());
    p.fatal = Some(reason);
    p
}

/// Reads the device (never writes it) and the snapshot and decides what the export would do.
fn plan(root: &Path, snapshot: &Snapshot, options: &ExportOptions) -> Planned {
    let db = database(root);
    let p = Planned::default();
    let layout = root.join(crate::LIBRARY).join("m.db");
    if layout.is_file() {
        return stop(
            p,
            format!(
                "The Engine library in {} uses the old two-file format, which this plugin cannot extend. Nothing was changed. Open the stick in Engine DJ once to convert it, then export again.",
                layout.parent().unwrap_or(root).display()
            ),
        );
    }
    let want = match options.config.get(SCHEMA_OPTION) {
        None => schema::CURRENT,
        Some(v) => match schema::parse_new(v) {
            Some(v) => v,
            None => {
                return stop(
                    p,
                    format!(
                        "The schema {v} chosen for new Engine libraries is unknown. Nothing was written. Choose 3.0.0 or 2.18.0 in the settings of this device."
                    ),
                )
            }
        },
    };
    let old = match schema::open_old(&db) {
        Ok(old) => old,
        Err(e) => return stop(p, e),
    };
    let mut p = Planned { want, old, ..p };

    let mut claimed = HashSet::new();
    for (t, a) in &snapshot.tracks {
        let slot = audio_slot(root, t).map_err(|e| format!("{}: skipped, {e}", t.title));
        let slot = slot.and_then(|(rel, there)| {
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_owned();
            let bytes = if there {
                0
            } else {
                t.file
                    .as_deref()
                    .and_then(|f| std::fs::metadata(f).ok())
                    .map_or(0, |m| m.len())
            };
            // A second track with the same new file brings no second copy.
            let there = there || !claimed.insert(rel.clone());
            let left_out = if options.max_file.is_some_and(|m| bytes > m) {
                Some(format!(
                    "{name} is {} MB: this stick holds files up to {} MB. It is left out of the export; convert it to a smaller file, or use a stick formatted for larger files.",
                    mb(bytes),
                    mb(options.max_file.unwrap_or(0))
                ))
            } else if !there && reserved(&rel) {
                Some(format!(
                    "{name} has a name this stick's file system cannot hold. It is left out of the export; rename the file and export again."
                ))
            } else {
                None
            };
            match left_out {
                Some(text) => {
                    p.blocked.push(text.clone());
                    Err(text)
                }
                None => Ok((rel, there, if there { 0 } else { bytes })),
            }
        });
        match slot {
            Ok((rel, there, bytes)) => {
                let rate = if t.sample_rate > 0 {
                    t.sample_rate
                } else {
                    t.file.as_deref().map_or(0, host::sample_rate)
                };
                if rate == 0 && a.is_some() {
                    // The row still goes in; only the performance data is skipped.
                    p.warnings.push(format!(
                        "{}: sample rate unknown, its cues, loops and grid were not written",
                        t.title
                    ));
                }
                p.copy_bytes += bytes;
                p.slots.push(Some(Slot {
                    rel,
                    there,
                    bytes,
                    rate,
                }));
            }
            Err(text) => {
                p.warnings.push(text);
                p.slots.push(None);
            }
        }
    }
    let kept = p.slots.iter().flatten().count() as u64;
    p.write_bytes = std::fs::metadata(&db).map_or(0, |m| m.len()) + 2048 * kept + (64 << 10);
    let need = p.copy_bytes + p.write_bytes;
    if let Some(free) = options.free.filter(|&f| need > f) {
        let text = format!(
            "Not enough space on the stick: about {} MB are needed and {} MB are free. Nothing is written; free some space or choose fewer tracks.",
            mb(need),
            mb(free)
        );
        p = stop(p, text);
    }
    p
}

/// What the export would do, as the plan of a dry run: counts by `Track.path`, as the real export does.
fn dry(snapshot: &Snapshot, mut p: Planned) -> ExportReport {
    let mut known: HashSet<String> = HashSet::new();
    if let Some(old) = &p.old {
        let paths = old
            .conn
            .prepare("SELECT path FROM Track WHERE path IS NOT NULL")
            .and_then(|mut st| {
                st.query_map([], |r| r.get(0))?
                    .collect::<Result<Vec<String>, _>>()
            });
        known.extend(paths.unwrap_or_default());
    }
    let mut plan = ExportPlan::default();
    for ((t, a), slot) in snapshot.tracks.iter().zip(&p.slots) {
        let Some(s) = slot else { continue };
        let item = |bytes| PlanItem {
            key: track_key(t),
            title: t.title.clone(),
            artist: t.artist.clone(),
            bytes,
        };
        if known.insert(format!("../{}", s.rel)) {
            plan.analyse += usize::from(!tracks::has_analysis(a.as_ref(), t.length_s, s.rate));
            plan.add.push(item(s.bytes));
        } else {
            plan.update.push(item(0));
        }
    }
    plan.playlists = snapshot.playlists.len();
    plan.copy_bytes = p.copy_bytes;
    plan.write_bytes = p.write_bytes;
    plan.files = plan.add.iter().filter(|i| i.bytes > 0).count() + usize::from(p.fatal.is_none());
    plan.blocked = std::mem::take(&mut p.blocked);
    ExportReport {
        tracks: plan.add.len() + plan.update.len(),
        added: plan.add.len(),
        updated: plan.update.len(),
        playlists: plan.playlists,
        warnings: p.warnings,
        plan: Some(plan),
        ..Default::default()
    }
}

/// Where a run writes before the commit: everything it makes is recorded so a failed or cancelled run can take it away again.
struct Writer {
    root: PathBuf,
    /// Files and folders this run made, oldest first.
    created: Vec<PathBuf>,
    commit: Option<Commit>,
    /// The device had the host's `.openfader` folder before the run (it stays).
    had_dir: bool,
}

impl Writer {
    /// Creates `dir`, recording the folders that were missing.
    fn make_dirs(&mut self, dir: &Path) -> Result<(), String> {
        let mut missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| !d.exists())
            .map(Path::to_path_buf)
            .collect();
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        missing.reverse();
        self.created.extend(missing);
        Ok(())
    }

    /// Removes what the run made: the device is as it was.
    fn undo(&mut self) {
        self.commit = None; // drops the staged files and the journal
        if !self.had_dir {
            // Before the folders above it: it was made inside the root.
            let _ = std::fs::remove_dir(self.root.join(commit::DIR));
        }
        for p in self.created.iter().rev() {
            let _ = if p.is_dir() {
                std::fs::remove_dir(p)
            } else {
                std::fs::remove_file(p)
            };
        }
    }
}

/// Writes `snapshot` to the device at `root`, or (a dry run) says what it would write.
pub fn export(
    root: &Path,
    snapshot: &Snapshot,
    options: &ExportOptions,
) -> Result<ExportReport, String> {
    if options.dry_run {
        return Ok(dry(snapshot, plan(root, snapshot, options)));
    }
    let had_dir = root.join(commit::DIR).is_dir();
    // An interrupted change is recovered before the library is read.
    let commit = Commit::new(root, OWNER)?;
    let mut p = plan(root, snapshot, options);
    if let Some(fatal) = p.fatal.take() {
        return Err(fatal);
    }
    let mut w = Writer {
        root: root.to_owned(),
        created: Vec::new(),
        commit: Some(commit),
        had_dir,
    };
    match run(&mut w, snapshot, p) {
        Err(e) => {
            // Once the switching journal is on the device the new library is the one in use (a recovery finishes it).
            let switching =
                commit::pending(root).is_some_and(|j| j.state == commit::State::Switching);
            if !switching {
                w.undo();
            }
            Err(e)
        }
        ok => ok,
    }
}

fn run(w: &mut Writer, snapshot: &Snapshot, p: Planned) -> Result<ExportReport, String> {
    let root = w.root.clone();
    let db = database(&root);
    let Planned {
        mut slots,
        warnings,
        copy_bytes,
        write_bytes,
        want,
        old,
        ..
    } = p;
    let mut report = ExportReport {
        warnings,
        ..Default::default()
    };
    host::set_bytes_total(copy_bytes + write_bytes);

    let total = slots.iter().flatten().count();
    let mut done = 0;
    for (slot, (t, _)) in slots.iter_mut().zip(&snapshot.tracks) {
        let Some(s) = slot.as_mut() else { continue };
        tick("Copying audio", done, total)?;
        done += 1;
        if !s.there {
            if let Some(dir) = root.join(&s.rel).parent() {
                w.make_dirs(dir)?;
            }
        }
        match place_audio(&root, t) {
            Ok((rel, copied)) => {
                if copied {
                    w.created.push(root.join(&rel));
                }
                s.rel = rel;
            }
            Err(e) if e == "cancelled" => return Err(e),
            Err(e) => {
                report.warnings.push(format!("{}: skipped, {e}", t.title));
                *slot = None;
            }
        }
    }

    if let Some(dir) = db.parent() {
        w.make_dirs(dir)?;
    }
    w.created.push(Commit::staged_path(&db));
    let mut staged = schema::stage(&db, want, old)?;
    if staged.version != want && want != schema::CURRENT {
        report.notes.push(format!(
            "the library on the device has schema {}, so it was extended as it is; {} applies to a new library only",
            schema::text(staged.version),
            schema::text(want)
        ));
    }
    let db_err = |e: rusqlite::Error| format!("{}: {e}", db.display());
    let before: i64 = staged
        .conn
        .query_row(
            "SELECT COUNT(*) FROM Track WHERE COALESCE(path, '') <> ''",
            [],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    let edit_time = schema::has_edit_time(staged.version);
    let uuid = staged.uuid.clone();
    let tx = staged.conn.transaction().map_err(db_err)?;
    let mut ids: Vec<Option<i64>> = vec![None; snapshot.tracks.len()];
    // Cues a reader must find again, by `Track.id`.
    let mut cues: HashMap<i64, usize> = HashMap::new();
    let (mut dropped, mut unanalysed) = (0, 0);
    let mut done = 0;
    for (i, ((t, a), slot)) in snapshot.tracks.iter().zip(&slots).enumerate() {
        let Some(s) = slot else { continue };
        tick("Writing the library", done, total)?;
        done += 1;
        let file = root.join(&s.rel);
        let placed = tracks::Placed {
            track: t,
            analysis: a.as_ref(),
            path: format!("../{}", s.rel),
            file: &file,
            rate: s.rate,
            edit_time,
        };
        let (id, wrote) = tracks::upsert(&tx, &uuid, &placed).map_err(db_err)?;
        ids[i] = Some(id);
        if wrote.added {
            report.added += 1;
        } else {
            report.updated += 1;
        }
        if let Some(a) = a.as_ref().filter(|_| s.rate > 0 && t.length_s > 0) {
            cues.insert(id, tracks::expected_cues(a, s.rate));
        }
        dropped += wrote.dropped;
        unanalysed += usize::from(wrote.added && !wrote.analyzed);
    }
    playlists::write(&tx, &uuid, snapshot, &ids).map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    tick("Writing the library", total, total)?;

    tick("Checking the library", 0, 1)?;
    let path = staged.finish()?;
    read_back(&path, snapshot, &ids, before as usize + report.added, &cues)?;
    let len = std::fs::metadata(&path).map_or(0, |m| m.len());
    if host::add_bytes(len) {
        return Err("cancelled".into());
    }
    w.commit.as_mut().ok_or("no commit")?.stage_built(&db)?;
    tick("Checking the library", 1, 1)?;

    w.commit.take().ok_or("no commit")?.commit()?;
    // The old file's journal files would be replayed onto the new one.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(schema::sidecar(&db, suffix));
    }

    report.tracks = report.added + report.updated;
    report.playlists = snapshot.playlists.len();
    report.verified = Some(true);
    if dropped > 0 {
        report.notes.push(format!("{dropped} cues or loops have no place in an Engine library (one main cue, 8 hot cues, 8 loops) and were left out"));
    }
    if unanalysed > 0 {
        report.notes.push(format!(
            "{unanalysed} new tracks have no waveform and grid: the players analyse them when loaded"
        ));
    }
    Ok(report)
}

/// The `Track.id` a track the reader returned carries in its `analysis_ref`.
pub(crate) fn id_of(t: &Track) -> Option<i64> {
    t.analysis_ref.as_deref()?.split_once('\t')?.0.parse().ok()
}

/// Reads the staged library with the plugin's own reader and compares it with what was written: the number of tracks, the tracks
/// of every list written (in order) and the cues, hot cues and loops of every track that got performance data.
fn read_back(
    path: &Path,
    snapshot: &Snapshot,
    ids: &[Option<i64>],
    want_tracks: usize,
    cues: &HashMap<i64, usize>,
) -> Result<(), String> {
    let mismatch = |what: String| {
        format!(
            "The new library did not read back as written ({what}). The old library on the stick was kept unchanged. Try the export again; if it repeats, the stick may be failing."
        )
    };
    let src = reader::open_file(path, false)?;
    let found = src.tracks(None)?;
    if found.len() != want_tracks {
        return Err(mismatch(format!(
            "{} tracks instead of {want_tracks}",
            found.len()
        )));
    }
    let by_path: HashMap<Vec<String>, String> = {
        let mut stack: Vec<String> = Vec::new();
        src.playlists()?
            .into_iter()
            .map(|l| {
                stack.truncate(l.depth);
                stack.push(l.name);
                (stack.clone(), l.id)
            })
            .collect()
    };
    // A list named twice in the snapshot ends up with the tracks of the last one.
    let mut want_lists: HashMap<Vec<String>, (&str, Option<Vec<i64>>)> = HashMap::new();
    let mut stack: Vec<String> = Vec::new();
    for (list, members) in snapshot.playlists.iter().zip(&snapshot.members) {
        stack.truncate(list.depth);
        stack.push(playlists::title(&list.name));
        let mut seen = HashSet::new();
        let tracks = (!list.is_folder).then(|| {
            members
                .iter()
                .filter_map(|&m| ids.get(m).copied().flatten())
                .filter(|t| seen.insert(*t))
                .collect()
        });
        want_lists.insert(stack.clone(), (&list.name, tracks));
    }
    for (path, (name, tracks)) in want_lists {
        let Some(id) = by_path.get(&path) else {
            return Err(mismatch(format!("the list {name} is missing")));
        };
        if let Some(want) = tracks {
            let got: Vec<i64> = src
                .tracks(Some(id.as_str()))?
                .iter()
                .filter_map(id_of)
                .collect();
            if got != want {
                return Err(mismatch(format!(
                    "the list {name} has {} tracks instead of {}",
                    got.len(),
                    want.len()
                )));
            }
        }
    }
    for t in &found {
        let Some(want) = id_of(t).and_then(|id| cues.get(&id)) else {
            continue;
        };
        let got = src.analysis(t).map_or(0, |a| a.cues.len());
        if got != *want {
            return Err(mismatch(format!(
                "{}: {got} cues and loops instead of {want}",
                t.title
            )));
        }
    }
    Ok(())
}
