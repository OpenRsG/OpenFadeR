//! Render the arrangement to a new 16-bit stereo PCM WAV (RIFF/WAVE, canonical 44-byte header; the same
//! layout as the REC writer, `audio::rec`). The file is created new, never overwritten: the name is
//! `<title>-edit-<timestamp>.wav` in `$OPENFADER_EDIT_DIR` (see [`crate::paths::output_dir`]).
//! FLAC is not offered: the decoder dependencies (symphonia) have no FLAC encoder.
//! ponytail: the header sizes are u32, so an arrangement past 4 GiB (~6 h at 48 kHz) is refused.

use super::Edit;
use crate::audio::wavout::{FRAME_BYTES, HEADER, header, to_i16};
use crate::paths;
use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

/// Frames per write.
const CHUNK: usize = 1 << 14;

/// `<dir>/<title>-edit-YYYYmmdd-HHMMSS.wav`, a name no file has yet; the directory is created.
pub fn new_path(title: &str) -> io::Result<PathBuf> {
    let dir = paths::output_dir("OPENFADER_EDIT_DIR").ok_or_else(|| {
        io::Error::other("no output folder: set OPENFADER_EDIT_DIR, XDG_MUSIC_DIR or HOME")
    })?;
    paths::timestamped(
        &dir,
        &format!("{}-edit", paths::file_stem(title, 80)),
        "wav",
    )
}

/// Writes the arrangement to `path`, which must not exist.
pub fn write(path: &Path, edit: &Edit) -> io::Result<()> {
    let data = edit.len() as u64 * FRAME_BYTES as u64;
    if data + HEADER as u64 - 8 > u32::MAX as u64 {
        return Err(io::Error::other("arrangement too long for a WAV file"));
    }
    let mut f = BufWriter::new(OpenOptions::new().write(true).create_new(true).open(path)?);
    f.write_all(&header(edit.rate(), data))?;
    let mut buf = vec![0.0f32; CHUNK * 2];
    let mut bytes = Vec::with_capacity(CHUNK * FRAME_BYTES);
    for pos in (0..edit.len()).step_by(CHUNK) {
        let n = CHUNK.min(edit.len() - pos);
        edit.edl().read(&edit.pcm().samples, pos, &mut buf[..2 * n]);
        bytes.clear();
        bytes.extend(buf[..2 * n].iter().flat_map(|&s| to_i16(s).to_le_bytes()));
        f.write_all(&bytes)?;
    }
    f.flush()
}
