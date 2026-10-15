//! "Automatically optimize audio level" (Preferences > Advanced > Recordings): once a file is
//! finished, its samples are scaled so the loudest one reaches [`TARGET`] of full scale. Never lowers a
//! level. ours leaves 0.1 dB of headroom. Streams the file twice (peak, then
//! rewrite in place), so a long recording costs no memory.

use crate::audio::wavout::HEADER;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const TARGET: f32 = 0.989; // -0.1 dBFS
const CHUNK: usize = 1 << 16;

/// Calls `each` on consecutive chunks of the sample data; a chunk it returns `true` for is written back.
fn pass(f: &mut File, samples: u64, mut each: impl FnMut(&mut [u8]) -> bool) -> io::Result<()> {
    let mut buf = vec![0u8; CHUNK];
    let (mut at, mut left) = (HEADER as u64, samples * 2);
    while left > 0 {
        let n = left.min(CHUNK as u64) as usize;
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(&mut buf[..n])?;
        if each(&mut buf[..n]) {
            f.seek(SeekFrom::Start(at))?;
            f.write_all(&buf[..n])?;
        }
        (at, left) = (at + n as u64, left - n as u64);
    }
    Ok(())
}

/// Scales the 16-bit WAV at `path` (as written by [`super::wav::Wav`]) and returns the gain applied
/// (1.0 when the file is silent or already full scale).
pub(super) fn optimise(path: &Path) -> io::Result<f32> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    let samples = f.metadata()?.len().saturating_sub(HEADER as u64) / 2;
    let mut peak = 0i32;
    pass(&mut f, samples, |b| {
        let chunk = b
            .chunks_exact(2)
            .map(|s| (i16::from_le_bytes([s[0], s[1]]) as i32).abs());
        peak = chunk.fold(peak, i32::max);
        false
    })?;
    let gain = TARGET * 32767.0 / peak.max(1) as f32;
    if peak == 0 || gain <= 1.0 {
        return Ok(1.0);
    }
    pass(&mut f, samples, |b| {
        for s in b.chunks_exact_mut(2) {
            let v = (i16::from_le_bytes([s[0], s[1]]) as f32 * gain)
                .round()
                .clamp(-32767.0, 32767.0);
            s.copy_from_slice(&(v as i16).to_le_bytes());
        }
        true
    })?;
    f.flush()?;
    Ok(gain)
}

#[cfg(test)]
mod tests {
    use super::super::wav::Wav;
    use super::*;

    fn write(frames: &[[f32; 2]]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "openfader-opt-{}-{}.wav",
            std::process::id(),
            N.fetch_add(1, Relaxed)
        );
        let p = std::env::temp_dir().join(name);
        let mut w = Wav::new(std::io::BufWriter::new(File::create(&p).unwrap()), 8000).unwrap();
        w.write(frames).unwrap();
        w.finish().unwrap();
        p
    }

    fn samples(p: &Path) -> Vec<i16> {
        std::fs::read(p).unwrap()[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn quiet_file_is_raised_to_the_target_peak_keeping_its_shape() {
        // 200 000 frames span several chunks
        let frames: Vec<[f32; 2]> = (0..200_000)
            .map(|i| [0.1 * ((i % 50) as f32 / 25.0 - 1.0), 0.0])
            .collect();
        let p = write(&frames);
        let gain = optimise(&p).unwrap();
        let s = samples(&p);
        std::fs::remove_file(&p).unwrap();
        assert!((gain - 9.89).abs() < 0.1, "{gain}");
        assert_eq!(s.len(), 400_000);
        let peak = s.iter().map(|v| v.unsigned_abs()).max().unwrap();
        assert!((peak as f32 / 32767.0 - TARGET).abs() < 0.002, "{peak}");
        assert!(s[1] == 0 && s[3] == 0, "silent channel stays silent");
        assert!(
            s[0] < 0 && s[2 * 49] > 0,
            "the waveform keeps its sign pattern"
        );
    }

    #[test]
    fn loud_or_silent_files_are_left_alone() {
        let loud = write(&[[1.0, -1.0], [0.5, 0.5]]);
        let silent = write(&[[0.0; 2]; 100]);
        let (before_l, before_s) = (
            std::fs::read(&loud).unwrap(),
            std::fs::read(&silent).unwrap(),
        );
        assert_eq!(
            (optimise(&loud).unwrap(), optimise(&silent).unwrap()),
            (1.0, 1.0)
        );
        let same = std::fs::read(&loud).unwrap() == before_l
            && std::fs::read(&silent).unwrap() == before_s;
        std::fs::remove_file(&loud).unwrap();
        std::fs::remove_file(&silent).unwrap();
        assert!(same);
    }
}
