//! The engine behind [`super::export`]: plan which snapshot track is which device track, check the space, copy the audio, write
//! the analysis files, merge the playlists, then stage the library files, read them back and switch them all together (one
//! [`Commit`]). A dry run ([`dry_run`]) stops after the plan and writes nothing, not even a folder.
//!
//! Identity of a track (inferred; rekordbox's own rule is not documented): the device path `Contents/<artist>/<album>/<file>` it
//! would get plus the audio's size. A device track at that path (or at its ` (2)`, ` (3)` collision variants) with the same size
//! is the same track: its row, id and analysis folder are reused and its audio is not copied again. Anything else is new.
//!
//! What a run puts on the device: the audio and analysis files of new tracks are written before the commit, flushed, and removed
//! again when the run fails or is cancelled (`Writer`). A file that exists and changes (the analysis of a track the device holds,
//! a cover, the settings files, the libraries) is staged in the commit, so it switches together with the libraries.
//!
//! Children: `plan` (device paths, sizes and the limits check) and `track` (writing one track).

mod plan;
mod track;

pub(crate) use track::complete;

use super::pdb::{
    self,
    db::{Db, Tags},
};
use super::progress::Progress;
use super::settings::DeviceConfig;
use super::{merge, profile, put, stage_libraries, today, verify, OWNER};
use openfader_plugin::library::{
    audio_slot, commit::Commit, host, place_audio, track_key, ExportOptions, ExportPlan,
    ExportReport, PlanItem, Snapshot,
};
use plan::{Plan, Planned};
use rekordbox_common::at;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use track::Covers;

/// The library to extend; a new one when `dest` has none.
fn open(dest: &Path, config: &DeviceConfig) -> Result<(Db, Tags), String> {
    let (pdb_path, one) = (dest.join(super::PDB), dest.join(super::ONE));
    if pdb_path.exists() {
        return pdb::read::open(dest, config);
    }
    if one.exists() {
        return Err(format!(
            "{} has exportLibrary.db but no export.pdb: only a Device Library can be extended",
            dest.display()
        ));
    }
    Ok((
        Db {
            analyze_date: today(),
            config: config.clone(),
            ..Db::default()
        },
        Tags::default(),
    ))
}

/// Highest `<n>` of the `a<n>.jpg` covers the library names, 0 when none.
fn last_cover(db: &Db) -> usize {
    db.tracks
        .iter()
        .filter_map(|t| {
            t.artwork
                .as_deref()?
                .rsplit_once("/a")?
                .1
                .strip_suffix(".jpg")?
                .parse()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

/// The cues one track's analysis files must give back when they are read from the device: the count of what was encoded.
pub(super) struct CueCheck {
    pub title: String,
    /// Where the `.DAT` and `.EXT` can be read (the staged file, or the file itself when it is new or unchanged).
    pub dat: PathBuf,
    pub ext: PathBuf,
    pub want: usize,
}

/// Where a run writes: a new file goes straight to the device (flushed) and is recorded, so a run that fails removes it again;
/// a file that exists and differs is staged in the commit and switches with the libraries.
pub(super) struct Writer {
    pub dest: PathBuf,
    /// Files and folders this run made, oldest first.
    pub created: Vec<PathBuf>,
    pub commit: Option<Commit>,
    /// The device had the host's `.openfader` folder before the run (it stays).
    pub had_journal_dir: bool,
}

impl Writer {
    /// Creates `dir`, recording the folders that were missing.
    pub fn make_dirs(&mut self, dir: &Path) -> Result<(), String> {
        let mut missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| !d.exists())
            .map(Path::to_path_buf)
            .collect();
        at(std::fs::create_dir_all(dir), dir)?;
        missing.reverse();
        self.created.extend(missing);
        Ok(())
    }

    /// Puts `bytes` at `path` and counts them in the job's bytes (`Err("cancelled")` when the user cancelled). Returns the
    /// path the bytes can be read back from before the commit: `path` itself, or its staged copy.
    pub fn place(&mut self, path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
        let back = if path.is_file() {
            if std::fs::read(path).is_ok_and(|old| old == bytes) {
                return Ok(path.to_owned()); // nothing to switch
            }
            let commit = self.commit.as_mut().ok_or("no commit")?;
            commit.stage(path, bytes)?;
            Commit::staged_path(path)
        } else {
            if let Some(dir) = path.parent() {
                self.make_dirs(dir)?;
            }
            self.created.push(path.to_owned());
            put(path, bytes)?;
            path.to_owned()
        };
        if host::add_bytes(bytes.len() as u64) {
            return Err("cancelled".into());
        }
        Ok(back)
    }

    /// Removes what the run made: the device is as it was.
    fn undo(&mut self) {
        self.commit = None; // drops the staged files and the journal
        for p in self.created.iter().rev() {
            let _ = if p.is_dir() {
                std::fs::remove_dir(p)
            } else {
                std::fs::remove_file(p)
            };
        }
        if !self.had_journal_dir {
            let _ = std::fs::remove_dir(self.dest.join(openfader_plugin::library::commit::DIR));
        }
    }
}

/// A running job: the library being extended and what the run has made so far.
struct Job<'a> {
    snap: &'a Snapshot,
    dest: &'a Path,
    o: &'a ExportOptions,
    config: &'a DeviceConfig,
    key: &'a str,
    progress: &'a mut Progress,
    /// The snapshot's track colours by track index.
    colors: HashMap<usize, u8>,
    db: Db,
    tags: Tags,
    report: ExportReport,
    w: Writer,
    cue_checks: Vec<CueCheck>,
    /// The commit was asked to switch the files: from then on the new library is the one in use (a recovery finishes it), so
    /// what the run made must stay.
    switching: bool,
}

/// Writes `snap` to `dest`; the report and the device ids of the tracks written.
pub(super) fn run(
    dest: &Path,
    snap: &Snapshot,
    o: &ExportOptions,
    config: &DeviceConfig,
    key: &str,
    progress: &mut Progress,
) -> Result<(ExportReport, Vec<u32>), String> {
    // An interrupted change is recovered before the library is read.
    let had_journal_dir = dest.join(openfader_plugin::library::commit::DIR).is_dir();
    let commit = Commit::new(dest, OWNER)?;
    let (db, tags) = open(dest, config)?;
    let mut report = ExportReport::default();
    let planned = plan::make(dest, snap, &db, o, config, &mut report)?;
    if let Some(fatal) = planned.fatal.clone() {
        return Err(fatal);
    }
    let mut job = Job {
        snap,
        dest,
        o,
        config,
        key,
        progress,
        colors: snap.colors.iter().copied().collect(),
        db,
        tags,
        report,
        w: Writer {
            dest: dest.to_owned(),
            created: Vec::new(),
            commit: Some(commit),
            had_journal_dir,
        },
        cue_checks: Vec::new(),
        switching: false,
    };
    let result = job.execute(planned);
    if result.is_err() && !job.switching {
        job.w.undo();
    }
    result.map(|ids| (job.report, ids))
}

/// What [`run`] would do, from the device as it is: the plan and the counts, nothing written (no folder, no journal).
pub(super) fn dry_run(
    dest: &Path,
    snap: &Snapshot,
    o: &ExportOptions,
    config: &DeviceConfig,
) -> Result<ExportReport, String> {
    let (db, _) = open(dest, config)?;
    let mut report = ExportReport::default();
    let planned = plan::make(dest, snap, &db, o, config, &mut report)?;
    let mut out = ExportPlan {
        playlists: snap.playlists.len(),
        copy_bytes: planned.copy_bytes,
        write_bytes: planned.write_bytes,
        files: planned.files,
        blocked: planned.blocked,
        ..ExportPlan::default()
    };
    for p in &planned.plans {
        let (t, given) = &snap.tracks[p.i];
        let item = PlanItem {
            key: track_key(t),
            title: t.title.clone(),
            artist: t.artist.clone(),
            bytes: if p.copy { p.size } else { 0 },
        };
        if p.at.is_some() {
            out.update.push(item);
        } else {
            out.add.push(item);
        }
        out.analyse += usize::from(track::analyse_needed(t, given.as_ref()));
    }
    (report.added, report.updated) = (out.add.len(), out.update.len());
    report.tracks = report.added + report.updated;
    report.playlists = snap.playlists.len();
    report.plan = Some(out);
    Ok(report)
}

impl Job<'_> {
    /// Copies the audio, writes the analysis files, stages and checks the library files and switches them; the device ids of the
    /// tracks written.
    fn execute(&mut self, mut planned: Planned) -> Result<Vec<u32>, String> {
        let snap = self.snap;
        host::set_bytes_total(planned.copy_bytes + planned.write_bytes);
        let fresh: Vec<usize> = (0..planned.plans.len())
            .filter(|&k| planned.plans[k].at.is_none())
            .collect();
        self.progress.phase("Copying audio", fresh.len())?;
        for (n, &k) in fresh.iter().enumerate() {
            self.progress.tick(n)?;
            self.copy_audio(&mut planned.plans[k])?;
        }
        let plans = &planned.plans;
        let mut ids: Vec<Option<u32>> = vec![None; snap.tracks.len()];
        let mut next_id = self.db.tracks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let mut covers = Covers::new(last_cover(&self.db) + 1);
        let today = today();
        self.progress.phase("Writing analysis files", plans.len())?;
        for (k, plan) in plans.iter().enumerate() {
            self.progress.tick(k)?;
            let id = plan.at.map_or(next_id, |at| self.db.tracks[at].id);
            let item = self.write_track(plan, id, &today, &mut covers)?;
            match plan.at {
                Some(at) => {
                    self.db.tracks[at] = item;
                    self.report.updated += 1;
                }
                None => {
                    self.db.tracks.push(item);
                    next_id += 1;
                    self.report.added += 1;
                }
            }
            ids[plan.i] = Some(id);
        }
        self.report.tracks = self.report.added + self.report.updated;
        self.add_banks(plans, &ids);
        self.progress.phase("Writing the library", 1)?;
        merge::playlists(&mut self.db.nodes, snap, &ids);
        self.report.playlists = snap.playlists.len();
        merge::mytag(&mut self.tags, snap, &ids);
        self.write_device_files()?;
        self.w.make_dirs(&self.dest.join("PIONEER/rekordbox"))?;
        let commit = self.w.commit.as_mut().ok_or("no commit")?;
        stage_libraries(
            commit,
            self.dest,
            &self.db,
            &self.tags,
            self.key,
            super::wants_onelibrary(&self.o.config),
        )?;
        let staged: u64 = commit
            .staged()
            .filter_map(|(_, s)| std::fs::metadata(s).ok())
            .map(|m| m.len())
            .sum();
        if host::add_bytes(staged) {
            return Err("cancelled".into());
        }
        self.progress.phase("Checking the library", 1)?;
        let commit = self.w.commit.as_ref().ok_or("no commit")?;
        verify::staged(commit, &self.db, &self.tags, self.key)?;
        verify::cues(&self.cue_checks)?;
        self.switching = true;
        self.w.commit.take().ok_or("no commit")?.commit()?;
        self.report.verified = Some(true);
        Ok(ids.iter().flatten().copied().collect())
    }

    /// Copies the audio of a new track (flushed to the device, counted in the job's bytes) and takes the place it went to.
    fn copy_audio(&mut self, plan: &mut Plan) -> Result<(), String> {
        let t = plan::placed(&self.snap.tracks[plan.i].0);
        let t = &*t;
        let (rel, there) = audio_slot(self.dest, t)?;
        if !there {
            let target = self.dest.join(&rel);
            if let Some(dir) = target.parent() {
                self.w.make_dirs(dir)?;
            }
            self.w.created.push(target);
        }
        plan.rel = place_audio(self.dest, t)?.0;
        Ok(())
    }

    /// Hot Cue Bank Lists: each bank names the audio file of its track; banks of tracks that did not go to the device are dropped.
    fn add_banks(&mut self, plans: &[Plan], ids: &[Option<u32>]) {
        let (snap, o) = (self.snap, self.o);
        let by_file: HashMap<&Path, u32> = plans
            .iter()
            .filter_map(|p| Some((snap.tracks[p.i].0.file.as_deref()?, ids[p.i]?)))
            .collect();
        for b in &o.banks {
            let cues: Vec<_> = b
                .cues
                .iter()
                .filter_map(|c| by_file.get(c.file.as_path()).map(|&id| (id, c.cue.clone())))
                .collect();
            if cues.len() < b.cues.len() {
                self.report.warnings.push(format!("Hot Cue Bank List {}: {} bank(s) of tracks that are not in this export were left out", b.name, b.cues.len() - cues.len()));
            }
            self.db.banks.push((b.name.clone(), cues));
        }
    }

    /// The device settings files and, when the device has none, the DJ profile.
    fn write_device_files(&mut self) -> Result<(), String> {
        let pioneer = self.dest.join("PIONEER");
        self.w.make_dirs(&pioneer)?;
        for (name, bytes) in self.config.files() {
            self.w.place(&pioneer.join(name), &bytes)?;
        }
        if profile::write(&pioneer, &profile::user())? {
            self.w.created.push(pioneer.join(profile::FILE));
        }
        Ok(())
    }
}
