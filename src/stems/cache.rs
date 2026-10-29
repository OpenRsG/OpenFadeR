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
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"RKST";
/// Bump when the model, its post-processing or this layout changes.
const VERSION: u32 = 1;
const HEADER: usize = 20;
/// Frames moved per read or write.
const CHUNK: usize = 1 << 15;
/// Complete entries and interrupted writes together stay within 4 GiB.
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

fn check_cancelled(cancelled: &dyn Fn() -> bool) -> Result<(), String> {
    if cancelled() {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

fn lock(dir: &Path, exclusive: bool, cancelled: &dyn Fn() -> bool) -> Result<File, String> {
    check_cancelled(cancelled)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("cache.lock"))
        .map_err(|e| format!("stems cache lock: {e}"))?;
    loop {
        check_cancelled(cancelled)?;
        match if exclusive {
            f.try_lock()
        } else {
            f.try_lock_shared()
        } {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(100))
            }
            Err(e) => return Err(format!("stems cache lock: {e}")),
        }
    }
    Ok(f)
}

fn owned(name: &str) -> bool {
    let Some((key, suffix)) = name.split_once('.') else {
        return false;
    };
    key.len() == 16
        && key.bytes().all(|b| b.is_ascii_hexdigit())
        && matches!(suffix, "stems" | "fast.stems" | "part" | "fast.part")
}

/// Caller holds the directory's exclusive lock; readers and writers in other processes hold the same lock.
fn prune(
    dir: &Path,
    budget: u64,
    keep: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
) -> Result<bool, String> {
    check_cancelled(cancelled)?;
    let mut entries = Vec::new();
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir).map_err(|e| format!("stems cache: {e}"))? {
        check_cancelled(cancelled)?;
        let entry = entry.map_err(|e| format!("stems cache: {e}"))?;
        if !owned(&entry.file_name().to_string_lossy())
            || !entry
                .file_type()
                .map_err(|e| format!("stems cache: {e}"))?
                .is_file()
        {
            continue;
        }
        let path = entry.path();
        if path.extension().is_some_and(|s| s == "part") {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            continue;
        }
        let m = entry
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        total = total
            .checked_add(m.len())
            .ok_or("stems cache size overflow")?;
        entries.push((
            m.modified()
                .map_err(|e| format!("{}: {e}", path.display()))?,
            path,
            m.len(),
        ));
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (_, path, bytes) in entries {
        check_cancelled(cancelled)?;
        if total <= budget {
            break;
        }
        if keep == Some(path.as_path()) {
            continue;
        }
        std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        total -= bytes;
    }
    Ok(total <= budget)
}

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

fn entry_bytes(frames: usize) -> Result<u64, String> {
    (frames as u64)
        .checked_mul(4 * STEM_COUNT as u64)
        .and_then(|n| n.checked_add(HEADER as u64))
        .ok_or_else(|| "stems cache size overflow".into())
}

/// Reads the entry at `path` into `into` (whose length must match) and marks it separated: `Ok(false)` = no such entry.
fn read_from(
    path: &Path,
    rate: u32,
    into: &Stems,
    cancelled: &dyn Fn() -> bool,
) -> Result<bool, String> {
    check_cancelled(cancelled)?;
    let frames = into.frames();
    let bad = |what: &str| format!("{}: {what}", path.display());
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !meta.is_file() || meta.len() != entry_bytes(frames)? {
        return Err(bad("not a regular cache file of the expected length"));
    }
    let mut f = match std::fs::File::open(path) {
        Ok(f) => std::io::BufReader::with_capacity(1 << 20, f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let meta = f
        .get_ref()
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() || meta.len() != entry_bytes(frames)? {
        return Err(bad("cache changed while opening"));
    }
    let mut h = [0u8; HEADER];
    f.read_exact(&mut h).map_err(|_| bad("short header"))?;
    if h != header(rate, frames) {
        return Err(bad("different version, sample rate or length"));
    }
    let mut bytes = vec![0u8; 4 * CHUNK];
    let mut samples = vec![0i16; 2 * CHUNK];
    for k in 0..STEM_COUNT {
        for from in (0..frames).step_by(CHUNK) {
            check_cancelled(cancelled)?;
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
    check_cancelled(cancelled)?;
    into.finish();
    Ok(true)
}

fn write_to(path: &Path, rate: u32, s: &Stems, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
    check_cancelled(cancelled)?;
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    std::fs::create_dir_all(path.parent().ok_or("no cache folder")?).map_err(err)?;
    let part = path.with_extension("part");
    let result = (|| {
        let mut f =
            std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&part).map_err(err)?);
        f.write_all(&header(rate, s.frames())).map_err(err)?;
        let (mut samples, mut bytes) = (vec![0i16; 2 * CHUNK], vec![0u8; 4 * CHUNK]);
        for k in 0..STEM_COUNT {
            for from in (0..s.frames()).step_by(CHUNK) {
                check_cancelled(cancelled)?;
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
        check_cancelled(cancelled)?;
        std::fs::rename(&part, path).map_err(err)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
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
    cancelled: &dyn Fn() -> bool,
) -> Result<bool, String> {
    check_cancelled(cancelled)?;
    if !dir.is_dir() {
        return Ok(false);
    }
    let wanted: &[Quality] = match quality {
        Quality::Quality => &[Quality::Quality],
        Quality::Speed => &[Quality::Quality, Quality::Speed],
    };
    let mut err = None;
    for &q in wanted {
        check_cancelled(cancelled)?;
        let path = file(dir, audio, q)?;
        let _lock = lock(dir, false, cancelled)?;
        match read_from(&path, rate, into, cancelled) {
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
    budget: u64,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    check_cancelled(cancelled)?;
    if !s.is_complete() {
        return Err("stems are not complete".into());
    }
    let path = file(dir, audio, quality)?;
    let superseded = if quality == Quality::Quality {
        Some(file(dir, audio, Quality::Speed)?)
    } else {
        None
    };
    let _lock = lock(dir, true, cancelled)?;
    let bytes = entry_bytes(s.frames())?;
    if bytes > budget {
        prune(dir, budget, None, cancelled)?;
        return Ok(());
    }
    if !prune(dir, budget - bytes, Some(&path), cancelled)? {
        return Ok(());
    }
    write_to(&path, rate, s, cancelled)?;
    if let Some(path) = superseded {
        let _ = std::fs::remove_file(path); // superseded; absent is fine
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
/// `quality` and marks it separated: `Ok(false)` = not cached, `into` untouched; `Err` = corrupt, stale or cancelled.
pub fn load(
    audio: &Path,
    rate: u32,
    into: &Stems,
    quality: Quality,
    cancelled: &dyn Fn() -> bool,
) -> Result<bool, String> {
    load_in(&dir_or_err()?, audio, rate, into, quality, cancelled)
}

/// Saves the complete `s`, separated at `quality`, for `audio`, replacing any old entry of that quality.
pub fn store(
    audio: &Path,
    rate: u32,
    s: &Stems,
    quality: Quality,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    store_in(
        &dir_or_err()?,
        audio,
        rate,
        s,
        quality,
        MAX_BYTES,
        cancelled,
    )
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
        assert!(!read_from(&path, 44100, &blank(100), &|| false).unwrap());
        write_to(&path, 44100, &stems(100), &|| false).unwrap();
        let back = blank(100);
        assert!(read_from(&path, 44100, &back, &|| false).unwrap());
        assert!(back.is_complete(), "a hit counts as separated");
        assert_eq!(dump(&back), dump(&stems(100)));
        assert!(
            read_from(&path, 48000, &blank(100), &|| false).is_err(),
            "other rate is stale"
        );
        assert!(
            read_from(&path, 44100, &blank(101), &|| false).is_err(),
            "other length is stale"
        );
        std::fs::write(&path, &std::fs::read(&path).unwrap()[..HEADER + 100]).unwrap();
        assert!(
            read_from(&path, 44100, &blank(100), &|| false).is_err(),
            "truncated file"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_long_track_crosses_chunk_boundaries_intact() {
        let (d, audio) = scratch("long");
        let frames = 2 * CHUNK + 777;
        store_in(
            &d,
            &audio,
            44100,
            &stems(frames),
            Quality::Quality,
            MAX_BYTES,
            &|| false,
        )
        .unwrap();
        let back = blank(frames);
        assert!(load_in(&d, &audio, 44100, &back, Quality::Quality, &|| false).unwrap());
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
        assert!(
            store_in(
                &d,
                &audio,
                44100,
                &blank(100),
                Quality::Quality,
                MAX_BYTES,
                &|| false
            )
            .is_err()
        );
        assert!(!file(&d, &audio, Quality::Quality).unwrap().exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_speed_entry_never_answers_quality_but_a_quality_entry_answers_both() {
        let (d, audio) = scratch("q");
        let (quality, speed) = (Quality::Quality, Quality::Speed);
        let load = |q, frames| {
            let into = blank(frames);
            load_in(&d, &audio, 44100, &into, q, &|| false).map(|hit| hit.then(|| dump(&into)))
        };
        assert!(load(quality, 100).unwrap().is_none() && load(speed, 100).unwrap().is_none());

        store_in(&d, &audio, 44100, &stems(100), speed, MAX_BYTES, &|| false).unwrap();
        assert!(
            load(quality, 100).unwrap().is_none(),
            "speed does not satisfy quality"
        );
        assert_eq!(load(speed, 100).unwrap().unwrap(), dump(&stems(100)));

        let better = || Stems::complete(std::array::from_fn(|k| vec![k as i16; 200]));
        store_in(&d, &audio, 44100, &better(), quality, MAX_BYTES, &|| false).unwrap();
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
        store_in(&d, &audio, 44100, &stems(100), speed, MAX_BYTES, &|| false).unwrap();
        std::fs::write(file(&d, &audio, quality).unwrap(), b"RKST").unwrap();
        assert_eq!(load(speed, 100).unwrap().unwrap(), dump(&stems(100)));
        assert!(load(quality, 100).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn pruning_keeps_recent_entries_and_never_deletes_unrelated_files() {
        use std::time::{Duration, UNIX_EPOCH};
        let (d, a) = scratch("budget");
        let (b, c) = (d.join("b.wav"), d.join("c.wav"));
        std::fs::write(&b, b"audio").unwrap();
        std::fs::write(&c, b"audio").unwrap();
        let budget = 2 * (HEADER as u64 + 16 * 8);
        let put = |audio: &Path| {
            store_in(
                &d,
                audio,
                44100,
                &stems(8),
                Quality::Quality,
                budget,
                &|| false,
            )
            .unwrap()
        };
        put(&a);
        let pa = file(&d, &a, Quality::Quality).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&pa)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        put(&b);
        let pb = file(&d, &b, Quality::Quality).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&pb)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(2))
            .unwrap();
        let partial = d.join("0123456789abcdef.part");
        std::fs::write(&partial, b"interrupted").unwrap();
        let unrelated = d.join("notes.txt");
        std::fs::write(&unrelated, b"keep").unwrap();
        put(&c);
        assert!(!pa.exists());
        assert!(!partial.exists());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
        assert_eq!(std::fs::read(&a).unwrap(), b"audio");
        for audio in [&b, &c] {
            let back = blank(8);
            assert!(load_in(&d, audio, 44100, &back, Quality::Quality, &|| false).unwrap());
            assert_eq!(dump(&back), dump(&stems(8)));
        }
        std::fs::write(&b, b"changed audio").unwrap();
        put(&b);
        assert!(!pb.exists(), "the unreachable old key is evicted");
        let back = blank(8);
        assert!(load_in(&d, &b, 44100, &back, Quality::Quality, &|| false).unwrap());
        assert_eq!(dump(&back), dump(&stems(8)));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_replacement_that_cannot_fit_preserves_the_existing_cache() {
        let (d, audio) = scratch("oversized");
        let budget = HEADER as u64 + 16 * 8;
        store_in(
            &d,
            &audio,
            44100,
            &stems(8),
            Quality::Quality,
            budget,
            &|| false,
        )
        .unwrap();
        store_in(
            &d,
            &audio,
            44100,
            &stems(16),
            Quality::Quality,
            budget,
            &|| false,
        )
        .unwrap();
        let back = blank(8);
        assert!(load_in(&d, &audio, 44100, &back, Quality::Quality, &|| false).unwrap());
        assert_eq!(dump(&back), dump(&stems(8)));
        store_in(
            &d,
            &audio,
            44100,
            &stems(8),
            Quality::Quality,
            budget,
            &|| false,
        )
        .unwrap();
        assert!(load_in(&d, &audio, 44100, &blank(8), Quality::Quality, &|| false).unwrap());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn truncated_cache_never_exposes_partial_stem_audio() {
        let (d, audio) = scratch("truncated-early");
        let frames = CHUNK + 2;
        let path = file(&d, &audio, Quality::Quality).unwrap();
        write_to(&path, 44100, &stems(frames), &|| false).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len((HEADER + 4 * CHUNK + 4) as u64)
            .unwrap();
        let back = blank(frames);
        assert!(read_from(&path, 44100, &back, &|| false).is_err());
        assert!(dump(&back).iter().flatten().all(|s| *s == 0));
        assert!(!back.is_complete());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn cancelled_cache_reads_never_publish_partially_filled_audio() {
        let (d, audio) = scratch("cancel-read");
        let frames = 2 * CHUNK + 1;
        let path = file(&d, &audio, Quality::Quality).unwrap();
        write_to(&path, 44100, &stems(frames), &|| false).unwrap();
        let back = blank(frames);
        let checks = std::cell::Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        };
        assert_eq!(
            read_from(&path, 44100, &back, &cancelled).unwrap_err(),
            "cancelled"
        );
        assert!(!back.is_complete());
        assert!((0..frames).all(|i| !back.ready(i)));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn cancelled_cache_writes_preserve_the_old_entry_and_remove_the_partial() {
        let (d, audio) = scratch("cancel-write");
        let frames = 2 * CHUNK + 1;
        let path = file(&d, &audio, Quality::Quality).unwrap();
        let original = stems(frames);
        write_to(&path, 44100, &original, &|| false).unwrap();
        let replacement = Stems::complete(std::array::from_fn(|_| vec![7; 2 * frames]));
        let checks = std::cell::Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        };
        assert_eq!(
            write_to(&path, 44100, &replacement, &cancelled).unwrap_err(),
            "cancelled"
        );
        assert!(!path.with_extension("part").exists());
        let back = blank(frames);
        assert!(read_from(&path, 44100, &back, &|| false).unwrap());
        assert_eq!(dump(&back), dump(&original));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_cancelled_lock_wait_finishes_before_the_holder_releases() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering::Relaxed},
            mpsc,
        };
        use std::time::Duration;
        let (d, _) = scratch("cancel-lock");
        let held = lock(&d, true, &|| false).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (entered, started) = mpsc::channel();
        let (finished, outcome) = mpsc::channel();
        let worker = {
            let (d, cancelled) = (d.clone(), cancelled.clone());
            std::thread::spawn(move || {
                let first = std::cell::Cell::new(true);
                let result = lock(&d, false, &|| {
                    if first.replace(false) {
                        entered.send(()).unwrap();
                        false
                    } else {
                        cancelled.load(Relaxed)
                    }
                })
                .map(drop);
                finished.send(result).unwrap();
            })
        };
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        cancelled.store(true, Relaxed);
        let result = outcome.recv_timeout(Duration::from_secs(2));
        drop(held);
        worker.join().unwrap();
        assert_eq!(result.unwrap().unwrap_err(), "cancelled");
        std::fs::remove_dir_all(d).unwrap();
    }
}
