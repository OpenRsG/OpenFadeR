//! Writing [`Tags`] into audio files without moving the audio. A child plans the change as [`Edit`]s (byte ranges of the file
//! to replace) and [`apply`] carries them out. Planning has two modes:
//!
//! - `fixed`: the file keeps its exact size. The new tag data must fit in the room the old one holds (ID3 padding, a FLAC
//!   PADDING block, an MP4 `free` atom, an existing ID3 chunk) or the plan fails with [`Error::NoRoom`]. This is the mode for a
//!   file other software identifies by name and size.
//! - not fixed: the file may grow (a private copy of a track): missing tags are created and generous padding is left so the
//!   next export fits in place.
//!
//! Children, one per container: `mp3` (ID3v2 at the head), `flac` (VORBIS_COMMENT), `riff` (the ID3 chunk of WAV and AIFF),
//! `mp4` (`----` atoms under `moov/udta/meta/ilst`). Shared: `markers` (the payloads), `id3` (the ID3 tag codec mp3 and riff use).

mod flac;
mod id3;
pub mod markers;
mod mp3;
mod mp4;
mod riff;

pub use markers::Tags;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Padding left behind a tag that had to grow.
const SLACK: usize = 2048;

#[derive(Debug)]
pub enum Error {
    /// The tags do not fit in the room the file has.
    NoRoom,
    /// The container is not one we write, or the file is damaged.
    Unsupported,
    Io(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

/// Bytes `start..end` of the file become `data`.
pub struct Edit {
    pub start: u64,
    pub end: u64,
    pub data: Vec<u8>,
}

/// What it takes to put `tags` into the file at `path` (empty: nothing to change).
pub fn plan(path: &Path, tags: &Tags, fixed: bool) -> Result<Vec<Edit>, Error> {
    if !tags.touch {
        return Ok(vec![]);
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    let f = &mut File::open(path)?;
    match ext.as_deref() {
        Some("mp3") => mp3::plan(f, tags, fixed),
        Some("flac") => flac::plan(f, tags, fixed),
        Some("wav" | "aif" | "aiff") => riff::plan(f, tags, fixed),
        Some("m4a" | "mp4") => mp4::plan(f, tags, fixed),
        _ => Err(Error::Unsupported),
    }
}

/// The data of the two GEOB objects (or their FLAC / MP4 counterparts) Serato keeps in an audio file.
#[derive(Debug, Default)]
pub struct Stored {
    /// `Serato Markers2`: see [`markers::entries`].
    pub markers2: Option<Vec<u8>>,
    /// `Serato BeatGrid`.
    pub grid: Option<Vec<u8>>,
}

/// What the file at `path` holds (nothing for a file without the tags); `Err` for a container we do not read.
pub fn read(path: &Path) -> Result<Stored, Error> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    let f = &mut File::open(path)?;
    match ext.as_deref() {
        Some("mp3") => mp3::read(f),
        Some("flac") => flac::read(f),
        Some("wav" | "aif" | "aiff") => riff::read(f),
        Some("m4a" | "mp4") => mp4::read(f),
        _ => Err(Error::Unsupported),
    }
}

/// Carries out `edits`; `true` when the file changed. Edits of equal length are written in place (only the bytes that differ),
/// anything else goes through a temporary file that replaces the original.
pub fn apply(path: &Path, edits: &mut [Edit]) -> Result<bool, Error> {
    edits.sort_by_key(|e| e.start);
    if edits.iter().all(|e| e.end - e.start == e.data.len() as u64) {
        let mut f = OpenOptions::new().read(true).write(true).open(path)?;
        let mut changed = false;
        for e in edits.iter() {
            let mut old = vec![0; e.data.len()];
            f.seek(SeekFrom::Start(e.start))?;
            f.read_exact(&mut old)?;
            let same =
                |a: &mut dyn Iterator<Item = (&u8, &u8)>| a.take_while(|(x, y)| x == y).count();
            let head = same(&mut old.iter().zip(&e.data));
            if head == old.len() {
                continue;
            }
            let tail = same(&mut old.iter().rev().zip(e.data.iter().rev()));
            f.seek(SeekFrom::Start(e.start + head as u64))?;
            f.write_all(&e.data[head..e.data.len() - tail])?;
            changed = true;
        }
        return Ok(changed);
    }
    let tmp = path.with_extension("part");
    let done = (|| -> io::Result<()> {
        let mut src = File::open(path)?;
        let mut out = io::BufWriter::new(File::create(&tmp)?);
        let mut pos = 0;
        for e in edits.iter() {
            io::copy(&mut (&mut src).take(e.start - pos), &mut out)?;
            out.write_all(&e.data)?;
            src.seek(SeekFrom::Start(e.end))?;
            pos = e.end;
        }
        io::copy(&mut src, &mut out)?;
        out.flush()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(true)
}

/// `n` bytes from the current position (fewer at the end of the file).
fn read_n(f: &mut File, n: u64) -> io::Result<Vec<u8>> {
    let mut v = Vec::new();
    Read::take(f, n).read_to_end(&mut v)?;
    Ok(v)
}
