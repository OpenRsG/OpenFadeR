//! Persistent store of our own [`analyze`] results (beats, key, 3-band waveforms) so a track is analysed once. This is user
//! data, not a cache: `<data folder>/analysis/<hash of the track's identity>.bin` (the data folder is
//! [`crate::paths::data_dir`]), named by [`crate::store::key`] (`t<id>` for a Collection track), so the entry follows a
//! track whose file moved or was renamed. The audio file's size is in the header: another size is a changed file and the
//! entry is stale.
//!
//! Own format, little-endian: `"RKAC"`, `version u32`, `audio size u64`, `profile u64` (what the result was
//! made with, [`Profile`]), `key_len u8` + key text (0 = none), then counted tables: beats (`bar_pos u16, bpm_x100 u16,
//! time_ms u32`), 3-band detail and 3-band preview (`[u8; 3]` each) and vocal regions (`start_ms u32, end_ms u32`), each
//! with its `count u32` first. A wrong magic or version, a short or over-long file is an error the caller reports and
//! recomputes over; it is never trusted. A result whose profile does not cover the current options is recomputed.
//!
//! Older results lived in `<cache folder>/analysis/<hash of path + size + mtime>.bin` (version 3, no size field);
//! [`migrate`] moves those of the Collection's tracks here once.

use super::{
    Analysis, Beat,
    detect::VocalRegion,
    options::{self, Options, Profile},
};
use crate::{audio::Frames, library::Track, paths::at, store};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"RKAC";
/// Bump when the layout or the detectors change enough that old results should be recomputed (5: bar phase from accent,
/// novelty and start, see `detect::grid`).
const VERSION: u32 = 5;
/// The layout before the audio size was stored: [`migrate`] upgrades these.
const VERSION_BEFORE_SIZE: u32 = 3;

/// [`crate::paths::cache_dir`]: the parent of every cache folder (stems, models, folder tags).
pub fn root() -> Option<PathBuf> {
    crate::paths::cache_dir()
}

/// `<data folder>/analysis`: where our analysis results live.
pub fn dir() -> Option<PathBuf> {
    Some(crate::paths::data_dir()?.join("analysis"))
}

/// `<cache folder>/analysis`: where they lived before they became data (see [`migrate`]).
pub fn legacy_dir() -> Option<PathBuf> {
    Some(root()?.join("analysis"))
}

/// Key of an audio file by its path, size and mtime: FNV-1a 64 as 16 hex digits (stable across Rust versions, unlike
/// `DefaultHasher`). The name of an old analysis entry and what the stems cache is keyed by.
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

/// The audio file of `t`, which an analysis is of.
fn audio_of(t: &Track) -> Result<&Path, String> {
    t.file
        .as_deref()
        .ok_or_else(|| format!("{}: the track has no audio file", t.title))
}

/// File name of `t`'s entry: the hash of its [`store::key`].
fn file_name(t: &Track) -> String {
    format!("{:016x}.bin", fnv1a(store::key(t).bytes()))
}

/// Size of `t`'s audio file: what a stored entry must match.
fn audio_size(t: &Track) -> Result<u64, String> {
    let audio = audio_of(t)?;
    Ok(at(std::fs::metadata(audio), audio)?.len())
}

fn encode(a: &Analysis, profile: Profile, size: u64) -> Vec<u8> {
    let key = a.key.as_deref().unwrap_or("").as_bytes();
    let key = &key[..key.len().min(255)];
    let mut o = Vec::with_capacity(
        48 + a.beats.len() * 8
            + (a.three_band_detail.len() + a.three_band_preview.len()) * 3
            + a.vocal.len() * 8,
    );
    o.extend_from_slice(MAGIC);
    o.extend_from_slice(&VERSION.to_le_bytes());
    o.extend_from_slice(&size.to_le_bytes());
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
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// `count u32` then `count` records of `size` bytes; the count is checked against what is left before allocating.
    fn table(&mut self, size: usize) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n.checked_mul(size).ok_or("bad count")?)
    }
}

/// The analysis, the profile it was made with and the audio size it was made for.
fn decode(b: &[u8]) -> Result<(Analysis, Profile, u64), String> {
    let mut c = Cur(b);
    if c.take(4)? != MAGIC {
        return Err("not an analysis cache file".into());
    }
    let v = c.u32()?;
    if v != VERSION {
        return Err(format!("cache version {v}, expected {VERSION}"));
    }
    let size = c.u64()?;
    let profile = Profile(c.u64()?);
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
        size,
    ))
}

/// `Ok(None)`: no entry, or one made for a file of another size (the audio changed).
fn load_from(dir: &Path, t: &Track) -> Result<Option<(Analysis, Profile)>, String> {
    let size = audio_size(t)?;
    match std::fs::read(dir.join(file_name(t))) {
        Ok(b) => decode(&b)
            .map(|(a, p, s)| (s == size).then_some((a, p)))
            .map_err(|e| format!("analysis cache ignored ({e})")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("analysis cache ignored ({e})")),
    }
}

/// Writes `bytes` as `name` in `dir` (created): beside the target and renamed, so a concurrent reader (deck load vs bulk
/// worker) never sees half a file.
fn put(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    at(std::fs::create_dir_all(dir), dir)?;
    let tmp = dir.join(format!(
        "{name}.{}.{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, dir.join(name)))
        .map_err(|e| format!("analysis cache write: {e}"))
}

fn store_in(dir: &Path, t: &Track, a: &Analysis, profile: Profile) -> Result<(), String> {
    put(dir, &file_name(t), &encode(a, profile, audio_size(t)?))
}

fn data_dir() -> Result<PathBuf, String> {
    dir().ok_or_else(|| "no data directory (set XDG_DATA_HOME or HOME)".to_owned())
}

/// The stored analysis of `t` as it was saved, whatever options it was made with (what a track list shows):
/// `Ok(None)` = not stored (or the audio file changed since); `Err` = the file is corrupt, old or unreadable and is
/// ignored (report it, then recompute and [`store`] over it).
pub fn load(t: &Track) -> Result<Option<Analysis>, String> {
    Ok(load_from(&data_dir()?, t)?.map(|x| x.0))
}

/// The vocal regions the analysis cache holds for `t` ([`super::detect::vocal_regions`], an inferred heuristic);
/// empty when the track was not analysed or the Vocal part was off. What VIEW > Layout > Vocal draws.
pub fn vocal(t: &Track) -> Vec<VocalRegion> {
    load(t).ok().flatten().map_or_else(Vec::new, |a| a.vocal)
}

/// The stored analysis of `t` when it covers `o` (every part `o` runs, the same grid settings), with the parts `o`
/// switches off emptied; `Ok(None)` = not stored or made with other options.
pub fn load_for(t: &Track, o: &Options) -> Result<Option<Analysis>, String> {
    Ok(load_from(&data_dir()?, t)?
        .filter(|x| x.1.covers(o.profile()))
        .map(|mut x| {
            x.0.phrases = super::phrases_for(o, &x.0.beats, &x.0.three_band_detail);
            o.restrict(&mut x.0);
            x.0
        }))
}

/// Saves `a`, made with `profile`, for `t`, replacing any old entry.
pub fn store(t: &Track, a: &Analysis, profile: Profile) -> Result<(), String> {
    store_in(&data_dir()?, t, a, profile)
}

/// Where an analysis came from.
pub enum Source {
    Cache,
    /// Freshly computed (and stored); `ignored` says why an existing but unusable cache file was not trusted.
    Computed {
        ignored: Option<String>,
    },
}

/// Stored analysis under the current [`options`], else [`super::analyze`] of the decoded `samples` + [`store`].
/// ponytail: a failed cache write is dropped silently; the result is still returned.
pub fn analyze_cached<S: Frames + ?Sized>(t: &Track, samples: &S, rate: u32) -> (Analysis, Source) {
    analyze_cached_cancellable(t, samples, rate, &|| false).unwrap()
}

/// Like [`analyze_cached`], but abandoned loads cannot publish partial or stale results.
pub fn analyze_cached_cancellable<S: Frames + ?Sized>(
    t: &Track,
    samples: &S,
    rate: u32,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<(Analysis, Source)> {
    let o = options::get();
    analyze_pcm(
        t,
        samples,
        rate,
        &o,
        false,
        super::Execution::Interactive,
        cancelled,
    )
}

fn analyze_pcm<S: Frames + ?Sized>(
    t: &Track,
    samples: &S,
    rate: u32,
    o: &Options,
    explicit: bool,
    execution: super::Execution,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<(Analysis, Source)> {
    if cancelled() {
        return None;
    }
    let ignored = if explicit {
        None
    } else {
        match load_for(t, o) {
            Ok(Some(a)) => return (!cancelled()).then_some((a, Source::Cache)),
            Ok(None) => None,
            Err(e) => Some(e),
        }
    };
    let a = super::analyze_cancellable(samples, rate, o, execution, cancelled)?;
    if cancelled() {
        return None;
    }
    let _ = store(t, &a, o.profile());
    Some((a, Source::Computed { ignored }))
}

/// Bulk work uses its captured options and worker, never nested detector threads.
pub fn get_or_analyze_with(
    t: &Track,
    o: &Options,
    explicit: bool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Option<(Analysis, Source)>, String> {
    if cancelled() {
        return Ok(None);
    }
    if !explicit {
        if let Ok(Some(a)) = load_for(t, o) {
            return Ok((!cancelled()).then_some((a, Source::Cache)));
        }
    }
    let (track, print) = crate::audio::decode_track16_cancellable(audio_of(t)?, cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    remember_fingerprint(t, &print);
    Ok(analyze_pcm(
        t,
        &track,
        track.rate,
        o,
        explicit,
        super::Execution::Bulk,
        cancelled,
    ))
}

/// [`super::analyze_with`] `o`, stored for `t` whatever the cache held.
pub fn analyze_fresh<S: Frames + ?Sized>(
    t: &Track,
    samples: &S,
    rate: u32,
    o: &Options,
) -> Analysis {
    let a = super::analyze_with(samples, rate, o);
    let _ = store(t, &a, o.profile());
    a
}

/// [`analyze_cached`] for a file not decoded yet: a cache hit skips the decode entirely (bulk analysis).
pub fn get_or_analyze(t: &Track) -> Result<(Analysis, Source), String> {
    if let Ok(Some(a)) = load_for(t, &options::get()) {
        return Ok((a, Source::Cache));
    }
    let (track, print) = crate::audio::decode_track16_cancellable(audio_of(t)?, &|| false)?;
    remember_fingerprint(t, &print);
    Ok(analyze_cached(t, &track, track.rate))
}

/// Analyses `t` again under the current options, ignoring and replacing what the cache holds (an explicit
/// "Analyze Track").
pub fn reanalyze(t: &Track) -> Result<Analysis, String> {
    let (track, print) = crate::audio::decode_track16_cancellable(audio_of(t)?, &|| false)?;
    remember_fingerprint(t, &print);
    Ok(analyze_fresh(t, &track, track.rate, &options::get()))
}

/// Gives a Collection track the fingerprint `print` of its decoded audio ([`super::fingerprint`], empty for no audio) when it
/// has none yet: what finds the file again after it moved. Tracks outside the Collection (no id) and failed writes are
/// skipped silently.
pub fn remember_fingerprint(t: &Track, print: &str) {
    let Some(id) = t.id else { return };
    let store = store::global();
    if store.fingerprint(id).ok().flatten().is_some() {
        return;
    }
    if !print.is_empty() {
        let _ = store.set_fingerprint(id, print);
    }
}

/// Preferences > Analysis "Beat grid and cues from OpenFadeR analysis": drops the library's grid, phrases and cues from `a`
/// and puts `ours` (the stored analysis, `None` = not analysed yet) in their place. With no analysis yet the grid stays empty
/// so the analysis a LOAD runs fills it. The library's waveform and key stay where `ours` has none; the caller puts the
/// user's own edits back on top ([`crate::store::apply`]).
pub fn take_ours(a: &mut Analysis, ours: Option<Analysis>) {
    a.beats.clear();
    a.cues.clear();
    a.phrases = None;
    let Some(o) = ours else { return };
    a.beats = o.beats;
    a.phrases = o.phrases;
    if a.three_band_detail.is_empty() {
        (a.three_band_detail, a.three_band_preview) = (o.three_band_detail, o.three_band_preview);
    }
    if a.key.is_none() {
        a.key = o.key;
    }
}

/// What [`migrate`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migrated {
    /// Entries moved into [`dir`].
    pub moved: usize,
    /// Entries still in [`legacy_dir`] (tracks that are not in the Collection, files that are gone).
    pub left: usize,
}

/// `old` (a version 3 entry) as the current layout for an audio file of `size`; `None` when it is not a version 3 file.
/// The old detector made its beats, so the profile is emptied: every part is recomputed when asked for, like any entry made
/// by an older detector.
fn upgrade(old: &[u8], size: u64) -> Option<Vec<u8>> {
    let (head, rest) = old.split_at_checked(8)?;
    if &head[..4] != MAGIC || head[4..] != VERSION_BEFORE_SIZE.to_le_bytes() {
        return None;
    }
    Some(
        [
            MAGIC.as_slice(),
            &VERSION.to_le_bytes(),
            &size.to_le_bytes(),
            &Profile(0).0.to_le_bytes(),
            rest.get(8..)?,
        ]
        .concat(),
    )
}

/// One-time move of the old per-path entries of `tracks` (the Collection) into [`dir`] under their identity names; the old
/// folder is removed when it ends up empty, else left alone. Slow for a big library (a stat per track): run it off the UI thread.
pub fn migrate(tracks: &[Track]) -> Migrated {
    let (Some(old), Some(new)) = (legacy_dir(), dir()) else {
        return Migrated::default();
    };
    let Ok(entries) = std::fs::read_dir(&old) else {
        return Migrated::default();
    };
    let mut left = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "bin"))
        .count();
    let mut moved = 0;
    for t in tracks {
        if left == 0 {
            break;
        }
        let Some(audio) = t.file.as_deref() else {
            continue;
        };
        let Ok(k) = key(audio) else { continue };
        let from = old.join(format!("{k}.bin"));
        let Ok(bytes) = std::fs::read(&from) else {
            continue;
        };
        let name = file_name(t);
        let size = std::fs::metadata(audio).map_or(0, |m| m.len());
        // A newer entry already there (analysed since the update) wins; the old one is dropped either way.
        let fresh = !new.join(&name).exists();
        let ok = match upgrade(&bytes, size).filter(|u| decode(u).is_ok()) {
            Some(u) => !fresh || put(&new, &name, &u).is_ok(),
            None => false,
        };
        if ok && std::fs::remove_file(&from).is_ok() {
            moved += 1;
            left -= 1;
        }
    }
    if left == 0 {
        let _ = std::fs::remove_dir(&old);
    } else {
        eprintln!(
            "analysis: {left} old entries stay in {} (no Collection track owns them)",
            old.display()
        );
    }
    Migrated { moved, left }
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

    /// (cache dir, a track with an audio file inside a fresh temp dir, its id fixed so a move keeps the identity)
    fn fixture(name: &str) -> (PathBuf, Track) {
        let root =
            std::env::temp_dir().join(format!("openfader-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let audio = root.join("a.wav");
        std::fs::write(&audio, b"audio").unwrap();
        let t = Track {
            id: Some(7),
            file: Some(audio),
            ..Track::default()
        };
        (root.join("cache"), t)
    }

    fn profile() -> Profile {
        Options::default().profile()
    }

    #[test]
    fn round_trips_and_follows_the_track_not_the_path() {
        let (dir, t) = fixture("rt");
        assert!(load_from(&dir, &t).unwrap().is_none());
        let s = Analysis {
            vocal: vec![VocalRegion {
                start_ms: 1000,
                end_ms: 9000,
            }],
            ..sample()
        };
        store_in(&dir, &t, &s, profile()).unwrap();
        let (a, p) = load_from(&dir, &t).unwrap().unwrap();
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
        store_in(&dir, &t, &none, profile()).unwrap();
        assert_eq!(load_from(&dir, &t).unwrap().unwrap().0.key, None);

        // The same track (id) after its file moved and was renamed: still found.
        let moved = t.file.as_ref().unwrap().with_file_name("renamed.wav");
        std::fs::rename(t.file.as_ref().unwrap(), &moved).unwrap();
        let t2 = Track {
            file: Some(moved),
            ..t.clone()
        };
        assert!(load_from(&dir, &t2).unwrap().is_some());
        // Another track is another entry.
        let other = Track {
            id: Some(8),
            ..t2.clone()
        };
        assert!(load_from(&dir, &other).unwrap().is_none());
        // A changed file (size) makes the entry stale, not an error.
        std::fs::write(t2.file.as_ref().unwrap(), b"audio, edited").unwrap();
        assert!(load_from(&dir, &t2).unwrap().is_none());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_entry_answers_only_options_its_profile_covers() {
        let (dir, t) = fixture("prof");
        let fewer = Options {
            parts: options::Parts {
                key: false,
                ..Default::default()
            },
            ..Default::default()
        };
        store_in(
            &dir,
            &t,
            &Analysis {
                key: None,
                ..sample()
            },
            fewer.profile(),
        )
        .unwrap();
        let (_, p) = load_from(&dir, &t).unwrap().unwrap();
        assert!(p.covers(fewer.profile()), "answers what it was made for");
        assert!(
            !p.covers(Options::default().profile()),
            "a key was never computed: the key part must run"
        );
        store_in(&dir, &t, &sample(), profile()).unwrap();
        let (a, p) = load_from(&dir, &t).unwrap().unwrap();
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
    fn cancelled_analysis_never_requires_an_audio_file_or_returns_a_fallback() {
        let track = Track::default();
        assert!(analyze_cached_cancellable(&track, &[0.0; 64], 8000, &|| true).is_none());
        assert!(
            analyze_pcm(
                &track,
                &[0.0; 64],
                8000,
                &Options::default(),
                true,
                super::super::Execution::Bulk,
                &|| true,
            )
            .is_none()
        );
    }

    #[test]
    fn old_version_and_damaged_files_are_errors() {
        let (dir, t) = fixture("bad");
        let good = encode(&sample(), profile(), 5);
        let put = |b: &[u8]| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file_name(&t)), b).unwrap();
            load_from(&dir, &t)
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
        huge[25 + 2..25 + 6].copy_from_slice(&u32::MAX.to_le_bytes()); // beat count past the end
        assert!(put(&huge).is_err());
        assert!(put(&[good.clone(), vec![0]].concat()).is_err());
        assert!(put(&good).unwrap().is_some());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// A version 3 entry as the old code wrote it: the current layout without the audio size.
    fn old_entry(a: &Analysis) -> Vec<u8> {
        let mut b = encode(a, profile(), 0);
        b[4..8].copy_from_slice(&VERSION_BEFORE_SIZE.to_le_bytes());
        b.drain(8..16);
        b
    }

    #[test]
    fn upgrade_turns_a_version_3_entry_into_a_valid_stale_one() {
        let up = upgrade(&old_entry(&sample()), 5).unwrap();
        let (a, p, size) = decode(&up).unwrap();
        assert_eq!((a.beats, size), (sample().beats, 5));
        assert!(!p.covers(profile()), "its beats come from the old detector");
        assert!(
            upgrade(&encode(&sample(), profile(), 5), 5).is_none(),
            "a current entry is not an old one"
        );
        assert!(upgrade(b"RKA", 5).is_none());
    }

    #[test]
    fn our_analysis_replaces_the_librarys_grid_and_cues_but_not_its_waveform() {
        let cue = crate::analysis::Cue {
            hot_cue: 0,
            time_ms: 5,
            loop_end_ms: None,
            color: None,
            comment: None,
        };
        let theirs = || Analysis {
            beats: vec![Beat {
                bar_pos: 1,
                bpm_x100: 9000,
                time_ms: 1,
            }],
            cues: vec![cue.clone()],
            three_band_detail: vec![[9, 9, 9]],
            ..Default::default()
        };
        let mut a = theirs();
        take_ours(&mut a, Some(sample()));
        assert_eq!(a.beats, sample().beats);
        assert!(a.cues.is_empty());
        assert_eq!(a.three_band_detail, [[9, 9, 9]], "their waveform stays");
        assert_eq!(a.key.as_deref(), Some("8A"), "a key they lack is ours");
        // Not analysed yet: nothing of theirs is left to mislead the deck, the load analyses.
        let mut a = theirs();
        take_ours(&mut a, None);
        assert!(a.beats.is_empty() && a.cues.is_empty() && a.phrases.is_none());
    }
}
