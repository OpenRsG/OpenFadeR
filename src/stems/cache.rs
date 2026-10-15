//! Cache of separated stems: `$XDG_CACHE_HOME/openfader/stems/<key>.stems` (QUALITY) or `<key>.fast.stems` (SPEED), the key
//! being the analysis cache's ([`crate::analysis::cache::key`]: hash of path, size and mtime), so a changed audio file never
//! finds an old entry. Two files because the two [`Quality`]s differ: a SPEED entry never answers a QUALITY request, a
//! QUALITY entry answers either, and storing a QUALITY entry drops the SPEED one it supersedes.
//!
//! Own format, little-endian: `"RKST"`, `version u32`, `rate u32`, `frames u64`, then the four stems
//! ([`crate::audio::VOCAL`]…) as interleaved stereo `i16`, `frames` frames each. A wrong magic, version, rate or length, or
//! a short or over-long file, is an error the caller reports and recomputes over; it is never trusted. Only complete stems are
//! stored, and a hit is read straight into the [`Stems`] of the job, which then counts as separated.

use super::Quality;
use crate::analysis::cache;
use crate::audio::{STEM_COUNT, Stems};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"RKST";
/// Bump when the model, its post-processing or this layout changes.
const VERSION: u32 = 1;
const HEADER: usize = 20;
/// Frames moved per read or write.
const CHUNK: usize = 1 << 15;

pub fn dir() -> Option<PathBuf> {
    Some(cache::root()?.join("stems"))
}

fn file(dir: &Path, audio: &Path, quality: Quality) -> Result<PathBuf, String> {
    let ext = match quality {
        Quality::Quality => "stems",
        Quality::Speed => "fast.stems",
    };
    Ok(dir.join(format!("{}.{ext}", cache::key(audio)?)))
}

fn header(rate: u32, frames: usize) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..4].copy_from_slice(MAGIC);
    h[4..8].copy_from_slice(&VERSION.to_le_bytes());
    h[8..12].copy_from_slice(&rate.to_le_bytes());
    h[12..].copy_from_slice(&(frames as u64).to_le_bytes());
    h
}

/// Reads the entry at `path` into `into` (whose length must match) and marks it separated: `Ok(false)` = no such entry.
fn read_from(path: &Path, rate: u32, into: &Stems) -> Result<bool, String> {
    let frames = into.frames();
    let mut f = match std::fs::File::open(path) {
        Ok(f) => std::io::BufReader::with_capacity(1 << 20, f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let bad = |what: &str| format!("{}: {what}", path.display());
    let mut h = [0u8; HEADER];
    f.read_exact(&mut h).map_err(|_| bad("short header"))?;
    if h != header(rate, frames) {
        return Err(bad("different version, sample rate or length"));
    }
    let mut bytes = vec![0u8; 4 * CHUNK];
    let mut samples = vec![0i16; 2 * CHUNK];
    for k in 0..STEM_COUNT {
        for from in (0..frames).step_by(CHUNK) {
            let n = CHUNK.min(frames - from);
            f.read_exact(&mut bytes[..4 * n])
                .map_err(|_| bad("short file"))?;
            for (s, b) in samples.iter_mut().zip(bytes[..4 * n].chunks_exact(2)) {
                *s = i16::from_le_bytes([b[0], b[1]]);
            }
            into.write(k, from, &samples[..2 * n]);
        }
    }
    if f.read(&mut [0u8; 1]).map_err(|e| bad(&e.to_string()))? != 0 {
        return Err(bad("trailing bytes"));
    }
    into.finish();
    Ok(true)
}

fn write_to(path: &Path, rate: u32, s: &Stems) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    std::fs::create_dir_all(path.parent().ok_or("no cache folder")?).map_err(err)?;
    let part = path.with_extension("part");
    let mut f =
        std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&part).map_err(err)?);
    f.write_all(&header(rate, s.frames())).map_err(err)?;
    let (mut samples, mut bytes) = (vec![0i16; 2 * CHUNK], vec![0u8; 4 * CHUNK]);
    for k in 0..STEM_COUNT {
        for from in (0..s.frames()).step_by(CHUNK) {
            let n = CHUNK.min(s.frames() - from);
            s.read(k, from, &mut samples[..2 * n]);
            for (b, x) in bytes.chunks_exact_mut(2).zip(&samples[..2 * n]) {
                b.copy_from_slice(&x.to_le_bytes());
            }
            f.write_all(&bytes[..4 * n]).map_err(err)?;
        }
    }
    f.flush().map_err(err)?;
    drop(f);
    std::fs::rename(&part, path).map_err(err)
}

fn dir_or_err() -> Result<PathBuf, String> {
    dir().ok_or_else(|| "no cache directory (set XDG_CACHE_HOME or HOME)".into())
}

fn load_in(
    dir: &Path,
    audio: &Path,
    rate: u32,
    into: &Stems,
    quality: Quality,
) -> Result<bool, String> {
    let wanted: &[Quality] = match quality {
        Quality::Quality => &[Quality::Quality],
        Quality::Speed => &[Quality::Quality, Quality::Speed],
    };
    let mut err = None;
    for &q in wanted {
        match read_from(&file(dir, audio, q)?, rate, into) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(e) => err = err.or(Some(e)), // a bad entry must not hide a good one
        }
    }
    err.map_or(Ok(false), Err)
}

fn store_in(
    dir: &Path,
    audio: &Path,
    rate: u32,
    s: &Stems,
    quality: Quality,
) -> Result<(), String> {
    if !s.is_complete() {
        return Err("stems are not complete".into());
    }
    write_to(&file(dir, audio, quality)?, rate, s)?;
    if quality == Quality::Quality {
        let _ = std::fs::remove_file(file(dir, audio, Quality::Speed)?); // superseded; absent is fine
    }
    Ok(())
}

/// Whether an entry good enough for `quality` exists (no read; a corrupt one still counts and is found by [`load`]).
pub fn has(audio: &Path, quality: Quality) -> bool {
    let wanted: &[Quality] = match quality {
        Quality::Quality => &[Quality::Quality],
        Quality::Speed => &[Quality::Quality, Quality::Speed],
    };
    dir().is_some_and(|d| {
        wanted
            .iter()
            .any(|&q| file(&d, audio, q).is_ok_and(|f| f.is_file()))
    })
}

/// Fills `into` (the stems of `audio` decoded to `into.frames()` frames at `rate`) from the cache entry good enough for
/// `quality` and marks it separated: `Ok(false)` = not cached, `into` untouched; `Err` = corrupt or stale.
pub fn load(audio: &Path, rate: u32, into: &Stems, quality: Quality) -> Result<bool, String> {
    load_in(&dir_or_err()?, audio, rate, into, quality)
}

/// Saves the complete `s`, separated at `quality`, for `audio`, replacing any old entry of that quality.
pub fn store(audio: &Path, rate: u32, s: &Stems, quality: Quality) -> Result<(), String> {
    store_in(&dir_or_err()?, audio, rate, s, quality)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Grid;

    /// Finished stems whose stem `k` frame `i` is a function of both.
    fn stems(frames: usize) -> Stems {
        Stems::complete(std::array::from_fn(|k| {
            (0..2 * frames)
                .map(|i| (i as i16).wrapping_mul(k as i16 + 3))
                .collect()
        }))
    }

    /// Stems of the same length that nothing has filled in.
    fn blank(frames: usize) -> Stems {
        Stems::new(
            frames,
            Grid {
                len: 64,
                overlap: 8,
            },
        )
    }

    fn dump(s: &Stems) -> Vec<Vec<i16>> {
        (0..STEM_COUNT)
            .map(|k| {
                let mut v = vec![0; 2 * s.frames()];
                s.read(k, 0, &mut v);
                v
            })
            .collect()
    }

    /// A fresh folder holding a stand-in audio file.
    fn scratch(tag: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("rk-stems-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let audio = d.join("a.mp3");
        std::fs::write(&audio, b"audio").unwrap();
        (d, audio)
    }

    #[test]
    fn roundtrip_and_misses() {
        let (d, audio) = scratch("rt");
        let path = file(&d.join("stems"), &audio, Quality::Quality).unwrap();
        assert!(!read_from(&path, 44100, &blank(100)).unwrap());
        write_to(&path, 44100, &stems(100)).unwrap();
        let back = blank(100);
        assert!(read_from(&path, 44100, &back).unwrap());
        assert!(back.is_complete(), "a hit counts as separated");
        assert_eq!(dump(&back), dump(&stems(100)));
        assert!(
            read_from(&path, 48000, &blank(100)).is_err(),
            "other rate is stale"
        );
        assert!(
            read_from(&path, 44100, &blank(101)).is_err(),
            "other length is stale"
        );
        std::fs::write(&path, &std::fs::read(&path).unwrap()[..HEADER + 100]).unwrap();
        assert!(
            read_from(&path, 44100, &blank(100)).is_err(),
            "truncated file"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_long_track_crosses_chunk_boundaries_intact() {
        let (d, audio) = scratch("long");
        let frames = 2 * CHUNK + 777;
        store_in(&d, &audio, 44100, &stems(frames), Quality::Quality).unwrap();
        let back = blank(frames);
        assert!(load_in(&d, &audio, 44100, &back, Quality::Quality).unwrap());
        assert_eq!(dump(&back), dump(&stems(frames)));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn key_follows_the_audio_file() {
        let d = std::env::temp_dir().join(format!("rk-stems-key-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (a, b) = (d.join("a.mp3"), d.join("b.mp3"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let ka = file(&d, &a, Quality::Quality).unwrap();
        assert_eq!(ka, file(&d, &a, Quality::Quality).unwrap());
        assert_ne!(
            ka,
            file(&d, &b, Quality::Quality).unwrap(),
            "path is part of the key"
        );
        std::fs::write(&a, b"xy").unwrap();
        assert_ne!(
            ka,
            file(&d, &a, Quality::Quality).unwrap(),
            "size is part of the key"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn only_complete_stems_are_stored() {
        let (d, audio) = scratch("partial");
        assert!(store_in(&d, &audio, 44100, &blank(100), Quality::Quality).is_err());
        assert!(!file(&d, &audio, Quality::Quality).unwrap().exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_speed_entry_never_answers_quality_but_a_quality_entry_answers_both() {
        let (d, audio) = scratch("q");
        let (quality, speed) = (Quality::Quality, Quality::Speed);
        let load = |q, frames| {
            let into = blank(frames);
            load_in(&d, &audio, 44100, &into, q).map(|hit| hit.then(|| dump(&into)))
        };
        assert!(load(quality, 100).unwrap().is_none() && load(speed, 100).unwrap().is_none());

        store_in(&d, &audio, 44100, &stems(100), speed).unwrap();
        assert!(
            load(quality, 100).unwrap().is_none(),
            "speed does not satisfy quality"
        );
        assert_eq!(load(speed, 100).unwrap().unwrap(), dump(&stems(100)));

        let better = || Stems::complete(std::array::from_fn(|k| vec![k as i16; 200]));
        store_in(&d, &audio, 44100, &better(), quality).unwrap();
        assert_eq!(load(quality, 100).unwrap().unwrap(), dump(&better()));
        assert_eq!(
            load(speed, 100).unwrap().unwrap(),
            dump(&better()),
            "quality satisfies speed"
        );
        assert!(
            !file(&d, &audio, speed).unwrap().exists(),
            "the superseded speed entry is dropped"
        );

        // A bad quality entry does not hide a good speed one, but a quality request still reports it.
        store_in(&d, &audio, 44100, &stems(100), speed).unwrap();
        std::fs::write(file(&d, &audio, quality).unwrap(), b"RKST").unwrap();
        assert_eq!(load(speed, 100).unwrap().unwrap(), dump(&stems(100)));
        assert!(load(quality, 100).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }
}
