//! VirtualDJ's library as a source: `database.xml` (of the home folder, or of a drive: `<root>/VirtualDJ/database.xml`) and the
//! lists next to it.
//!
//! Sources: the element and attribute names are the ones [`crate::song`] writes (VDJPedia "VDJ_database"); a list is a
//! `.vdjfolder` below `MyLists` (VirtualDJ 2024 and later) or `Folders` (older), a folder of lists being a folder on disk
//! ([`crate::lists`]); `Playlists/*.m3u` is VirtualDJ's older plain playlist export (inferred: one path per line, relative
//! paths are relative to the list). Mapping: `Tags` give title, artist, album, genre, label, key, rating (`Stars`) and the
//! tempo (`Bpm`, seconds per beat, so `bpm_x100 = 6000 / Bpm`); `Infos` the length, the date added (`FirstSeen`, Unix
//! seconds) and the bitrate; `<Comment>` the comment; `Scan` the key and tempo when `Tags` have none and the grid.
//!
//! Grid (inferred from the writer): one constant grid, `Scan Bpm` (else `Tags Bpm`) seconds per beat from the anchor,
//! `<Poi Type="beatgrid" Pos>` (else `Scan Phase`), the anchor being a downbeat; it is expanded backwards to the start and
//! forwards to the end of the track. `<Poi>` of no type or `cue` is a cue (`Num` from 1 a hot cue, anything else a memory cue),
//! `loop` a loop of `Size` beats at the grid's tempo; other types (automix, remix, load, action) are not cues. A cue named
//! like the writer's default (`Cue 3`, `Memory`) has no comment.
//!
//! Paths: `FilePath` is `E:\Contents\x.mp3` or `/Volumes/NAME/Contents/x.mp3` on a drive and VirtualDJ puts the real drive in;
//! here the part after the drive is looked for below the root (the folder holding `VirtualDJ`) when the path is not there as it
//! is, so a stick read on any computer finds its audio. A relative path is relative to that root.

use crate::song::{camelot, drive_rest, identity};
use crate::xml::{self, El};
use crate::{DATABASE, ROOT};
use openfader_plugin::library::{
    playlist_tree, Analysis, Beat, Cue, Playlist, PlaylistRow, Probe, Source, Track,
};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Short name of the source.
const KIND: &str = "VirtualDJ";
/// Lists deeper than this are not read.
const DEPTH: usize = 32;

/// Whether the first bytes of `file` open a VirtualDJ database.
fn is_database(file: &Path) -> bool {
    let mut head = Vec::new();
    std::fs::File::open(file)
        .and_then(|f| f.take(4096).read_to_end(&mut head))
        .is_ok()
        && String::from_utf8_lossy(&head).contains(&format!("<{ROOT}"))
}

/// The database file `path` names: the file itself, or the one in the folder (a drive root or the `VirtualDJ` folder).
pub fn locate(path: &Path) -> Option<PathBuf> {
    let found = if path.is_dir() {
        [path.join(DATABASE), path.join("database.xml")]
            .into_iter()
            .find(|p| p.is_file())?
    } else {
        path.to_owned()
    };
    is_database(&found).then_some(found)
}

pub fn probe(path: &Path) -> Option<Probe> {
    locate(path).map(|_| Probe {
        score: 100,
        kind: KIND.into(),
    })
}

fn num(e: Option<&El>, key: &str) -> Option<f64> {
    e?.attr(key)?.trim().parse().ok()
}

fn text(e: Option<&El>, key: &str) -> String {
    e.and_then(|e| e.attr(key)).unwrap_or_default().to_owned()
}

/// Seconds per beat of a `Tags` or `Scan` element.
fn period(e: Option<&El>) -> Option<f64> {
    num(e, "Bpm").filter(|p| *p > 0.0)
}

/// Milliseconds of `seconds`, never negative.
fn ms(seconds: f64) -> u32 {
    (seconds * 1000.0).round().max(0.0) as u32
}

/// `[r, g, b]` of an ARGB decimal colour; `None` for 0 (no colour).
fn color(v: &str) -> Option<[u8; 3]> {
    let v = v.trim().parse::<i64>().ok()? as u32;
    (v != 0).then(|| [(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// More beats than this in one grid is a corrupt file, not a track.
const MAX_BEATS: f64 = 100_000.0;

/// The beats of a grid: `seconds` per beat from the downbeat `anchor_ms`, from the start of the track to `end_ms`.
fn beats(seconds: f64, anchor_ms: f64, end_ms: f64) -> Vec<Beat> {
    let bpm = (6000.0 / seconds).round();
    if !(1.0..=f64::from(u16::MAX)).contains(&bpm) {
        return Vec::new();
    }
    let step = seconds * 1000.0;
    let first = (-anchor_ms / step).ceil();
    let last = ((end_ms - anchor_ms) / step).floor();
    // A corrupt tempo or position (NaN and infinity fail this too) makes no grid rather than a huge one.
    if !(first.is_finite() && last.is_finite() && last - first < MAX_BEATS) {
        return Vec::new();
    }
    (first as i64..=last as i64)
        .map(|k| Beat {
            bar_pos: k.rem_euclid(4) as u16 + 1,
            bpm_x100: bpm as u16,
            time_ms: (anchor_ms + k as f64 * step).round() as u32,
        })
        .collect()
}

/// The cues of a song: `<Poi>` cues and loops, `period` being the seconds per beat that sizes a loop.
fn cues(song: &El, period: Option<f64>) -> Vec<Cue> {
    let mut out = Vec::new();
    for p in song.children("Poi") {
        let kind = p.attr("Type");
        let Some(pos) = num(Some(p), "Pos").filter(|_| matches!(kind, None | Some("cue" | "loop")))
        else {
            continue;
        };
        let hot_cue = p
            .attr("Num")
            .and_then(|n| n.trim().parse::<i32>().ok())
            .filter(|n| *n > 0)
            .map_or(0, |n| n.min(i32::from(u16::MAX)) as u16);
        let loop_end_ms = match (kind, num(Some(p), "Size"), period) {
            (Some("loop"), Some(size), Some(per)) if size > 0.0 => Some(ms(pos + size * per)),
            _ => None,
        };
        let comment = p
            .attr("Name")
            .filter(|n| !n.is_empty() && *n != "Memory" && *n != format!("Cue {hot_cue}"))
            .map(str::to_owned);
        out.push(Cue {
            hot_cue,
            time_ms: ms(pos),
            loop_end_ms,
            color: p.attr("Color").and_then(color),
            comment,
        });
    }
    out
}

/// Title, artist and file of a list entry the database does not hold, from the list's own attributes.
struct Entry {
    path: String,
    artist: String,
    title: String,
}

/// The songs of a list file in order: `.vdjfolder` (`<song path artist title idx/>`) or `.m3u` (one path per line).
fn list_entries(file: &Path, m3u: bool) -> Result<Vec<Entry>, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let body = String::from_utf8_lossy(&bytes);
    let body = body.strip_prefix('\u{feff}').unwrap_or(&body);
    if m3u {
        let dir = file.parent().unwrap_or(Path::new(""));
        return Ok(body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                let relative = drive_rest(l).is_none()
                    && !Path::new(l).is_absolute()
                    && !l.starts_with(['/', '\\']);
                Entry {
                    path: if relative {
                        dir.join(l.replace('\\', "/"))
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        l.to_owned()
                    },
                    artist: String::new(),
                    title: String::new(),
                }
            })
            .collect());
    }
    let list = xml::parse(body).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut songs: Vec<(Option<i64>, Entry)> = list
        .children("song")
        .filter_map(|s| {
            Some((
                s.attr("idx").and_then(|i| i.trim().parse().ok()),
                Entry {
                    path: s.attr("path")?.to_owned(),
                    artist: text(Some(s), "artist"),
                    title: text(Some(s), "title"),
                },
            ))
        })
        .collect();
    songs.sort_by_key(|(idx, _)| idx.unwrap_or(i64::MAX));
    Ok(songs.into_iter().map(|(_, e)| e).collect())
}

/// Adds the folders and lists below `dir` (id prefix `id`) to `rows`; `files` maps a list's id to its file and whether it is `.m3u`.
fn scan(
    dir: &Path,
    id: &str,
    rows: &mut Vec<PlaylistRow>,
    files: &mut HashMap<String, (PathBuf, bool)>,
    depth: usize,
) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    for p in paths {
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if name.starts_with('.') {
            continue;
        }
        let child = format!("{id}/{name}");
        let parent = Some(id.to_owned()).filter(|_| depth > 0);
        if p.is_dir() {
            if depth < DEPTH {
                rows.push(PlaylistRow {
                    id: child.clone(),
                    name,
                    is_folder: true,
                    parent,
                    order: rows.len() as i64,
                });
                scan(&p, &child, rows, files, depth + 1);
            }
            continue;
        }
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase());
        let m3u = match ext.as_deref() {
            Some("vdjfolder") => false,
            Some("m3u" | "m3u8") => true,
            _ => continue,
        };
        rows.push(PlaylistRow {
            id: child.clone(),
            name: p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            is_folder: false,
            parent,
            order: rows.len() as i64,
        });
        files.insert(child, (p, m3u));
    }
}

/// An opened `database.xml` with its lists.
pub struct Reader {
    /// The `database.xml` that was opened.
    file: PathBuf,
    /// The folder that holds `VirtualDJ`: what the paths of a drive are relative to.
    root: PathBuf,
    /// Text of every `<Song>` that has a `FilePath`, as written.
    songs: Vec<String>,
    /// [`identity`] of a `FilePath` to its index in `songs`.
    at: HashMap<String, usize>,
    playlists: Vec<Playlist>,
    files: HashMap<String, (PathBuf, bool)>,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file =
            locate(path).ok_or_else(|| format!("{}: not a VirtualDJ database", path.display()))?;
        let text = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let text = String::from_utf8(text).map_err(|_| format!("{}: not UTF-8", file.display()))?;
        let doc = xml::split(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        if doc.root != ROOT {
            return Err(format!("{}: not a VirtualDJ database", file.display()));
        }
        let (mut songs, mut at) = (Vec::new(), HashMap::new());
        for item in doc.items {
            if let Some(p) = item.file_path {
                at.entry(identity(&p)).or_insert(songs.len());
                songs.push(item.raw);
            }
        }
        let vdj = file.parent().unwrap_or(Path::new("."));
        let root = if vdj
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("VirtualDJ"))
        {
            vdj.parent().unwrap_or(vdj)
        } else {
            vdj
        };
        let (mut rows, mut files) = (Vec::new(), HashMap::new());
        let mut dirs: Vec<(String, PathBuf)> = std::fs::read_dir(vdj)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .filter(|(n, _)| {
                ["mylists", "folders", "playlists"].contains(&n.to_lowercase().as_str())
            })
            .collect();
        dirs.sort_by_key(|(n, _)| n.to_lowercase());
        for (name, dir) in dirs {
            scan(&dir, &name, &mut rows, &mut files, 0);
        }
        Ok(Self {
            root: root.to_owned(),
            songs,
            at,
            playlists: playlist_tree(&rows),
            files,
            file,
        })
    }

    /// The file a `FilePath` (or list entry) names, see the module doc.
    fn resolve(&self, file_path: &str) -> PathBuf {
        let p = file_path.replace('\\', "/");
        if let Some(rest) = drive_rest(file_path) {
            let native = Path::new(&p);
            if (cfg!(windows) || p.starts_with('/')) && native.exists() {
                return native.to_owned();
            }
            return self.root.join(rest.trim_start_matches('/'));
        }
        if p.starts_with('/') || Path::new(&p).is_absolute() {
            p.into()
        } else {
            self.root.join(p)
        }
    }

    /// `<database file>\t<FilePath>`: a reference that needs no open reader.
    fn reference(&self, file_path: &str) -> String {
        format!("{}\t{file_path}", self.file.display())
    }

    fn song(&self, i: usize) -> Option<El> {
        xml::parse(&self.songs[i]).ok()
    }

    fn track(&self, s: &El) -> Track {
        let (tags, infos, scan) = (
            s.children("Tags").next(),
            s.children("Infos").next(),
            s.children("Scan").next(),
        );
        let path = s.attr("FilePath").unwrap_or_default();
        let stem = path.rsplit(['/', '\\']).next().unwrap_or_default();
        let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
        let title = text(tags, "Title");
        let key = Some(text(tags, "Key"))
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| text(scan, "Key"));
        Track {
            title: if title.is_empty() {
                stem.to_owned()
            } else {
                title
            },
            artist: text(tags, "Author"),
            album: text(tags, "Album"),
            genre: text(tags, "Genre"),
            label: text(tags, "Label"),
            key,
            bpm_x100: period(tags)
                .or(period(scan))
                .map_or(0, |p| (6000.0 / p).round() as i64),
            length_s: num(infos, "SongLength").map_or(0, |l| l.round() as i64),
            file: Some(self.resolve(path)),
            analysis_ref: Some(self.reference(path)),
            rating: num(tags, "Stars").map_or(0, |r| r.round().clamp(0.0, 5.0) as u8),
            comment: s
                .children("Comment")
                .next()
                .map(El::text)
                .unwrap_or_default(),
            date_added: num(infos, "FirstSeen")
                .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            bitrate: num(infos, "Bitrate").map_or(0, |b| b.max(0.0).round() as u32),
            ..Track::default()
        }
    }
}
/// What `export` and `device_check` read back from a database.
impl Reader {
    pub(crate) fn song_count(&self) -> usize {
        self.songs.len()
    }

    /// Hot cues, memory cues and loops of the song whose `FilePath` is `file_path`.
    pub(crate) fn cue_count(&self, file_path: &str) -> usize {
        self.analysis_of(file_path).map_or(0, |a| a.cues.len())
    }

    /// `(playlist id, list file)` of every list found next to the database.
    pub(crate) fn list_files(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.files
            .iter()
            .map(|(id, (f, _))| (id.as_str(), f.as_path()))
    }
}

impl Source for Reader {
    fn kind(&self) -> String {
        KIND.into()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(self.playlists.clone())
    }

    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let Some(id) = playlist else {
            return Ok((0..self.songs.len())
                .filter_map(|i| self.song(i))
                .map(|s| self.track(&s))
                .collect());
        };
        let (file, m3u) = self
            .files
            .get(id)
            .ok_or_else(|| format!("{id}: no such playlist"))?;
        Ok(list_entries(file, *m3u)?
            .into_iter()
            .map(
                |e| match self.at.get(&identity(&e.path)).and_then(|&i| self.song(i)) {
                    Some(s) => self.track(&s),
                    None => {
                        let name = e.path.rsplit(['/', '\\']).next().unwrap_or_default();
                        Track {
                            title: if e.title.is_empty() {
                                name.rsplit_once('.').map_or(name, |(s, _)| s).to_owned()
                            } else {
                                e.title
                            },
                            artist: e.artist,
                            file: Some(self.resolve(&e.path)),
                            analysis_ref: Some(self.reference(&e.path)),
                            ..Track::default()
                        }
                    }
                },
            )
            .collect())
    }

    fn analysis(&self, track: &Track) -> Option<Analysis> {
        self.analysis_of(track.analysis_ref.as_deref()?.split_once('\t')?.1)
    }
}

/// The analysis behind a reference from [`Source::tracks`]: opens the database itself.
pub fn analysis_ref(reference: &str) -> Option<Analysis> {
    let (db, path) = reference.split_once('\t')?;
    Reader::open(Path::new(db)).ok()?.analysis_of(path)
}

impl Reader {
    /// The analysis of the song whose `FilePath` is `path`.
    fn analysis_of(&self, path: &str) -> Option<Analysis> {
        let s = self.song(*self.at.get(&identity(path))?)?;
        let (tags, infos, scan) = (
            s.children("Tags").next(),
            s.children("Infos").next(),
            s.children("Scan").next(),
        );
        let seconds = period(scan).or(period(tags));
        let cues = cues(&s, seconds);
        let anchor = s
            .children("Poi")
            .filter(|p| p.attr("Type") == Some("beatgrid"))
            .find_map(|p| num(Some(p), "Pos"))
            .or(num(scan, "Phase"));
        let end = cues
            .iter()
            .map(|c| c.loop_end_ms.unwrap_or(c.time_ms))
            .max()
            .map_or(0.0, f64::from)
            .max(num(infos, "SongLength").unwrap_or(0.0) * 1000.0);
        let beats = match (seconds, anchor) {
            (Some(p), Some(a)) => beats(p, a.max(0.0) * 1000.0, end),
            _ => Vec::new(),
        };
        let key = Some(text(scan, "Key"))
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| text(tags, "Key"));
        let key = camelot(&key);
        (!beats.is_empty() || !cues.is_empty() || key.is_some()).then(|| Analysis {
            beats,
            cues,
            key,
            ..Analysis::default()
        })
    }
}
