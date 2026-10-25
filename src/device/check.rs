//! The stick checker: is this device ready for a set, and which players read it?
//!
//! [`gather`] reads the stick (the journal of an interrupted change, each format plugin's `device_check`, or for a plugin that
//! has none the state read from its opened library, the audio files the library lists, the players the plugins ship in
//! `players.json`); [`judge`] is pure: [`Facts`] and a booth profile in, a [`Verdict`] out. [`check`] is both.
//!
//! Texts the host itself finds (an interrupted change, audio files gone, a library that does not open, a check that failed) are
//! [`Problem`]s with an `openfader:` id and their data in `cause` / `files`; the app words them (`app/device/check.rs`) so they are
//! translated. A plugin's problems carry their own translated texts.

use super::Device;
use crate::library::{is_history, Library};
use crate::plugin::{
    self,
    device::{
        self as plugins, commit, BoothPreset, Feature, LibraryState, Player, Problem, Severity,
    },
};
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A change to the stick's libraries was cut off (`cause`: the plugin that was writing). Fixed by recovery.
pub const INTERRUPTED: &str = "openfader:interrupted";
/// Audio files a library lists are not on the stick (`files`, `library`). Fixed by copying them again from the collection.
pub const MISSING_AUDIO: &str = "openfader:missing_audio";
/// A library does not open (`library`, `cause`: the error). Fixed by rebuilding it from the collection.
pub const UNREADABLE: &str = "openfader:unreadable";
/// A plugin's check failed (`cause`: what it said). Nothing to fix.
pub const CHECK_FAILED: &str = "openfader:check_failed";

/// One library on the stick; `None` where only a plugin's own check could tell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lib {
    pub name: String,
    pub present: bool,
    pub readable: bool,
    /// Unix seconds it was last written.
    pub modified: Option<i64>,
    pub tracks: Option<usize>,
    pub playlists: Option<usize>,
    pub cues: Option<usize>,
}

impl From<LibraryState> for Lib {
    fn from(s: LibraryState) -> Self {
        Self {
            name: s.name,
            present: s.present,
            readable: s.readable,
            modified: s.modified,
            tracks: Some(s.tracks),
            playlists: Some(s.playlists),
            cues: Some(s.cues),
        }
    }
}

/// What was read from a stick: the input of [`judge`].
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Kernel file system name and partition table (`dos`, `gpt`, empty = unknown).
    pub fs: String,
    pub table: String,
    pub libs: Vec<Lib>,
    pub problems: Vec<Problem>,
    pub players: Vec<Player>,
    pub presets: Vec<BoothPreset>,
}

/// How the stick stands overall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overall {
    Ready,
    NeedsFixing,
    Unreadable,
    /// No library at all: nothing to check yet.
    Empty,
}

impl Overall {
    /// The exit code of `openfader verify`.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Ready => 0,
            Self::NeedsFixing | Self::Empty => 1,
            Self::Unreadable => 2,
        }
    }
}

/// Why a player cannot use the stick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// It cannot read this file system (kernel name).
    FileSystem(String),
    /// It cannot read this partition table.
    Table(String),
    /// It needs one of these libraries and the stick has none.
    NoLibrary(Vec<String>),
    /// The library (name) is there but does not open.
    Unreadable(String),
    /// The library (name) has a problem a player would trip over.
    Damaged(String),
}

/// What one player model makes of the stick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    Ok,
    /// Everything fits but the maker's documentation or a test has not confirmed this model yet.
    Unconfirmed,
    No(Why),
    /// A model of the booth profile no plugin knows.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerFit {
    pub model: String,
    pub fit: Fit,
    /// The one line of `players.json` about it.
    pub note: String,
}

/// What FIX THE STICK does, in this order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Finish or undo the interrupted change.
    Recover,
    /// The plugin's own repair (`device_fix`).
    Plugin { plugin: String, fix: String },
    /// Copy these audio files (relative to the root) from the collection again.
    Recopy(Vec<String>),
    /// Write the libraries again from the collection.
    Rebuild,
}

#[derive(Clone, Debug)]
pub struct Verdict {
    pub overall: Overall,
    pub libs: Vec<Lib>,
    /// Worst first.
    pub problems: Vec<Problem>,
    /// The booth's models (every known model when the booth is empty).
    pub players: Vec<PlayerFit>,
    /// Every model any plugin knows, for the booth picker.
    pub known: Vec<String>,
    pub presets: Vec<BoothPreset>,
}

impl Verdict {
    /// What FIX THE STICK would do; empty: nothing the host or a plugin can repair.
    pub fn steps(&self) -> Vec<Step> {
        let mut steps = Vec::new();
        if self.problems.iter().any(|p| p.id == INTERRUPTED) {
            steps.push(Step::Recover);
        }
        let (mut files, mut rebuild) = (Vec::<String>::new(), false);
        let mut ps: Vec<&Problem> = self
            .problems
            .iter()
            .filter(|p| p.id != INTERRUPTED)
            .collect();
        ps.sort_by_key(|p| Reverse(p.severity));
        for p in ps {
            match (&p.fix, p.files.is_empty()) {
                (Some(fix), _) => {
                    let s = Step::Plugin {
                        plugin: p.id.split(':').next().unwrap_or_default().to_owned(),
                        fix: fix.clone(),
                    };
                    if !steps.contains(&s) {
                        steps.push(s);
                    }
                }
                (None, false) => files.extend(
                    p.files
                        .iter()
                        .filter(|f| !files.contains(f))
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                (None, true) => rebuild |= p.severity >= Severity::Error && p.id != CHECK_FAILED,
            }
        }
        files.dedup();
        if !files.is_empty() {
            steps.push(Step::Recopy(files));
        }
        if rebuild {
            steps.push(Step::Rebuild);
        }
        steps
    }

    /// The models of the booth the stick cannot be used with.
    pub fn refused(&self) -> impl Iterator<Item = &PlayerFit> {
        self.players.iter().filter(|p| matches!(p.fit, Fit::No(_)))
    }
}

/// `msdos`, `fat`, `fat32` are `vfat`.
fn norm_fs(fs: &str) -> String {
    match fs.to_ascii_lowercase().as_str() {
        "msdos" | "fat" | "fat32" | "vfat" => "vfat".into(),
        other => other.into(),
    }
}

fn fit(p: &Player, f: &Facts) -> Fit {
    let fs = norm_fs(&f.fs);
    if !fs.is_empty()
        && !p.file_systems.is_empty()
        && !p.file_systems.iter().any(|x| norm_fs(x) == fs)
    {
        return Fit::No(Why::FileSystem(fs));
    }
    if !f.table.is_empty()
        && !p.partition_tables.is_empty()
        && !p
            .partition_tables
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&f.table))
    {
        return Fit::No(Why::Table(f.table.clone()));
    }
    let broken = |name: &str| {
        f.problems
            .iter()
            .any(|q| q.library == name && q.severity >= Severity::Error)
    };
    let lib = |name: &str| f.libs.iter().find(|l| l.present && l.name == name);
    if !p.reads.is_empty()
        && !p
            .reads
            .iter()
            .any(|n| lib(n).is_some_and(|l| l.readable) && !broken(n))
    {
        return Fit::No(match p.reads.iter().find(|n| lib(n).is_some()) {
            Some(n) if !lib(n).is_some_and(|l| l.readable) => Why::Unreadable(n.clone()),
            Some(n) => Why::Damaged(n.clone()),
            None => Why::NoLibrary(p.reads.clone()),
        });
    }
    if p.status == "confirmed" {
        Fit::Ok
    } else {
        Fit::Unconfirmed
    }
}

/// The verdict for `f` and the booth profile `booth` (player models; empty: every model known).
pub fn judge(f: &Facts, booth: &[String]) -> Verdict {
    let players: Vec<PlayerFit> = if booth.is_empty() {
        f.players
            .iter()
            .map(|p| (p.model.as_str(), Some(p)))
            .collect::<Vec<_>>()
    } else {
        booth
            .iter()
            .map(|m| {
                (
                    m.as_str(),
                    f.players.iter().find(|p| p.model.eq_ignore_ascii_case(m)),
                )
            })
            .collect()
    }
    .into_iter()
    .map(|(model, p)| PlayerFit {
        model: p.map_or(model, |p| p.model.as_str()).to_owned(),
        fit: p.map_or(Fit::Unknown, |p| fit(p, f)),
        note: p.map(|p| p.note.clone()).unwrap_or_default(),
    })
    .collect();
    let mut problems = f.problems.clone();
    problems.sort_by_key(|p| Reverse(p.severity));
    let at = |s: Severity| problems.iter().any(|p| p.severity == s);
    let empty = !f.libs.iter().any(|l| l.present) && !problems.iter().any(|p| p.id == INTERRUPTED);
    // A stick no player of the booth reads, whatever its libraries: the file system or the partition table is the cause.
    let device_no = |p: &PlayerFit| matches!(p.fit, Fit::No(Why::FileSystem(_) | Why::Table(_)));
    let all_no = |ok: fn(&PlayerFit) -> bool| !players.is_empty() && players.iter().all(ok);
    // Without a booth profile the stick is judged on its own: a library no known player reads (one made for software on a
    // laptop) is not a fault, a file system or partition table no player reads is.
    let booth_refused = if booth.is_empty() {
        all_no(device_no)
    } else {
        players.iter().any(|p| matches!(p.fit, Fit::No(_)))
    };
    let overall = if at(Severity::Unreadable)
        || (f.libs.iter().any(|l| l.present) && !f.libs.iter().any(|l| l.present && l.readable))
    {
        Overall::Unreadable
    } else if at(Severity::Error) {
        Overall::NeedsFixing
    } else if empty {
        if players.iter().any(device_no) && (!booth.is_empty() || all_no(device_no)) {
            Overall::NeedsFixing
        } else {
            Overall::Empty
        }
    } else if booth_refused {
        Overall::NeedsFixing
    } else {
        Overall::Ready
    };
    Verdict {
        overall,
        libs: f.libs.clone(),
        problems,
        players,
        known: f.players.iter().map(|p| p.model.clone()).collect(),
        presets: f.presets.clone(),
    }
}

/// Merges the players of every plugin: one entry per model (case-insensitive); a model is confirmed only when each plugin that
/// lists it says so.
fn merge(all: Vec<(Vec<Player>, Vec<BoothPreset>)>) -> (Vec<Player>, Vec<BoothPreset>) {
    let (mut players, mut presets): (Vec<Player>, Vec<BoothPreset>) = (Vec::new(), Vec::new());
    let union = |a: &mut Vec<String>, b: Vec<String>| {
        for x in b {
            if !a.contains(&x) {
                a.push(x);
            }
        }
    };
    for (ps, pre) in all {
        for p in ps {
            match players
                .iter_mut()
                .find(|q| q.model.eq_ignore_ascii_case(&p.model))
            {
                Some(q) => {
                    union(&mut q.reads, p.reads);
                    union(&mut q.file_systems, p.file_systems);
                    union(&mut q.partition_tables, p.partition_tables);
                    if p.status != "confirmed" {
                        q.status = p.status;
                    }
                    if q.note.is_empty() {
                        q.note = p.note;
                    }
                }
                None => players.push(p),
            }
        }
        for b in pre {
            match presets.iter_mut().find(|q| q.id == b.id) {
                Some(q) => union(&mut q.models, b.models),
                None => presets.push(b),
            }
        }
    }
    (players, presets)
}

/// A library as the host opened it, for the state of a plugin that cannot check itself.
pub struct Listing {
    /// What the library calls itself (`Library::kind`).
    pub kind: String,
    /// The audio file of each track.
    pub files: Vec<Option<PathBuf>>,
    pub playlists: usize,
}

/// `file` relative to `root` with `/` separators; `None` for a file outside it.
fn relative(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    Some(
        rel.iter()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// The state of the libraries `names` of a plugin without `device_check`, from what its opened library lists: how many tracks and
/// playlists, which audio files are gone. `opened` is the error when the library did not open.
pub fn basic_state(
    plugin: &str,
    names: &[String],
    modified: Option<i64>,
    opened: Result<Listing, String>,
    root: &Path,
) -> (Vec<Lib>, Vec<Problem>) {
    let mut libs: Vec<Lib> = names
        .iter()
        .map(|n| Lib {
            name: n.clone(),
            present: true,
            readable: opened.is_ok(),
            modified,
            ..Lib::default()
        })
        .collect();
    let mut problems = Vec::new();
    match opened {
        Err(e) => problems.extend(libs.iter().map(|l| Problem {
            id: UNREADABLE.into(),
            severity: Severity::Unreadable,
            library: l.name.clone(),
            cause: e.clone(),
            ..Problem::default()
        })),
        Ok(l) => {
            // The counts belong to the library that was opened: the one named like its kind, else the only one.
            let at = names
                .iter()
                .position(|n| {
                    let (n, k) = (n.to_lowercase(), l.kind.to_lowercase());
                    !k.is_empty() && (k.contains(&n) || n.contains(&k))
                })
                .or((names.len() == 1).then_some(0));
            let missing: Vec<String> = l
                .files
                .iter()
                .flatten()
                .filter(|f| !f.is_file())
                .filter_map(|f| relative(root, f))
                .collect();
            if let Some(i) = at {
                libs[i].tracks = Some(l.files.len());
                libs[i].playlists = Some(l.playlists);
                if !missing.is_empty() {
                    problems.push(Problem {
                        id: MISSING_AUDIO.into(),
                        severity: Severity::Error,
                        library: libs[i].name.clone(),
                        cause: plugin.into(),
                        files: missing,
                        ..Problem::default()
                    });
                }
            }
        }
    }
    (libs, problems)
}

fn listing(lib: &dyn Library) -> Result<Listing, String> {
    let files = lib.tracks(None)?.into_iter().map(|t| t.file).collect();
    let playlists = lib
        .playlists()?
        .iter()
        .filter(|p| !p.is_folder && !is_history(&p.id))
        .count();
    Ok(Listing {
        kind: lib.kind().to_owned(),
        files,
        playlists,
    })
}

/// Reads the stick: see the module doc. `config` is the device's settings the plugins' checks get.
pub fn gather(dev: &Device, config: &BTreeMap<String, String>) -> Facts {
    let mut f = Facts {
        fs: dev.fs.clone(),
        table: dev.table.clone(),
        ..Facts::default()
    };
    if let Some(j) = commit::pending(&dev.mount) {
        f.problems.push(Problem {
            id: INTERRUPTED.into(),
            severity: Severity::Error,
            cause: j.owner,
            ..Problem::default()
        });
    }
    for (name, info) in &dev.formats {
        let Some(p) = plugins::provider(name) else {
            continue;
        };
        if info.features.contains(&Feature::Check) {
            match p.check(&dev.mount, config) {
                Ok(r) => {
                    f.libs.extend(r.libraries.into_iter().map(Lib::from));
                    f.problems.extend(r.problems);
                }
                Err(e) => f.problems.push(Problem {
                    id: CHECK_FAILED.into(),
                    severity: Severity::Warning,
                    cause: e,
                    ..Problem::default()
                }),
            }
        } else if !info.libraries.is_empty() {
            let opened = plugin::library::open_with(name, &dev.mount).and_then(|l| listing(&l));
            let (libs, problems) =
                basic_state(name, &info.libraries, info.modified, opened, &dev.mount);
            f.libs.extend(libs);
            f.problems.extend(problems);
        }
    }
    let all = plugins::providers()
        .iter()
        .filter_map(|p| match p.players() {
            Ok(x) => Some((x.players, x.presets)),
            Err(e) => {
                eprintln!("players: {e}");
                None
            }
        })
        .collect();
    (f.players, f.presets) = merge(all);
    f
}

/// The verdict for the stick `dev` and the booth profile `booth`.
pub fn check(dev: &Device, config: &BTreeMap<String, String>, booth: &[String]) -> Verdict {
    judge(&gather(dev, config), booth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(model: &str, reads: &[&str], fs: &[&str], tables: &[&str], status: &str) -> Player {
        Player {
            model: model.into(),
            reads: reads.iter().map(|s| s.to_string()).collect(),
            file_systems: fs.iter().map(|s| s.to_string()).collect(),
            partition_tables: tables.iter().map(|s| s.to_string()).collect(),
            status: status.into(),
            note: format!("{model} note"),
            ..Player::default()
        }
    }

    fn lib(name: &str, readable: bool) -> Lib {
        Lib {
            name: name.into(),
            present: true,
            readable,
            tracks: Some(10),
            ..Lib::default()
        }
    }

    fn problem(id: &str, s: Severity, library: &str) -> Problem {
        Problem {
            id: id.into(),
            severity: s,
            library: library.into(),
            cause: format!("{id} cause"),
            ..Problem::default()
        }
    }

    /// A stick with both rekordbox libraries, FAT32 on an MBR disk, and three players.
    fn facts() -> Facts {
        Facts {
            fs: "vfat".into(),
            table: "dos".into(),
            libs: vec![lib("Device Library", true), lib("OneLibrary", true)],
            problems: vec![],
            players: vec![
                player(
                    "CDJ-2000NXS2",
                    &["Device Library"],
                    &["vfat", "hfsplus"],
                    &["dos"],
                    "confirmed",
                ),
                player(
                    "CDJ-3000X",
                    &["OneLibrary", "Device Library"],
                    &["vfat", "exfat"],
                    &["dos", "gpt"],
                    "confirmed",
                ),
                player(
                    "XDJ-AZ",
                    &["OneLibrary"],
                    &["vfat", "exfat"],
                    &[],
                    "to_measure",
                ),
            ],
            presets: vec![],
        }
    }

    fn fits(v: &Verdict) -> Vec<Fit> {
        v.players.iter().map(|p| p.fit.clone()).collect()
    }

    #[test]
    fn a_healthy_stick_is_ready_and_unmeasured_players_are_not_confirmed() {
        let v = judge(&facts(), &[]);
        assert_eq!(v.overall, Overall::Ready);
        assert_eq!(fits(&v), [Fit::Ok, Fit::Ok, Fit::Unconfirmed]);
        assert_eq!(v.players[2].note, "XDJ-AZ note");
        assert!(v.steps().is_empty());
    }

    #[test]
    fn the_file_system_and_the_partition_table_decide_per_player() {
        let mut f = facts();
        f.fs = "exfat".into();
        let v = judge(&f, &[]);
        assert_eq!(
            fits(&v),
            [
                Fit::No(Why::FileSystem("exfat".into())),
                Fit::Ok,
                Fit::Unconfirmed
            ]
        );
        assert_eq!(
            v.overall,
            Overall::Ready,
            "one player reads it: nobody asked for the others"
        );
        f.fs = "vfat".into();
        f.table = "gpt".into();
        assert_eq!(
            judge(&f, &[]).players[0].fit,
            Fit::No(Why::Table("gpt".into()))
        );
        // Unknown on either side is not a failure.
        f.table = String::new();
        assert_eq!(judge(&f, &[]).players[0].fit, Fit::Ok);
        f.fs = String::new();
        assert_eq!(judge(&f, &[]).players[2].fit, Fit::Unconfirmed);
        // FAT written another way is still FAT32.
        f.fs = "msdos".into();
        assert_eq!(judge(&f, &[]).players[0].fit, Fit::Ok);
        f.fs = "ntfs3".into();
        let v = judge(&f, &[]);
        assert!(fits(&v)
            .iter()
            .all(|x| matches!(x, Fit::No(Why::FileSystem(_)))));
        assert_eq!(v.overall, Overall::NeedsFixing, "no player at all reads it");
    }

    #[test]
    fn a_player_needs_one_of_its_libraries_present_readable_and_sound() {
        let mut f = facts();
        f.libs.retain(|l| l.name == "Device Library");
        let v = judge(&f, &[]);
        assert_eq!(
            fits(&v),
            [
                Fit::Ok,
                Fit::Ok,
                Fit::No(Why::NoLibrary(vec!["OneLibrary".into()]))
            ]
        );
        f.libs[0].readable = false;
        assert_eq!(
            judge(&f, &[]).players[0].fit,
            Fit::No(Why::Unreadable("Device Library".into()))
        );
        f.libs[0].readable = true;
        f.problems
            .push(problem("p:count", Severity::Error, "Device Library"));
        let v = judge(&f, &[]);
        assert_eq!(
            v.players[0].fit,
            Fit::No(Why::Damaged("Device Library".into()))
        );
        assert_eq!(v.overall, Overall::NeedsFixing);
        // A warning does not stop a player; an error on another library does not either.
        f.problems = vec![
            problem("p:old", Severity::Warning, "Device Library"),
            problem("p:x", Severity::Error, "OneLibrary"),
        ];
        let v = judge(&f, &[]);
        assert_eq!(v.players[0].fit, Fit::Ok);
    }

    #[test]
    fn without_a_booth_a_library_no_player_reads_is_no_fault() {
        let mut f = facts();
        f.libs = vec![LibraryState {
            name: "Laptop library".into(),
            present: true,
            readable: true,
            tracks: 3,
            ..Default::default()
        }
        .into()];
        let v = judge(&f, &[]);
        assert!(fits(&v).iter().all(|x| matches!(x, Fit::No(Why::NoLibrary(_)))));
        assert_eq!(v.overall, Overall::Ready, "made for software, not for players");
        let model = f.players[0].model.clone();
        assert_eq!(judge(&f, &[model]).overall, Overall::NeedsFixing, "the booth's player cannot read it");
    }

    #[test]
    fn the_booth_profile_narrows_the_players_and_a_refusal_needs_fixing() {
        let mut f = facts();
        f.fs = "exfat".into();
        let booth = vec!["cdj-2000nxs2".to_owned(), "Mystery 9".to_owned()];
        let v = judge(&f, &booth);
        assert_eq!(v.players.len(), 2);
        assert_eq!(
            v.players[0].model, "CDJ-2000NXS2",
            "the model is named as the plugin names it"
        );
        assert_eq!(v.players[1].fit, Fit::Unknown);
        assert_eq!(
            v.overall,
            Overall::NeedsFixing,
            "a player of the booth cannot read it"
        );
        assert_eq!(v.refused().count(), 1);
        assert_eq!(v.known.len(), 3);
        let v = judge(&f, &["CDJ-3000X".to_owned()]);
        assert_eq!(v.overall, Overall::Ready);
    }

    #[test]
    fn problems_order_the_verdict() {
        let mut f = facts();
        f.problems = vec![
            problem("p:w", Severity::Warning, ""),
            problem("p:i", Severity::Info, ""),
        ];
        assert_eq!(judge(&f, &[]).overall, Overall::Ready);
        f.problems.push(problem("p:e", Severity::Error, ""));
        let v = judge(&f, &[]);
        assert_eq!(v.overall, Overall::NeedsFixing);
        assert_eq!(v.problems[0].id, "p:e", "worst first");
        f.problems
            .push(problem("p:u", Severity::Unreadable, "Device Library"));
        assert_eq!(judge(&f, &[]).overall, Overall::Unreadable);
        // Libraries present but none opens is unreadable even when nobody said so.
        let mut f = facts();
        f.libs.iter_mut().for_each(|l| l.readable = false);
        assert_eq!(judge(&f, &[]).overall, Overall::Unreadable);
        assert_eq!(Overall::Unreadable.exit_code(), 2);
    }

    #[test]
    fn a_stick_without_a_library_is_empty_unless_it_cannot_be_read_by_players() {
        let mut f = facts();
        f.libs.clear();
        let v = judge(&f, &[]);
        assert_eq!(v.overall, Overall::Empty);
        assert_eq!(Overall::Empty.exit_code(), 1);
        assert!(matches!(v.players[0].fit, Fit::No(Why::NoLibrary(_))));
        f.fs = "ntfs".into();
        assert_eq!(judge(&f, &[]).overall, Overall::NeedsFixing);
        // A cut-off change is not empty: there is something to fix.
        let mut f = facts();
        f.libs.clear();
        f.problems.push(problem(INTERRUPTED, Severity::Error, ""));
        assert_eq!(judge(&f, &[]).overall, Overall::NeedsFixing);
    }

    #[test]
    fn fix_steps_run_in_order_and_only_what_can_be_repaired() {
        let mut f = facts();
        let mut by_plugin = problem("rekordbox-device:count", Severity::Error, "Device Library");
        by_plugin.fix = Some("rebuild_pdb".into());
        let mut warn = problem("rekordbox-device:old", Severity::Warning, "OneLibrary");
        warn.fix = Some("refresh".into());
        let mut gone = problem(MISSING_AUDIO, Severity::Error, "OneLibrary");
        gone.files = vec!["Contents/a.mp3".into(), "Contents/b.mp3".into()];
        let mut again = problem("serato:gone", Severity::Warning, "Device Library");
        again.files = vec!["Contents/b.mp3".into(), "Contents/c.mp3".into()];
        f.problems = vec![
            again,
            warn,
            gone,
            problem(UNREADABLE, Severity::Unreadable, "OneLibrary"),
            problem(INTERRUPTED, Severity::Error, ""),
            problem(CHECK_FAILED, Severity::Warning, ""),
            by_plugin,
        ];
        assert_eq!(
            judge(&f, &[]).steps(),
            [
                Step::Recover,
                Step::Plugin {
                    plugin: "rekordbox-device".into(),
                    fix: "rebuild_pdb".into()
                },
                Step::Plugin {
                    plugin: "rekordbox-device".into(),
                    fix: "refresh".into()
                },
                Step::Recopy(vec![
                    "Contents/a.mp3".into(),
                    "Contents/b.mp3".into(),
                    "Contents/c.mp3".into()
                ]),
                Step::Rebuild,
            ]
        );
        f.problems = vec![problem(CHECK_FAILED, Severity::Warning, "")];
        assert!(
            judge(&f, &[]).steps().is_empty(),
            "a check that failed is nothing to repair"
        );
    }

    #[test]
    fn players_of_several_plugins_merge_by_model() {
        let a = vec![player(
            "CDJ-3000X",
            &["OneLibrary"],
            &["vfat"],
            &["dos"],
            "confirmed",
        )];
        let b = vec![
            player(
                "cdj-3000x",
                &["Engine Library"],
                &["exfat", "vfat"],
                &[],
                "to_measure",
            ),
            player("Prime 4", &["Engine Library"], &[], &[], "confirmed"),
        ];
        let pa = BoothPreset {
            id: "club".into(),
            label: "Club".into(),
            models: vec!["CDJ-3000X".into()],
        };
        let pb = BoothPreset {
            id: "club".into(),
            label: "Club".into(),
            models: vec!["Prime 4".into()],
        };
        let (players, presets) = merge(vec![(a, vec![pa]), (b, vec![pb])]);
        assert_eq!(players.len(), 2);
        assert_eq!(players[0].reads, ["OneLibrary", "Engine Library"]);
        assert_eq!(players[0].file_systems, ["vfat", "exfat"]);
        assert_eq!(
            players[0].status, "to_measure",
            "confirmed only when every plugin confirms"
        );
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].models, ["CDJ-3000X", "Prime 4"]);
    }

    #[test]
    fn a_plugin_without_a_check_is_judged_from_its_opened_library() {
        let dir = std::env::temp_dir().join(format!("of-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Contents")).unwrap();
        std::fs::write(dir.join("Contents/here.mp3"), b"x").unwrap();
        let listing = |kind: &str| Listing {
            kind: kind.into(),
            files: vec![
                Some(dir.join("Contents/here.mp3")),
                Some(dir.join("Contents/gone.mp3")),
                Some(PathBuf::from("/elsewhere/other.mp3")),
                None,
            ],
            playlists: 3,
        };
        let names = vec!["Device Library".to_owned(), "OneLibrary".to_owned()];
        let (libs, problems) = basic_state(
            "p",
            &names,
            Some(7),
            Ok(listing("rekordbox OneLibrary")),
            &dir,
        );
        assert_eq!(
            libs[0].tracks, None,
            "the count is the opened library's, not its sibling's"
        );
        assert_eq!(
            (libs[1].tracks, libs[1].playlists, libs[1].modified),
            (Some(4), Some(3), Some(7))
        );
        assert_eq!(problems.len(), 1);
        assert_eq!(
            (problems[0].id.as_str(), problems[0].library.as_str()),
            (MISSING_AUDIO, "OneLibrary")
        );
        assert_eq!(
            problems[0].files,
            ["Contents/gone.mp3"],
            "a file off the stick is not this stick's to fix"
        );
        // One library takes the counts whatever the kind says.
        let (libs, _) = basic_state("p", &names[..1], None, Ok(listing("")), &dir);
        assert_eq!(libs[0].tracks, Some(4));
        // A library that does not open makes every library of the plugin unreadable.
        let (libs, problems) = basic_state("p", &names, None, Err("bad key".into()), &dir);
        assert!(libs.iter().all(|l| l.present && !l.readable));
        assert_eq!(problems.len(), 2);
        assert!(problems.iter().all(|p| p.id == UNREADABLE
            && p.severity == Severity::Unreadable
            && p.cause == "bad key"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
