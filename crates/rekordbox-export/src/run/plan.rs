//! Planning: the device path of each snapshot track, what the job would copy and write, and what stops it (the playlist count,
//! the free space, files the file system cannot hold).

use crate::clean;
use crate::merge::{self, MAX_LISTS};
use crate::pdb::db::Db;
use crate::settings::DeviceConfig;
use crate::{EXT, ONE, PDB};
use openfader_plugin::library::{audio_slot, ExportOptions, ExportReport, Snapshot, Track};
use rekordbox_common::at;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Bytes assumed for the three ANLZ files of one track when checking the free space (inferred: ~400 kB measured on 5-minute
/// tracks, rounded up).
const ANLZ_ESTIMATE: u64 = 512 * 1024;
/// Slack for the library files themselves.
const MARGIN: u64 = 2 * 1024 * 1024;

/// One snapshot track that will be written.
pub(super) struct Plan {
    /// Index into [`Snapshot::tracks`].
    pub i: usize,
    /// Device path without a leading `/`; for a new track the slot `place_audio` would use (the run takes the one it used).
    pub rel: String,
    /// Index into [`Db::tracks`] of the device track it is, `None` for a new one.
    pub at: Option<usize>,
    pub size: u64,
    /// Whether the audio is copied (a new track whose file the device does not hold yet).
    pub copy: bool,
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
}

/// Hands out device paths: the first of `name`, `stem (2).ext`, … in a directory that is free, or that holds the same track
/// already (an unclaimed device track of the same size); failing that, an unclaimed device track of the same file name and
/// size anywhere (a library written by another program files its audio under other folder names).
struct Resolver<'a> {
    db: &'a Db,
    /// Lowercased device path → index into [`Db::tracks`].
    by_path: HashMap<String, usize>,
    /// (Lowercased file name, size) → indexes into [`Db::tracks`].
    by_name: HashMap<(String, u64), Vec<usize>>,
    /// Lowercased paths given to new tracks.
    used: HashSet<String>,
    /// Device tracks already matched to a snapshot track.
    claimed: HashSet<usize>,
}

impl<'a> Resolver<'a> {
    fn new(db: &'a Db) -> Self {
        let mut by_name: HashMap<(String, u64), Vec<usize>> = HashMap::new();
        for (i, t) in db.tracks.iter().enumerate() {
            let name = t.file_path.rsplit('/').next().unwrap_or_default();
            by_name
                .entry((name.to_lowercase(), u64::from(t.file_size)))
                .or_default()
                .push(i);
        }
        let by_path = db
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.file_path.trim_start_matches('/').to_lowercase(), i))
            .collect();
        Self {
            db,
            by_path,
            by_name,
            used: HashSet::new(),
            claimed: HashSet::new(),
        }
    }

    /// The path of a track of `size` bytes whose source file is `original` (cleaned: `name`) in directory `dir`, and the device
    /// track it is (`None` = new).
    fn resolve(
        &mut self,
        dir: &str,
        name: &str,
        original: &str,
        size: u64,
    ) -> (String, Option<usize>) {
        let size = size.min(u64::from(u32::MAX));
        let (stem, ext) = name
            .rsplit_once('.')
            .map_or((name, String::new()), |(s, e)| (s, format!(".{e}")));
        for n in 1.. {
            let p = if n == 1 {
                format!("{dir}/{name}")
            } else {
                format!("{dir}/{stem} ({n}){ext}")
            };
            let key = p.to_lowercase();
            match self.by_path.get(&key) {
                Some(&known)
                    if !self.claimed.contains(&known)
                        && u64::from(self.db.tracks[known].file_size) == size =>
                {
                    self.claimed.insert(known);
                    return (p, Some(known));
                }
                Some(_) => {}
                None if !self.used.contains(&key) => break,
                None => {}
            }
        }
        let elsewhere = [original, name].iter().find_map(|n| {
            self.by_name
                .get(&(n.to_lowercase(), size))?
                .iter()
                .copied()
                .find(|i| !self.claimed.contains(i))
        });
        if let Some(known) = elsewhere {
            self.claimed.insert(known);
            let p = self.db.tracks[known].file_path.trim_start_matches('/');
            return (p.to_owned(), Some(known));
        }
        for n in 1.. {
            let p = if n == 1 {
                format!("{dir}/{name}")
            } else {
                format!("{dir}/{stem} ({n}){ext}")
            };
            let key = p.to_lowercase();
            if !self.by_path.contains_key(&key) && self.used.insert(key) {
                return (p, None);
            }
        }
        unreachable!()
    }
}

/// Longest path (characters) a library row holds: `export.pdb` keeps 255 characters of a text ([`crate::pdb::rows::clip`]), and a
/// cut path would name a file that is not there.
const MAX_PATH: usize = 255;

/// `t` as the audio is placed: when `Contents/<artist>/<album>/<file>` would be longer than a library row holds (with room for
/// a ` (2)` suffix), the artist and album folder names are cut so it fits. The file name keeps its (cleaned, at most 120
/// characters) name so the player still finds the audio by it.
pub(super) fn placed(t: &Track) -> Cow<'_, Track> {
    let name = t.file.as_deref().and_then(Path::file_name).map_or_else(
        || "track".to_owned(),
        |n| clean(&n.to_string_lossy(), "track"),
    );
    let (artist, album) = (
        clean(&t.artist, "UnknownArtist"),
        clean(&t.album, "UnknownAlbum"),
    );
    let fixed = "Contents///".len() + name.chars().count();
    let room = MAX_PATH - 8;
    if fixed + artist.chars().count() + album.chars().count() <= room {
        return Cow::Borrowed(t);
    }
    let each = (room.saturating_sub(fixed) / 2).max(1);
    let cut = |s: &str| s.chars().take(each).collect::<String>();
    Cow::Owned(Track {
        artist: cut(&artist),
        album: cut(&album),
        ..t.clone()
    })
}

/// Names the file systems of Windows and FAT refuse whatever the characters (`clean` cannot repair them).
fn reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit() && c != '0')
}

/// Size as a person reads it: "4.3 GiB", "312.0 MiB".
fn size_text(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64)
    } else {
        mib(b)
    }
}

/// What the export would do, see [`make`].
pub(super) struct Planned {
    pub plans: Vec<Plan>,
    /// What stops the export or leaves tracks out, one text each: the cause, what it means, what to do.
    pub blocked: Vec<String>,
    /// The blocked text that stops the whole export (too many lists, no room); the other lines only leave tracks out.
    pub fatal: Option<String>,
    /// Audio bytes to copy.
    pub copy_bytes: u64,
    /// Analysis files, covers and library files (an estimate).
    pub write_bytes: u64,
    /// Files created or rewritten (an estimate).
    pub files: usize,
}

/// A [`Plan`] per snapshot track that has an audio file (a warning in `report` names the others) and what the job would copy and
/// write on the device at `dest`. A track whose audio is over `o.max_file`, or whose name no FAT accepts, is left out (warning
/// and blocked text). The lists over [`MAX_LISTS`] and the bytes over `o.free` are `fatal`. Reads the device, writes nothing.
pub(super) fn make(
    dest: &Path,
    snap: &Snapshot,
    db: &Db,
    o: &ExportOptions,
    config: &DeviceConfig,
    report: &mut ExportReport,
) -> Result<Planned, String> {
    let mut paths = Resolver::new(db);
    let (mut plans, mut blocked) = (Vec::new(), Vec::new());
    // Lowercased slots a new track will fill, so two new tracks of one file share one copy as they do when copied.
    let mut slots: HashSet<String> = HashSet::new();
    for (i, (t, _)) in snap.tracks.iter().enumerate() {
        let Some(file) = t.file.as_deref().filter(|f| f.is_file()) else {
            report
                .warnings
                .push(format!("{}: audio file missing, track left out", t.title));
            continue;
        };
        let size = at(std::fs::metadata(file), file)?.len();
        let original = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(max) = o.max_file.filter(|&m| size > m) {
            report.warnings.push(format!(
                "{original}: the audio file is {}, over the {} this stick's file system holds; track left out",
                size_text(size),
                size_text(max)
            ));
            blocked.push(format!(
                "{original} is {} but the file system of this stick holds files up to {}. It would be left out of the export; convert it to FLAC or use an exFAT stick.",
                size_text(size),
                size_text(max)
            ));
            continue;
        }
        let name = clean(&original, "track");
        if reserved(&name) {
            report.warnings.push(format!(
                "{original}: the file name is reserved by the file system of the stick; track left out"
            ));
            blocked.push(format!(
                "The file system of this stick cannot hold a file called {name}: the name is reserved. It would be left out of the export; rename the file."
            ));
            continue;
        }
        let dir = format!(
            "Contents/{}/{}",
            clean(&t.artist, "UnknownArtist"),
            clean(&t.album, "UnknownAlbum")
        );
        let (rel, known) = paths.resolve(&dir, &name, &original, size);
        // A new track goes where `place_audio` puts it, so the plan and the copy agree and a stick holds one copy of a file.
        let (rel, copy) = match (known, audio_slot(dest, &placed(t))) {
            (None, Ok((slot, there))) => {
                let first = slots.insert(slot.to_lowercase());
                (slot, first && !there)
            }
            (known, _) => (rel, known.is_none()),
        };
        plans.push(Plan {
            i,
            rel,
            at: known,
            size,
            copy,
        });
    }
    let new_lists = merge::playlists(&mut db.nodes.clone(), snap, &vec![None; snap.tracks.len()]);
    let mut fatal = None;
    if db.nodes.len() + new_lists > MAX_LISTS {
        fatal = Some(format!(
            "The number of lists and folders cannot exceed {MAX_LISTS} on one stick ({} there, {new_lists} more to add). Nothing is written; export fewer playlists or remove some from the stick.",
            db.nodes.len()
        ));
    }
    let copy_bytes: u64 = plans.iter().filter(|p| p.copy).map(|p| p.size).sum();
    let covers: HashSet<&Path> = plans
        .iter()
        .filter_map(|p| snap.tracks[p.i].0.artwork.as_deref())
        .collect();
    let cover_bytes: u64 = covers
        .iter()
        .filter_map(|c| std::fs::metadata(c).ok())
        .map(|m| 4 * m.len()) // a and b, each with its _m copy
        .sum();
    let library: u64 = [PDB, EXT, ONE]
        .iter()
        .filter_map(|f| std::fs::metadata(dest.join(f)).ok())
        .map(|m| m.len())
        .sum::<u64>()
        + plans.iter().filter(|p| p.at.is_none()).count() as u64 * 4096
        + MARGIN;
    let write_bytes = plans.len() as u64 * ANLZ_ESTIMATE + cover_bytes + library;
    let files = plans.iter().filter(|p| p.copy).count()
        + 3 * plans.len()
        + 4 * covers.len()
        + 3
        + config.files().len();
    let need = copy_bytes + write_bytes;
    if fatal.is_none() {
        fatal = o.free.filter(|&f| need > f).map(|free| {
            format!(
                "Not enough space on the stick: about {} are needed and {} are free. Nothing is written; free some space or export fewer tracks.",
                mib(need),
                mib(free)
            )
        });
    }
    blocked.extend(fatal.clone());
    Ok(Planned {
        plans,
        blocked,
        fatal,
        copy_bytes,
        write_bytes,
        files,
    })
}
