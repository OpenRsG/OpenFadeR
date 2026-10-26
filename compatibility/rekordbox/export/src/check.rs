//! The stick checker's side of the plugin: [`run`] (`device_check`) reads the two libraries of a device and the files they list,
//! read-only and without parsing whole analysis files, and answers what is wrong in cause / consequence / action form, with the
//! `device_fix` id that repairs it; [`fix`] (`device_fix`) runs that repair, each through one [`Commit`].
//!
//! | problem id (`rekordbox-device:`) | severity | fix id |
//! |---|---|---|
//! | `count_mismatch`: the libraries list a different number of tracks | Error | rebuilds the one with fewer tracks from the other |
//! | `missing_library`: one library is not there | Warning | builds it from the other |
//! | `unreadable_library`: one library is there and cannot be read | Unreadable | rebuilds it from the other, when that reads |
//! | `audio_missing`: tracks are listed whose audio file is not there | Error | none (the host copies them again) |
//! | `analysis_missing`: a listed track's analysis files are missing or damaged | Warning | `rewrite_analysis` |
//! | `leftover_files`: staged `*.ofnew` files of a write that never began switching | Info | `clean_leftovers` |
//!
//! A pending journal (`commit::pending`) is the host's to finish, so it is not a problem here and hides the leftovers. The source
//! of a rebuild is the library that lists more tracks (completeness first), the newer one when both list as many (inferred: the
//! libraries only differ after another program wrote one of them).

use super::pdb::{
    db::{Db, Item},
    read as pdb_read,
};
use super::progress::Progress;
use super::run::{complete, CueCheck};
use super::settings::DeviceConfig;
use super::{convert, onelibrary, stage_pdb, verify, wants_onelibrary, EXT, ONE, OWNER, PDB};
use openfader_plugin::library::{
    commit::{self, Commit},
    ActionAnswer, CheckReport, LibraryState, Problem, Severity, Track,
};
use compat_rekordbox_common::{anlz as read_anlz, on_device};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The name of the library in `export.pdb` ([`LibraryState::name`]; `players.json` lists it in `reads`).
pub const DL: &str = "Device Library";
/// The name of the library in `exportLibrary.db`.
pub const OL: &str = "OneLibrary";

const REBUILD_DL: &str = "rebuild_device_library";
const REBUILD_OL: &str = "rebuild_onelibrary";
const REWRITE_ANALYSIS: &str = "rewrite_analysis";
const CLEAN: &str = "clean_leftovers";

/// A library as the check found it.
struct Lib {
    name: &'static str,
    present: bool,
    modified: Option<i64>,
    /// `Err`: why it cannot be read (only when present).
    read: Result<Db, String>,
}

impl Lib {
    fn readable(&self) -> bool {
        self.present && self.read.is_ok()
    }

    fn tracks(&self) -> &[Item] {
        self.read.as_ref().map_or(&[], |d| &d.tracks)
    }

    /// The `device_fix` id that rebuilds this library.
    fn fix(&self) -> &'static str {
        if self.name == DL {
            REBUILD_DL
        } else {
            REBUILD_OL
        }
    }
}

fn load(root: &Path, name: &'static str, key: &str) -> Lib {
    let rel = if name == DL { PDB } else { ONE };
    let path = root.join(rel);
    let meta = std::fs::metadata(&path);
    let present = meta.is_ok();
    let modified = meta
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let read = if !present {
        Err(String::new())
    } else if name == DL {
        pdb_read::open(root, &DeviceConfig::default()).map(|r| r.0)
    } else {
        onelibrary::read(&path, key).map(|r| r.0)
    };
    Lib {
        name,
        present,
        modified,
        read,
    }
}

/// What the analysis files of one track are: the cues of its `.DAT`, `None` when it is missing, damaged or has no `.EXT`.
fn analysis(root: &Path, item: &Item, seen: &mut HashMap<String, Option<usize>>) -> Option<usize> {
    if let Some(known) = seen.get(&item.analyze_path) {
        return *known;
    }
    let dat = on_device(root, &item.analyze_path);
    let ext_ok = !item.analyze_path.is_empty()
        && std::fs::metadata(dat.with_extension("EXT")).is_ok_and(|m| m.len() >= 28);
    let cues = std::fs::read(&dat)
        .ok()
        .and_then(|b| read_anlz::count_cues(&b, None).ok())
        .filter(|_| ext_ok);
    seen.insert(item.analyze_path.clone(), cues);
    cues
}

/// Where `*.ofnew` files of a write that was cut before it began switching can be: the folders a commit stages into.
fn leftovers(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth > 0 {
                    walk(&p, depth - 1, found);
                }
            } else if e.file_name().to_string_lossy().contains(".ofnew") {
                found.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(&root.join("PIONEER"), 0, &mut found);
    walk(&root.join("PIONEER/rekordbox"), 0, &mut found);
    walk(&root.join("PIONEER/USBANLZ"), 2, &mut found);
    walk(&root.join("PIONEER/Artwork"), 1, &mut found);
    found
}

fn state(lib: &Lib, root: &Path, seen: &mut HashMap<String, Option<usize>>) -> LibraryState {
    let db = lib.read.as_ref().ok();
    LibraryState {
        name: lib.name.into(),
        present: lib.present,
        readable: lib.readable(),
        modified: lib.modified,
        tracks: db.map_or(0, |d| d.tracks.len()),
        playlists: db.map_or(0, |d| d.nodes.iter().filter(|n| !n.is_folder).count()),
        cues: db.map_or(0, |d| {
            d.tracks
                .iter()
                .map(|t| analysis(root, t, seen).unwrap_or(0))
                .sum()
        }),
    }
}

/// Which library is rebuilt from which, when one has to be: `(target, source)` and why it is the target.
fn direction<'a>(dl: &'a Lib, ol: &'a Lib, wants_ol: bool) -> Option<(&'a Lib, &'a Lib)> {
    match (dl.readable(), ol.readable()) {
        (true, true) if dl.tracks().len() != ol.tracks().len() => {
            Some(if dl.tracks().len() < ol.tracks().len() {
                (dl, ol)
            } else {
                (ol, dl)
            })
        }
        (true, false) if wants_ol || ol.present => Some((ol, dl)),
        (false, true) => Some((dl, ol)),
        _ => None,
    }
}

/// Reads `root`'s two libraries and reports their state and what is wrong.
pub(crate) fn run(
    root: &Path,
    config: &BTreeMap<String, String>,
    key: &str,
) -> Result<CheckReport, String> {
    let (dl, ol) = (load(root, DL, key), load(root, OL, key));
    let mut seen = HashMap::new();
    let libraries = vec![state(&dl, root, &mut seen), state(&ol, root, &mut seen)];
    let mut problems = Vec::new();
    let id = |what: &str| format!("rekordbox-device:{what}");

    // The libraries against each other.
    let target = direction(&dl, &ol, wants_onelibrary(config));
    if let Some((to, from)) = target {
        let (fewer, most) = (to.tracks().len(), from.tracks().len());
        if to.readable() && from.readable() {
            problems.push(Problem {
                id: id("count_mismatch"),
                severity: Severity::Error,
                library: to.name.into(),
                cause: format!(
                    "The {} of this stick is incomplete ({fewer} tracks instead of {most}).",
                    to.name
                ),
                consequence: format!(
                    "Players that read it would miss {} of your tracks.",
                    most - fewer
                ),
                action: format!(
                    "FIX THE STICK rebuilds it from the {}, which lists the most tracks, in about a minute; your audio files are not touched.",
                    from.name
                ),
                fix: Some(to.fix().into()),
                files: Vec::new(),
            });
        } else if !to.present {
            problems.push(Problem {
                id: id("missing_library"),
                severity: Severity::Warning,
                library: to.name.into(),
                cause: format!("This stick has no {}.", to.name),
                consequence: format!(
                    "Players that read only the {} would show an empty stick.",
                    to.name
                ),
                action: format!(
                    "FIX THE STICK builds it from the {} in about a minute; your audio files are not touched.",
                    from.name
                ),
                fix: Some(to.fix().into()),
                files: Vec::new(),
            });
        }
    }
    for lib in [&dl, &ol] {
        let Err(why) = &lib.read else { continue };
        if !lib.present {
            continue;
        }
        let other = if lib.name == DL { &ol } else { &dl };
        let fix = other.readable().then(|| lib.fix().to_owned());
        problems.push(Problem {
            id: id("unreadable_library"),
            severity: Severity::Unreadable,
            library: lib.name.into(),
            cause: format!("The {} of this stick cannot be read ({why}).", lib.name),
            consequence: "Players that read it would show an empty stick or refuse the stick."
                .into(),
            action: if fix.is_some() {
                format!(
                    "FIX THE STICK builds it from the {} in about a minute; your audio files are not touched.",
                    other.name
                )
            } else {
                "The other library cannot be read either. Export to this stick again to write both libraries anew; your audio files are not touched.".into()
            },
            fix,
            files: Vec::new(),
        });
    }

    // The files the libraries list; the best-read library names the tracks (the other lists the same ones when they agree).
    let listed: Vec<(&'static str, &Item)> = [&dl, &ol]
        .iter()
        .filter(|l| l.readable())
        .flat_map(|l| l.tracks().iter().map(|t| (l.name, t)))
        .collect();
    let mut missing: Vec<(&'static str, &str, &str)> = Vec::new();
    let mut counted = HashSet::new();
    for &(lib, t) in &listed {
        if !counted.insert(t.file_path.to_lowercase()) {
            continue;
        }
        if !on_device(root, &t.file_path).is_file() {
            missing.push((lib, t.file_path.trim_start_matches('/'), &t.title));
        }
    }
    if let Some(&(lib, first, _)) = missing.first() {
        problems.push(Problem {
            id: id("audio_missing"),
            severity: Severity::Error,
            library: lib.into(),
            cause: format!(
                "{} track(s) of the {lib} are listed but their audio file is missing (for example {}).",
                missing.len(),
                first.rsplit('/').next().unwrap_or(first)
            ),
            consequence: "Players list these tracks but cannot play them.".into(),
            action: "Export to this stick again: the missing files are copied from your collection."
                .into(),
            fix: None,
            files: missing.iter().map(|m| m.1.to_owned()).collect(),
        });
    }
    let mut damaged: Vec<&str> = Vec::new();
    let mut counted = HashSet::new();
    for (_, t) in &listed {
        if counted.insert(t.analyze_path.clone()) && analysis(root, t, &mut seen).is_none() {
            damaged.push(&t.title);
        }
    }
    if let Some(first) = damaged.first() {
        problems.push(Problem {
            id: id("analysis_missing"),
            severity: Severity::Warning,
            library: String::new(),
            cause: format!(
                "{} track(s) have no readable analysis files (for example {first}).",
                damaged.len()
            ),
            consequence: "Players show these tracks without waveform, beat grid or cues.".into(),
            action: "FIX THE STICK writes them again from the library or by analysing the audio; your audio files are not touched.".into(),
            fix: Some(REWRITE_ANALYSIS.into()),
            files: Vec::new(),
        });
    }
    if commit::pending(root).is_none() {
        let left = leftovers(root);
        if !left.is_empty() {
            problems.push(Problem {
                id: id("leftover_files"),
                severity: Severity::Info,
                library: String::new(),
                cause: format!(
                    "{} unfinished temporary file(s) of an interrupted write are left on this stick.",
                    left.len()
                ),
                consequence: "Players ignore them; they only take space.".into(),
                action: "FIX THE STICK removes them; your library and audio files are not touched."
                    .into(),
                fix: Some(CLEAN.into()),
                files: Vec::new(),
            });
        }
    }
    Ok(CheckReport {
        libraries,
        problems,
    })
}

/// Runs the repair `fix` names (see the module docs).
pub(crate) fn fix(
    root: &Path,
    fix: &str,
    config: &DeviceConfig,
    key: &str,
) -> Result<ActionAnswer, String> {
    let message = match fix {
        REBUILD_DL => rebuild_device_library(root, config, key)?,
        REBUILD_OL => rebuild_onelibrary(root, config, key)?,
        REWRITE_ANALYSIS => rewrite_analysis(root, key)?,
        CLEAN => clean(root)?,
        _ => return Err(format!("unknown fix {fix}")),
    };
    Ok(ActionAnswer {
        message,
        confirm: None,
    })
}

fn cannot(target: &str, source: &str, why: &str) -> String {
    format!("The {target} cannot be rebuilt: the {source} it is built from cannot be read ({why}).")
}

fn rebuilt(target: &str, source: &str, tracks: usize, playlists: usize) -> String {
    format!(
        "Rebuilt the {target} from the {source}: {tracks} tracks and {playlists} playlists. Your audio files were not touched."
    )
}

/// Builds `export.pdb` (and `exportExt.pdb`) from the OneLibrary.
fn rebuild_device_library(root: &Path, config: &DeviceConfig, key: &str) -> Result<String, String> {
    let mut c = Commit::new(root, OWNER)?;
    let mut progress = Progress::new();
    progress.phase("Reading the library", 1)?;
    let (mut db, tags) = onelibrary::read(&root.join(ONE), key).map_err(|e| cannot(DL, OL, &e))?;
    let device_name = std::mem::take(&mut db.config.device_name);
    db.config = config.clone();
    if db.config.device_name.is_empty() {
        db.config.device_name = device_name;
    }
    // The histories the players wrote to the old Device Library, when it still reads and they are not in the OneLibrary.
    if let Ok((old, _)) = pdb_read::open(root, config) {
        let known: HashSet<u32> = db.tracks.iter().map(|t| t.id).collect();
        for mut h in old.histories {
            if db.histories.iter().all(|x| x.id != h.id) {
                h.tracks.retain(|t| known.contains(t));
                db.histories.push(h);
            }
        }
    }
    // `export.pdb` names the `a` cover: it is copied from the `b` the OneLibrary names where it is missing.
    let mut made = Vec::new();
    let staged = (|| {
        for t in &db.tracks {
            let Some(a) = t.artwork.as_deref() else {
                continue;
            };
            let (a, b) = (
                on_device(root, a),
                on_device(root, &super::onelibrary::image_variant(a)),
            );
            if !a.exists() && b.is_file() {
                let bytes = std::fs::read(&b).map_err(|e| format!("{}: {e}", b.display()))?;
                commit::write_synced(&a, &bytes)?;
                made.push(a);
            }
        }
        progress.phase("Writing the library", 1)?;
        stage_pdb(&mut c, root, &db, &tags)?;
        progress.phase("Checking the library", 1)?;
        verify::staged(&c, &db, &tags, key)
    })();
    if let Err(e) = staged.and_then(|()| c.commit()) {
        if commit::pending(root).is_none() {
            made.iter().for_each(|p| drop(std::fs::remove_file(p)));
        }
        return Err(e);
    }
    Ok(rebuilt(
        DL,
        OL,
        db.tracks.len(),
        db.nodes.iter().filter(|n| !n.is_folder).count(),
    ))
}

/// Builds `exportLibrary.db` from the Device Library ([`convert`], which replaces an existing one).
fn rebuild_onelibrary(root: &Path, config: &DeviceConfig, key: &str) -> Result<String, String> {
    pdb_read::open(root, config).map_err(|e| cannot(OL, DL, &e))?;
    let (tracks, lists) = convert::build(root, key, config)?;
    Ok(rebuilt(OL, DL, tracks, lists))
}

/// Writes the analysis files again for every listed track that has none that read, from what is left of them or the host's
/// analysis of the audio. A track whose audio is gone cannot be analysed and is left (the audio is the host's to copy again).
fn rewrite_analysis(root: &Path, key: &str) -> Result<String, String> {
    let mut c = Commit::new(root, OWNER)?;
    let mut progress = Progress::new();
    progress.phase("Reading the library", 1)?;
    let (dl, ol) = (load(root, DL, key), load(root, OL, key));
    let lib = [&dl, &ol]
        .into_iter()
        .find(|l| l.readable())
        .ok_or_else(|| cannot(DL, OL, "neither library can be read"))?;
    let mut seen = HashMap::new();
    let bad: Vec<&Item> = lib
        .tracks()
        .iter()
        .filter(|t| analysis(root, t, &mut seen).is_none())
        .collect();
    progress.phase("Writing analysis files", bad.len())?;
    let (mut checks, mut skipped, mut warnings) = (Vec::new(), 0, Vec::new());
    for (n, t) in bad.iter().enumerate() {
        progress.tick(n)?;
        let audio = on_device(root, &t.file_path);
        if t.analyze_path.is_empty() || !audio.is_file() {
            skipped += 1;
            continue;
        }
        let dat = on_device(root, &t.analyze_path);
        let track = Track {
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            genre: t.genre.clone(),
            key: t.key.clone(),
            bpm_x100: i64::from(t.bpm_x100),
            length_s: i64::from(t.duration_s),
            bitrate: t.bitrate,
            sample_rate: t.sample_rate,
            file: Some(audio),
            ..Track::default()
        };
        let given = read_anlz::load_track(&dat).ok();
        let a = complete(&track, given.as_ref(), &mut warnings);
        let files = super::anlz::encode(&a, &format!("/{}", t.file_path.trim_start_matches('/')));
        for (ext, bytes) in [
            ("DAT", &files.dat),
            ("EXT", &files.ext),
            ("2EX", &files.ex2),
        ] {
            c.stage(&dat.with_extension(ext), bytes)?;
        }
        checks.push(CueCheck {
            title: t.title.clone(),
            dat: Commit::staged_path(&dat),
            ext: Commit::staged_path(&dat.with_extension("EXT")),
            want: read_anlz::count_cues(&files.dat, Some(&files.ext))?,
        });
    }
    if !checks.is_empty() {
        progress.phase("Checking the library", 1)?;
        verify::cues(&checks)?;
        c.commit()?;
    }
    Ok(if skipped == 0 {
        format!("Rewrote the analysis files of {} track(s).", checks.len())
    } else {
        format!(
            "Rewrote the analysis files of {} track(s); {skipped} could not be rebuilt because their audio file is missing.",
            checks.len()
        )
    })
}

/// Deletes the unfinished temporary files.
fn clean(root: &Path) -> Result<String, String> {
    commit::recover(root)?;
    let mut n = 0;
    for f in leftovers(root) {
        std::fs::remove_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        n += 1;
    }
    Ok(format!("Removed {n} unfinished temporary file(s)."))
}

/// `exportExt.pdb` path, for the tests.
#[doc(hidden)]
pub const MYTAG: &str = EXT;
