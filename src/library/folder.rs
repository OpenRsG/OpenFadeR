//! A plain directory as a library ("Explorer"): every audio (or mp4 / mov / m4v video) file below the root is a
//! track, every subfolder holding audio is a playlist, nested like the disk. Tags come from
//! symphonia's metadata readers (observed: ID3v2.3/2.4, MP4 ilst, Vorbis comments, RIFF INFO, AIFF
//! chunks; ID3v1 is NOT read: the default probe only registers `Id3v2Reader`); the title falls back to the file stem, other fields stay empty/0 when untagged.
//! Open lists the files and `stat`s them; tags are read only for files whose (path, size, mtime) is not in the tag cache,
//! `$XDG_CACHE_HOME/openfader/folder/<FNV-1a 64 of the root path>.bin` (see [`crate::analysis::cache::root`]), one file per
//! root, rewritten (atomically, errors dropped: the tracks are still returned) when a file was read or one went away. Own
//! format, little-endian: `"RKFC"`, `version u32`, `count u32`, then per file `path`, `size u64`, `mtime_ns u128`, and the
//! [`read_track`] fields `title, artist, album, key, genre, comment` (each `len u32` + UTF-8), `bpm_x100 i64`,
//! `length_s i64`, `bitrate u32`, `sample_rate u32`. A wrong magic/version or a short or over-long file is ignored (all files
//! are read again). Bump [`CACHE_VERSION`] when [`read_track`] fills another field or reads tags differently.

use super::{Library, Playlist, PlaylistRow, Track, playlist_tree, walk_files};
use crate::analysis::cache;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

const AUDIO_EXTS: [&str; 7] = ["mp3", "m4a", "aac", "flac", "wav", "aif", "aiff"];
const CACHE_MAGIC: &[u8; 4] = b"RKFC";
const CACHE_VERSION: u32 = 1;

pub struct Folder {
    /// (folder relative to root with `/` separators, "" = root; track), sorted by path.
    tracks: Vec<(String, Track)>,
    /// Folders holding audio at any depth, sorted.
    dirs: BTreeSet<String>,
}

/// Whether `p` has a music or video ([`crate::video::is_video`]) file extension.
pub fn is_audio(p: &Path) -> bool {
    crate::paths::has_extension(p, &AUDIO_EXTS) || crate::video::is_video(p)
}

/// Audio files below `dir`, symlinked directories not followed.
pub fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    walk_files(dir, usize::MAX, |p| {
        if is_audio(&p) {
            out.push(p);
        }
    });
}

/// BPM text → ×100: "128", "127.50", "128,5" and "128 BPM" all parse; anything else is 0 (unknown).
fn bpm_x100(v: &str) -> i64 {
    let n: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | ','))
        .collect();
    n.replace(',', ".")
        .parse::<f64>()
        .map_or(0, |b| (b * 100.0).round() as i64)
}

/// ID3v2.3 genre refs: "(17)Rock" → "Rock"; a bare "(17)" / "17" is an index into the ID3v1 list → "".
/// ponytail: symphonia keeps that table private, so unresolved indexes are dropped; depend on symphonia-metadata for `id3v1::util::genre_name` if such tags matter.
fn genre(v: &str) -> &str {
    let v = match v.strip_prefix('(').and_then(|r| r.split_once(')')) {
        Some((n, rest)) if n.bytes().all(|b| b.is_ascii_digit()) => rest,
        _ => v,
    };
    if v.bytes().all(|b| b.is_ascii_digit()) {
        ""
    } else {
        v
    }
}

/// Fills still-empty fields; the first value of a field wins (multi-valued ID3v2.4/Vorbis frames list
/// the primary value first), except artists, which are joined with ", " so no collaborator is lost.
/// The key stays as written (Camelot "8A" or musical "F#m"); it is matched by the last `:` segment so
/// ID3 TXXX and MP4 `----` freeform tags (`…:initialkey`) count too.
fn apply(t: &mut Track, rev: &MetadataRevision) {
    for tag in rev.tags() {
        let v = tag.value.to_string();
        let v = v.trim_matches(|c: char| c.is_whitespace() || c == '\0');
        if v.is_empty() {
            continue;
        }
        match tag.std_key {
            Some(StandardTagKey::TrackTitle) if t.title.is_empty() => t.title = v.into(),
            Some(StandardTagKey::Album) if t.album.is_empty() => t.album = v.into(),
            Some(StandardTagKey::Artist)
                if t.artist != v && !t.artist.split(", ").any(|a| a == v) =>
            {
                if !t.artist.is_empty() {
                    t.artist.push_str(", ");
                }
                t.artist.push_str(v);
            }
            Some(StandardTagKey::Genre) if t.genre.is_empty() => t.genre = genre(v).into(),
            Some(StandardTagKey::Bpm) if t.bpm_x100 == 0 => t.bpm_x100 = bpm_x100(v),
            Some(StandardTagKey::Comment) if t.comment.is_empty() => t.comment = v.into(),
            None if t.key.is_empty()
                && matches!(
                    tag.key
                        .rsplit(':')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "TKEY" | "INITIALKEY" | "KEY"
                ) =>
            {
                t.key = v.into()
            }
            _ => {}
        }
    }
}

/// The track a file is: tags and stream facts through symphonia, the title falling back to the file stem.
pub fn read_track(path: &Path) -> Track {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut t = Track {
        file: Some(path.to_path_buf()),
        ..Track::default()
    };
    let _ = (|| -> Option<()> {
        let mss = MediaSourceStream::new(
            Box::new(std::fs::File::open(path).ok()?),
            Default::default(),
        );
        let mut hint = Hint::new();
        hint.with_extension(path.extension()?.to_str()?);
        let mut probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .ok()?;
        if let Some(rev) = probed
            .metadata
            .get()
            .as_ref()
            .and_then(|m| m.current().cloned())
        {
            apply(&mut t, &rev);
        }
        if let Some(rev) = probed.format.metadata().current() {
            apply(&mut t, rev);
        }
        let p = &probed.format.default_track()?.codec_params;
        t.sample_rate = p.sample_rate.unwrap_or(0);
        // MP4's n_frames counts media-timescale ticks, which only equals samples by convention: go through the time base.
        if let (Some(n), Some(tb)) = (
            p.n_frames,
            p.time_base
                .or_else(|| p.sample_rate.map(|r| TimeBase::new(1, r))),
        ) {
            let s = tb.calc_time(n);
            t.length_s = (s.seconds as f64 + s.frac).round() as i64;
        }
        if let (Some(bits), Some(ch), Some(r)) = (p.bits_per_sample, p.channels, p.sample_rate) {
            t.bitrate = bits * ch.count() as u32 * r / 1000;
        }
        Some(())
    })();
    if t.title.is_empty() {
        t.title = stem;
    }
    t
}

/// (size, mtime in ns since the epoch): what a changed file differs in.
type Stamp = (u64, u128);

fn stamp(p: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some((m.len(), t.as_nanos()))
}

/// `<cache root>/folder/<hash of the root path>.bin`; `None` when there is no cache directory.
fn cache_file(root: &Path) -> Option<PathBuf> {
    let h = cache::fnv1a(root.to_string_lossy().bytes());
    Some(cache::root()?.join("folder").join(format!("{h:016x}.bin")))
}

fn put_str(o: &mut Vec<u8>, s: &str) {
    o.extend_from_slice(&(s.len() as u32).to_le_bytes());
    o.extend_from_slice(s.as_bytes());
}

fn encode(entries: &[(&Track, Stamp)]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(CACHE_MAGIC);
    o.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    o.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (t, (size, mtime)) in entries {
        put_str(
            &mut o,
            &t.file.as_deref().unwrap_or(Path::new("")).to_string_lossy(),
        );
        o.extend_from_slice(&size.to_le_bytes());
        o.extend_from_slice(&mtime.to_le_bytes());
        for s in [&t.title, &t.artist, &t.album, &t.key, &t.genre, &t.comment] {
            put_str(&mut o, s);
        }
        o.extend_from_slice(&t.bpm_x100.to_le_bytes());
        o.extend_from_slice(&t.length_s.to_le_bytes());
        o.extend_from_slice(&t.bitrate.to_le_bytes());
        o.extend_from_slice(&t.sample_rate.to_le_bytes());
    }
    o
}

/// Cursor over the cache bytes; every read is `None` past the end.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (h, r) = self.0.split_at_checked(n)?;
        self.0 = r;
        Some(h)
    }
    fn num<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    fn str(&mut self) -> Option<String> {
        let n = u32::from_le_bytes(self.num()?) as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// Cached tracks by path; empty when the file is missing, corrupt or from another version.
fn decode(b: &[u8]) -> HashMap<String, (Stamp, Track)> {
    fn go(b: &[u8]) -> Option<HashMap<String, (Stamp, Track)>> {
        let mut r = Reader(b);
        if r.take(4)? != CACHE_MAGIC || r.num()? != CACHE_VERSION.to_le_bytes() {
            return None;
        }
        let n = u32::from_le_bytes(r.num()?);
        let mut out = HashMap::new();
        for _ in 0..n {
            let path = r.str()?;
            let stamp = (u64::from_le_bytes(r.num()?), u128::from_le_bytes(r.num()?));
            let t = Track {
                title: r.str()?,
                artist: r.str()?,
                album: r.str()?,
                key: r.str()?,
                genre: r.str()?,
                comment: r.str()?,
                bpm_x100: i64::from_le_bytes(r.num()?),
                length_s: i64::from_le_bytes(r.num()?),
                bitrate: u32::from_le_bytes(r.num()?),
                sample_rate: u32::from_le_bytes(r.num()?),
                file: Some(PathBuf::from(&path)),
                ..Track::default()
            };
            out.insert(path, (stamp, t));
        }
        r.0.is_empty().then_some(out)
    }
    go(b).unwrap_or_default()
}

impl Folder {
    pub fn open(root: &Path) -> Result<Self, String> {
        Self::open_with(root, cache_file(root).as_deref(), read_track)
    }

    /// [`Folder::open`] with the tag cache file and the tag reader given (the test counts reads).
    fn open_with(
        root: &Path,
        cache: Option<&Path>,
        read: impl Fn(&Path) -> Track,
    ) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("{} is not a folder", root.display()));
        }
        let mut files = Vec::new();
        walk(root, &mut files);
        files.sort();
        let mut old = cache
            .and_then(|c| std::fs::read(c).ok())
            .map(|b| decode(&b))
            .unwrap_or_default();
        let old_len = old.len();
        let mut dirs = BTreeSet::new();
        let mut tracks = Vec::with_capacity(files.len());
        let mut stamps = Vec::with_capacity(files.len());
        let mut dirty = false;
        for f in files {
            let rel = f
                .strip_prefix(root)
                .unwrap_or(&f)
                .parent()
                .unwrap_or(Path::new(""));
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let mut d = rel.as_str();
            while !d.is_empty() {
                dirs.insert(d.to_string());
                d = d.rfind('/').map_or("", |i| &d[..i]);
            }
            let st = stamp(&f);
            let hit = st.and_then(|st| {
                let (old_st, t) = old.remove(&*f.to_string_lossy())?;
                (old_st == st).then_some(t)
            });
            let t = hit.unwrap_or_else(|| {
                dirty = true;
                read(&f)
            });
            stamps.push(st);
            tracks.push((rel, t));
        }
        if let Some(c) = cache.filter(|_| dirty || old_len != tracks.len()) {
            let entries: Vec<_> = tracks
                .iter()
                .zip(&stamps)
                .filter_map(|((_, t), s)| Some((t, (*s)?)))
                .collect();
            let tmp = c.with_extension(format!("{}.tmp", std::process::id()));
            let _ = c.parent().map_or(Ok(()), std::fs::create_dir_all);
            // ponytail: a failed cache write is dropped; the tracks are still returned.
            let _ = std::fs::write(&tmp, encode(&entries)).and_then(|()| std::fs::rename(&tmp, c));
        }
        Ok(Self { tracks, dirs })
    }
}

impl Library for Folder {
    fn kind(&self) -> &str {
        "Folder"
    }

    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let parents: HashSet<&str> = self
            .dirs
            .iter()
            .filter_map(|d| d.rsplit_once('/').map(|(p, _)| p))
            .collect();
        let rows: Vec<PlaylistRow> = self
            .dirs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let (parent, name) = d
                    .rsplit_once('/')
                    .map_or((None, d.as_str()), |(p, n)| (Some(p.to_string()), n));
                PlaylistRow {
                    id: d.clone(),
                    name: name.into(),
                    is_folder: parents.contains(d.as_str()),
                    parent,
                    order: i as i64,
                }
            })
            .collect();
        Ok(playlist_tree(&rows))
    }

    /// A folder lists its own audio and that of everything below it.
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        let below = playlist.map(|p| (p, format!("{p}/")));
        Ok(self
            .tracks
            .iter()
            .filter(|(d, _)| {
                below
                    .as_ref()
                    .is_none_or(|(p, b)| d == p || d.starts_with(b.as_str()))
            })
            .map(|(_, t)| t.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn fake(p: &Path) -> Track {
        Track {
            title: format!("T {}", p.file_stem().unwrap().to_string_lossy()),
            artist: "Ä, B".into(),
            album: "Al\nbum".into(),
            key: "8A".into(),
            genre: "House".into(),
            comment: "c".into(),
            bpm_x100: 12850,
            length_s: 241,
            bitrate: 1411,
            sample_rate: 44100,
            file: Some(p.to_path_buf()),
            ..Track::default()
        }
    }

    #[test]
    fn second_open_reads_no_tags_from_unchanged_files() {
        let dir = std::env::temp_dir().join(format!("rk-foldercache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("lib");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.wav"), b"aaaa").unwrap();
        std::fs::write(root.join("sub/b.mp3"), b"bb").unwrap();
        let cache = dir.join("cache/folder.bin");

        let reads = Cell::new(0);
        let open = || {
            reads.set(0);
            let f = Folder::open_with(&root, Some(&cache), |p| {
                reads.set(reads.get() + 1);
                fake(p)
            })
            .unwrap();
            f.tracks(None).unwrap()
        };

        let first = open();
        assert_eq!((reads.get(), first.len()), (2, 2));
        assert_eq!(first[0], fake(&root.join("a.wav")));
        assert_eq!(open(), first);
        assert_eq!(reads.get(), 0, "unchanged files come from the cache");

        // A changed file is read again, the other one is not.
        std::fs::write(root.join("a.wav"), b"aaaaaa").unwrap();
        assert_eq!(open(), first);
        assert_eq!(reads.get(), 1);

        // A removed file leaves the cache; a corrupt cache reads everything again.
        std::fs::remove_file(root.join("sub/b.mp3")).unwrap();
        assert_eq!(open().len(), 1);
        assert_eq!(reads.get(), 0);
        std::fs::write(&cache, b"RKFC garbage").unwrap();
        assert_eq!(open().len(), 1);
        assert_eq!(reads.get(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
