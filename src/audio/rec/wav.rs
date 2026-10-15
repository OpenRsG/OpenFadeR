//! Canonical 16-bit stereo WAV writer, format and header from [`crate::audio::wavout`]. The sizes are patched by
//! [`Wav::sync`], so the file is valid at any moment.

use crate::audio::wavout::{FRAME_BYTES, header, to_i16};
use std::io::{self, Seek, SeekFrom, Write};

pub(super) struct Wav<W: Write + Seek> {
    w: W,
    rate: u32,
    pub(super) frames: u64,
    /// Reused encode buffer, so a write allocates only while it grows.
    bytes: Vec<u8>,
}

impl<W: Write + Seek> Wav<W> {
    pub(super) fn new(mut w: W, rate: u32) -> io::Result<Self> {
        w.write_all(&header(rate, 0))?;
        Ok(Wav {
            w,
            rate,
            frames: 0,
            bytes: Vec::new(),
        })
    }

    pub(super) fn write(&mut self, frames: &[[f32; 2]]) -> io::Result<()> {
        self.bytes.clear();
        self.bytes.extend(
            frames
                .iter()
                .flatten()
                .flat_map(|&x| to_i16(x).to_le_bytes()),
        );
        self.w.write_all(&self.bytes)?;
        self.frames += frames.len() as u64;
        Ok(())
    }

    /// Rewrites the header for the frames so far and returns to the end, so the file is valid as it stands.
    pub(super) fn sync(&mut self) -> io::Result<()> {
        self.w.seek(SeekFrom::Start(0))?;
        self.w
            .write_all(&header(self.rate, self.frames * FRAME_BYTES as u64))?;
        self.w.seek(SeekFrom::End(0))?;
        self.w.flush()
    }

    pub(super) fn finish(mut self) -> io::Result<W> {
        self.sync()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::wavout::u32_at;
    use std::io::Cursor;

    #[test]
    fn header_sizes_are_valid_after_n_frames() {
        for n in [0usize, 1, 1000] {
            let mut wav = Wav::new(Cursor::new(Vec::new()), 44100).unwrap();
            wav.write(&vec![[0.5, -0.5]; n]).unwrap();
            let b = wav.finish().unwrap().into_inner();
            assert_eq!(b.len(), 44 + 4 * n);
            assert_eq!(u32_at(&b, 4) as usize, b.len() - 8, "RIFF size");
            assert_eq!(u32_at(&b, 40) as usize, 4 * n, "data size");
        }
    }

    #[test]
    fn sync_midway_leaves_a_valid_file_and_writing_continues() {
        let mut wav = Wav::new(Cursor::new(Vec::new()), 48000).unwrap();
        wav.write(&[[0.0; 2]; 10]).unwrap();
        wav.sync().unwrap();
        assert_eq!(u32_at(wav.w.get_ref(), 40), 40);
        wav.write(&[[0.0; 2]; 5]).unwrap(); // appended after the patched header, not over it
        let b = wav.finish().unwrap().into_inner();
        assert_eq!((b.len(), u32_at(&b, 40)), (44 + 60, 60));
    }

    #[test]
    fn samples_are_clamped_16_bit() {
        let mut wav = Wav::new(Cursor::new(Vec::new()), 8000).unwrap();
        wav.write(&[[1.0, -1.0], [2.0, -2.0], [0.5, 0.0]]).unwrap();
        let b = wav.finish().unwrap().into_inner();
        let s: Vec<i16> = b[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(s, [32767, -32767, 32767, -32767, 16384, 0]);
    }
}
