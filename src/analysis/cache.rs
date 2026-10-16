//! Persistent cache of our own [`analyze`] results (beats, key, 3-band waveforms) so a track is analysed once.
//! One file per audio file: `<cache folder>/analysis/<hash of path + size + mtime>.bin` (the cache folder is
//! [`crate::paths::cache_dir`]). Changing the audio file changes the name, so a stale entry is never found.
//!
//! Own format, little-endian: `"RKAC"`, `version u32`, `profile u64` (what the result was
//! made with, [`Profile`]), `key_len u8` + key text (0 = none), then counted tables: beats (`bar_pos u16, bpm_x100 u16,
//! time_ms u32`), 3-band detail and 3-band preview (`[u8; 3]` each) and vocal regions (`start_ms u32, end_ms u32`), each
//! with its `count u32` first. A wrong magic or version, a short or over-long file is an error the caller reports and
//! recomputes over; it is never trusted. A result whose profile does not cover the current options is recomputed.

use super::{
    detect::VocalRegion,
    options::{self, Options, Profile},
    Analysis, Beat,
};
use crate::paths::at;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"RKAC";
/// Bump when the layout or the detectors change enough that old results should be recomputed.
const VERSION: u32 = 3;

/// [`crate::paths::cache_dir`]: the parent of every cache folder (analysis, stems, models).
pub fn root() -> Option<PathBuf> {
    crate::paths::cache_dir()
}

/// `<root>/analysis`.
pub fn dir() -> Option<PathBuf> {
    Some(root()?.join("analysis"))
}

/// Cache key of an audio file: FNV-1a 64 over path, size and mtime as 16 hex digits (stable across Rust versions, unlike `DefaultHasher`).
pub fn key(audio: &Path) -> Result<String, String> {
    let m = at(std::fs::metadata(audio), audio)?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let bytes = audio.to_string_lossy();
    let h = fnv1a(
        bytes
            .as_bytes()
            .iter()
            .chain(&m.len().to_le_bytes())
            .chain(&mtime.to_le_bytes())
            .copied(),
    );
    Ok(format!("{h:016x}"))
}

/// FNV-1a 64 of `bytes` (stable across Rust versions, unlike `DefaultHasher`): the cache file names.
pub fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

fn file_name(audio: &Path) -> Result<String, String> {
    Ok(format!("{}.bin", key(audio)?))
}

fn encode(a: &Analysis, profile: Profile) -> Vec<u8> {
    let key = a.key.as_deref().unwrap_or("").as_bytes();
    let key = &key[..key.len().min(255)];
    let mut o = Vec::with_capacity(
        40 + a.beats.len() * 8
            + (a.three_band_detail.len() + a.three_band_preview.len()) * 3
            + a.vocal.len() * 8,
    );
    o.extend_from_slice(MAGIC);
    o.extend_from_slice(&VERSION.to_le_bytes());
    o.extend_from_slice(&profile.0.to_le_bytes());
    o.push(key.len() as u8);
    o.extend_from_slice(key);
    o.extend_from_slice(&(a.beats.len() as u32).to_le_bytes());
    for b in &a.beats {
        o.extend_from_slice(&b.bar_pos.to_le_bytes());
        o.extend_from_slice(&b.bpm_x100.to_le_bytes());
        o.extend_from_slice(&b.time_ms.to_le_bytes());
    }
    for t in [&a.three_band_detail, &a.three_band_preview] {
        o.extend_from_slice(&(t.len() as u32).to_le_bytes());
        o.extend(t.iter().flatten());
    }
    o.extend_from_slice(&(a.vocal.len() as u32).to_le_bytes());
    for v in &a.vocal {
        o.extend_from_slice(&v.start_ms.to_le_bytes());
        o.extend_from_slice(&v.end_ms.to_le_bytes());
    }
    o
}

/// Bounds-checked front-to-back reader.
struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("truncated".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// `count u32` then `count` records of `size` bytes; the count is checked against what is left before allocating.
    fn table(&mut self, size: usize) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n.checked_mul(size).ok_or("bad count")?)
    }
}

fn decode(b: &[u8]) -> Result<(Analysis, Profile), String> {
    let mut c = Cur(b);
    if c.take(4)? != MAGIC {
        return Err("not an analysis cache file".into());
    }
    let v = c.u32()?;
    if v != VERSION {
        return Err(format!("cache version {v}, expected {VERSION}"));
    }
    let profile = Profile(u64::from_le_bytes(c.take(8)?.try_into().unwrap()));
    let n = c.take(1)?[0] as usize;
    let key = std::str::from_utf8(c.take(n)?).map_err(|_| "bad key text")?;
    let beats: Vec<Beat> = c
        .table(8)?
        .chunks_exact(8)
        .map(|e| Beat {
            bar_pos: u16::from_le_bytes([e[0], e[1]]),
            bpm_x100: u16::from_le_bytes([e[2], e[3]]),
            time_ms: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
        })
        .collect();
    let tri = |t: &[u8]| {
        t.chunks_exact(3)
            .map(|e| [e[0], e[1], e[2]])
            .collect::<Vec<_>>()
    };
    let three_band_detail = tri(c.table(3)?);
    let three_band_preview = tri(c.table(3)?);
    let vocal = c
        .table(8)?
        .chunks_exact(8)
        .map(|e| VocalRegion {
            start_ms: u32::from_le_bytes([e[0], e[1], e[2], e[3]]),
            end_ms: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
        })
        .collect();
    if !c.0.is_empty() {
        return Err("trailing bytes".into());
    }
    // Phrases are not stored: they are a pure function of the beats and the 3-band detail, so a cache hit gets the same ones as a fresh analysis
    // (none while the Phrase part of Track Analysis Setting is off).
    let phrases = super::phrases_for(&options::get(), &beats, &three_band_detail);
    Ok((
        Analysis {
            beats,
            three_band_detail,
            three_band_preview,
            key: (n > 0).then(|| key.to_owned()),
            vocal,
            phrases,
            ..Default::default()
        },
        profile,
    ))
}

fn load_from(dir: &Path, audio: &Path) -> Result<Option<(Analysis, Profile)>, String> {
    let file = dir.join(file_name(audio)?);
    match std::fs::read(&file) {
        Ok(b) => decode(&b)
            .map(Some)
            .map_err(|e| format!("analysis cache ignored ({e})")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("analysis cache ignored ({e})")),
    }
}

fn store_in(dir: &Path, audio: &Path, a: &Analysis, profile: Profile) -> Result<(), String> {
    at(std::fs::create_dir_all(dir), dir)?;
    let name = file_name(audio)?;
    // Write beside the target and rename, so a concurrent reader (deck load vs bulk worker) never sees half a file.
    let tmp = dir.join(format!(
        "{name}.{}.{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, encode(a, profile))
        .and_then(|()| std::fs::rename(&tmp, dir.join(name)))
        .map_err(|e| format!("analysis cache write: {e}"))
}

fn cache_dir() -> Result<PathBuf, String> {
    dir().ok_or_else(|| "no cache directory (set XDG_CACHE_HOME or HOME)".to_owned())
}

/// The cached analysis of `audio` as it was stored, whatever options it was made with (what a track list shows):
/// `Ok(None)` = not cached; `Err` = the file is corrupt, old or unreadable and is ignored (report it, then recompute and
/// [`store`] over it).
pub fn load(audio: &Path) -> Result<Option<Analysis>, String> {
    Ok(load_from(&cache_dir()?, audio)?.map(|x| x.0))
}

/// The vocal regions the analysis cache holds for `audio` ([`super::detect::vocal_regions`], an inferred heuristic);
/// empty when the track was not analysed or the Vocal part was off. What VIEW > Layout > Vocal draws.
pub fn vocal(audio: &Path) -> Vec<VocalRegion> {
    load(audio)
        .ok()
        .flatten()
        .map_or_else(Vec::new, |a| a.vocal)
}

/// The cached analysis of `audio` when it covers `o` (every part `o` runs, the same grid settings), with the parts `o`
/// switches off emptied; `Ok(None)` = not cached or made with other options.
pub fn load_for(audio: &Path, o: &Options) -> Result<Option<Analysis>, String> {
    Ok(load_from(&cache_dir()?, audio)?
        .filter(|x| x.1.covers(o.profile()))
        .map(|mut x| {
            o.restrict(&mut x.0);
            x.0
        }))
}

/// Saves `a`, made with `profile`, for `audio`, replacing any old entry.
pub fn store(audio: &Path, a: &Analysis, profile: Profile) -> Result<(), String> {
    store_in(&cache_dir()?, audio, a, profile)
}

/// Where an analysis came from.
pub enum Source {
    Cache,
    /// Freshly computed (and stored); `ignored` says why an existing but unusable cache file was not trusted.
    Computed {
        ignored: Option<String>,
    },
}

/// Cached analysis under the current [`options`], else [`super::analyze`] of the decoded `samples` + [`store`].
/// ponytail: a failed cache write is dropped silently; the result is still returned.
pub fn analyze_cached(audio: &Path, samples: &[f32], rate: u32) -> (Analysis, Source) {
    let o = options::get();
    let ignored = match load_for(audio, &o) {
        Ok(Some(a)) => return (a, Source::Cache),
        Ok(None) => None,
        Err(e) => Some(e),
    };
    (
        analyze_fresh(audio, samples, rate, &o),
        Source::Computed { ignored },
    )
}

/// [`super::analyze_with`] `o`, stored for `audio` whatever the cache held.
pub fn analyze_fresh(audio: &Path, samples: &[f32], rate: u32, o: &Options) -> Analysis {
    let a = super::analyze_with(samples, rate, o);
    let _ = store(audio, &a, o.profile());
    a
}

/// [`analyze_cached`] for a file not decoded yet: a cache hit skips the decode entirely (bulk analysis).
pub fn get_or_analyze(audio: &Path) -> Result<(Analysis, Source), String> {
    if let Ok(Some(a)) = load_for(audio, &options::get()) {
        return Ok((a, Source::Cache));
    }
    let pcm = crate::audio::decode(audio)?;
    Ok(analyze_cached(audio, &pcm.samples, pcm.rate))
}

/// Analyses `audio` again under the current options, ignoring and replacing what the cache holds (an explicit
/// "Analyze Track").
pub fn reanalyze(audio: &Path) -> Result<Analysis, String> {
    let pcm = crate::audio::decode(audio)?;
    Ok(analyze_fresh(
        audio,
        &pcm.samples,
        pcm.rate,
        &options::get(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Analysis {
        Analysis {
            beats: vec![
                Beat {
                    bar_pos: 1,
                    bpm_x100: 12800,
                    time_ms: 10,
                },
                Beat {
                    bar_pos: 2,
                    bpm_x100: 12800,
                    time_ms: 478,
                },
            ],
            three_band_detail: vec![[1, 2, 3], [4, 5, 6], [7, 8, 9]],
            three_band_preview: vec![[10, 11, 12]],
            key: Some("8A".into()),
            ..Default::default()
        }
    }

    /// (cache dir, an audio file inside a fresh temp dir)
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("openfader-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let audio = root.join("a.wav");
        std::fs::write(&audio, b"audio").unwrap();
        (root.join("cache"), audio)
    }

    fn profile() -> Profile {
        Options::default().profile()
    }

    #[test]
    fn round_trips_and_follows_the_file() {
        let (dir, audio) = fixture("rt");
        assert!(load_from(&dir, &audio).unwrap().is_none());
        let s = Analysis {
            vocal: vec![VocalRegion {
                start_ms: 1000,
                end_ms: 9000,
            }],
            ..sample()
        };
        store_in(&dir, &audio, &s, profile()).unwrap();
        let (a, p) = load_from(&dir, &audio).unwrap().unwrap();
        assert_eq!(p, profile());
        assert_eq!(
            (
                a.beats,
                a.three_band_detail,
                a.three_band_preview,
                a.key,
                a.vocal
            ),
            (
                s.beats,
                s.three_band_detail,
                s.three_band_preview,
                s.key,
                s.vocal
            )
        );
        let none = Analysis {
            key: None,
            ..sample()
        };
        store_in(&dir, &audio, &none, profile()).unwrap();
        assert_eq!(load_from(&dir, &audio).unwrap().unwrap().0.key, None);
        // A changed file (size) is a different entry.
        std::fs::write(&audio, b"audio, edited").unwrap();
        assert!(load_from(&dir, &audio).unwrap().is_none());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_entry_answers_only_options_its_profile_covers() {
        let (dir, audio) = fixture("prof");
        let fewer = Options {
            parts: options::Parts {
                key: false,
                ..Default::default()
            },
            ..Default::default()
        };
        store_in(
            &dir,
            &audio,
            &Analysis {
                key: None,
                ..sample()
            },
            fewer.profile(),
        )
        .unwrap();
        let (_, p) = load_from(&dir, &audio).unwrap().unwrap();
        assert!(p.covers(fewer.profile()), "answers what it was made for");
        assert!(
            !p.covers(Options::default().profile()),
            "a key was never computed: the key part must run"
        );
        store_in(&dir, &audio, &sample(), profile()).unwrap();
        let (a, p) = load_from(&dir, &audio).unwrap().unwrap();
        assert!(p.covers(fewer.profile()) && a.key.is_some());
        let mut restricted = a;
        fewer.restrict(&mut restricted);
        assert_eq!(
            restricted.key, None,
            "a request without the key part gets no key"
        );
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn old_version_and_damaged_files_are_errors() {
        let (dir, audio) = fixture("bad");
        let good = encode(&sample(), profile());
        let put = |b: &[u8]| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file_name(&audio).unwrap()), b).unwrap();
            load_from(&dir, &audio)
        };
        let mut old = good.clone();
        old[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
        assert!(put(&old).unwrap_err().contains("version"));
        let mut magic = good.clone();
        magic[0] = b'X';
        assert!(put(&magic).is_err());
        for cut in [0, 3, 9, good.len() - 1] {
            assert!(put(&good[..cut]).is_err(), "truncated at {cut}");
        }
        let mut huge = good.clone();
        huge[17 + 2..17 + 6].copy_from_slice(&u32::MAX.to_le_bytes()); // beat count past the end
        assert!(put(&huge).is_err());
        assert!(put(&[good.clone(), vec![0]].concat()).is_err());
        assert!(put(&good).unwrap().is_some());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
