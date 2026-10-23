//! The watched folders: each pass lists their top-level files (a poll, one `read_dir` per folder), and every audio file or zip
//! holding audio not offered before joins INBOX. "Offered before" is [`Store::inbox_seen`] on `path|size|mtime`, so a file the
//! user moved on, removed or left alone is not offered again, while a new download under an old name is.
//!
//! A file still being written is left for a later pass: its mtime must be [`SETTLE`] old and it must not carry a browser's
//! partial-download extension. A zip with audio is unpacked into a folder of its name beside it (the audio entries only, under
//! the paths the zip gives them, never outside that folder) and each audio file in it is offered as a file.

use crate::{
    library::{folder, today},
    paths,
    store::{Role, Store, R},
    zip::Zip,
};
use std::{
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime},
};

/// How old a file's last write must be before it is taken: a download in progress keeps writing.
pub const SETTLE: Duration = Duration::from_secs(3);

/// Extensions of files a browser or downloader has not finished.
const PARTIAL: [&str; 6] = [
    "part",
    "crdownload",
    "download",
    "tmp",
    "opdownload",
    "partial",
];

/// What one pass did.
#[derive(Debug, Default, PartialEq)]
pub struct Scan {
    /// The Collection rows added to INBOX, in order.
    pub added: Vec<i64>,
    /// Files whose track the Collection (or a stage) has already: linked to it, not added.
    pub linked: usize,
    /// Zips whose audio was unpacked.
    pub unzipped: usize,
    /// Files and zips that could not be taken, with the reason.
    pub failed: Vec<String>,
}

enum Taken {
    Added(i64),
    Linked(i64),
}

fn is_zip(p: &Path) -> bool {
    paths::has_extension(p, &["zip"])
}

/// The files of `folder` to look at now, by name, each with its `path|size|mtime` signature.
fn candidates(folder: &Path, settle: Duration) -> Vec<(PathBuf, String)> {
    let now = SystemTime::now();
    let mut found: Vec<(PathBuf, String)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let (path, meta) = (e.path(), e.metadata().ok()?);
            let hidden = e.file_name().to_string_lossy().starts_with('.');
            if !meta.is_file() || meta.len() == 0 || hidden || paths::has_extension(&path, &PARTIAL)
            {
                return None;
            }
            if !(folder::is_audio(&path) || is_zip(&path)) {
                return None;
            }
            let modified = meta.modified().ok()?;
            if now.duration_since(modified).is_ok_and(|age| age < settle) {
                return None;
            }
            let secs = modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let sig = format!("{}|{}|{secs}", path.display(), meta.len());
            Some((path, sig))
        })
        .collect();
    found.sort();
    found
}

/// Unpacks the audio entries of `zip` into a new folder beside it named after it; the files made (none when the zip holds no
/// audio, and then no folder either).
fn unzip(zip: &Path) -> Result<Vec<PathBuf>, String> {
    let shown = |e| format!("{}: {e:?}", zip.display());
    let mut z = Zip::open(zip).map_err(shown)?;
    let names: Vec<String> = z
        .names()
        .filter(|n| !n.ends_with('/') && folder::is_audio(Path::new(n)))
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(bad) = names.iter().find(|n| {
        Path::new(n)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    }) {
        return Err(format!(
            "{}: entry {bad:?} leaves the folder",
            zip.display()
        ));
    }
    let parent = zip.parent().unwrap_or(Path::new("."));
    let stem = zip.file_stem().unwrap_or_default().to_string_lossy();
    let dir = paths::free_name(&parent.join(stem.as_ref()), |p| p.exists());
    let mut made = Vec::new();
    for name in names {
        let dst = dir.join(&name);
        if let Some(d) = dst.parent() {
            paths::at(std::fs::create_dir_all(d), d)?;
        }
        let mut out = paths::at(std::fs::File::create(&dst), &dst)?;
        z.extract(&name, &mut out)
            .map_err(|e| format!("{}: {name}: {e:?}", zip.display()))?;
        made.push(dst);
    }
    Ok(made)
}

/// One audio file: linked when its path or its audio is a row already, else a new row in `stage` with its tags and fingerprint.
// ponytail: the fingerprint decodes the whole file to 16 bit (a long mix is hundreds of MB for a moment); a header-only
// duplicate check would be cheaper but misses a re-encode.
fn take(store: &Store, file: &Path, stage: i64) -> R<Taken> {
    if let Some(id) = store.id_of_path(file)? {
        return Ok(Taken::Linked(id));
    }
    let (_, print) = crate::audio::decode_track16_cancellable(file, &|| false)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if print.is_empty() {
        return Err(format!("{}: no audio", file.display()));
    }
    if let Some(&id) = store.find_by_fingerprint(&print)?.first() {
        return Ok(Taken::Linked(id));
    }
    let mut t = folder::read_track(file);
    t.date_added = today();
    let id = store.collection_put_staged(&[t], stage)?[0];
    store.set_fingerprint(id, &print)?;
    Ok(Taken::Added(id))
}

/// One pass over `folders`: what is new joins INBOX. `settle` is [`SETTLE`] (a test passes zero).
pub fn scan(store: &Store, folders: &[PathBuf], settle: Duration) -> Scan {
    let mut out = Scan::default();
    let Some(inbox) = store
        .stages()
        .ok()
        .and_then(|s| s.into_iter().find(|s| s.role == Role::Inbox))
    else {
        return out;
    };
    for folder in folders {
        for (path, sig) in candidates(folder, settle) {
            if matches!(store.inbox_seen(&sig), Ok(Some(_))) {
                continue;
            }
            let zipped = is_zip(&path);
            let files = if zipped {
                unzip(&path).unwrap_or_else(|e| {
                    out.failed.push(e);
                    Vec::new()
                })
            } else {
                vec![path.clone()]
            };
            out.unzipped += usize::from(zipped && !files.is_empty());
            let mut link = None;
            for f in &files {
                match take(store, f, inbox.id) {
                    Ok(Taken::Added(id)) => out.added.push(id),
                    Ok(Taken::Linked(id)) => {
                        out.linked += 1;
                        link = Some(id);
                    }
                    Err(e) => out.failed.push(e),
                }
            }
            if let Err(e) = store.mark_inbox_seen(&sig, link.filter(|_| !zipped)) {
                out.failed.push(e);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{wavout::wav_bytes, Pcm};

    /// A second of `hz` as a WAV: another pitch, another fingerprint.
    fn tone(hz: f32) -> Vec<u8> {
        let samples = (0..8000).flat_map(|i| {
            let v = (i as f32 / 8000.0 * hz * std::f32::consts::TAU).sin() * 0.5;
            [v, v]
        });
        wav_bytes(
            &Pcm {
                rate: 8000,
                samples: samples.collect(),
            },
            8000,
        )
    }

    fn folder_for(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("openfader-inbox-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn inbox_titles(s: &Store) -> Vec<String> {
        let first = s.stages().unwrap()[0].id;
        let mut titles: Vec<String> = s
            .stage_tracks(first)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        titles.sort();
        titles
    }

    fn pass(s: &Store, dir: &Path) -> Scan {
        scan(s, &[dir.to_path_buf()], Duration::ZERO)
    }

    #[test]
    fn a_new_file_in_the_watched_folder_appears_in_inbox_once_and_only_when_it_has_settled() {
        let dir = folder_for("watch");
        let s = Store::in_memory().unwrap();
        std::fs::write(dir.join("one.wav"), tone(440.0)).unwrap();
        std::fs::write(dir.join("notes.txt"), "no music").unwrap();
        std::fs::write(dir.join("half.mp3.crdownload"), "partial").unwrap();
        assert_eq!(
            scan(&s, &[dir.clone()], SETTLE),
            Scan::default(),
            "written just now: wait for the next pass"
        );
        let found = pass(&s, &dir);
        assert_eq!(
            (found.added.len(), found.linked, found.failed.len()),
            (1, 0, 0)
        );
        assert_eq!(inbox_titles(&s), ["one"]);
        assert!(
            s.collection_tracks().unwrap().is_empty(),
            "not in the Collection yet"
        );
        assert!(s.fingerprint(found.added[0]).unwrap().is_some());
        assert_eq!(pass(&s, &dir), Scan::default(), "offered once");
        std::fs::write(dir.join("two.wav"), tone(523.0)).unwrap();
        assert_eq!(pass(&s, &dir).added.len(), 1);
        assert_eq!(inbox_titles(&s), ["one", "two"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_zip_is_unpacked_and_its_audio_is_listed() {
        let dir = folder_for("zip");
        let s = Store::in_memory().unwrap();
        let zip = crate::zip::write(&[
            ("Pack/a.wav", tone(330.0)),
            ("Pack/disc 2/b.wav", tone(392.0)),
            ("Pack/cover.jpg", vec![1, 2, 3]),
        ]);
        std::fs::write(dir.join("Pack.zip"), zip).unwrap();
        let found = pass(&s, &dir);
        assert_eq!(
            (found.unzipped, found.added.len(), found.failed.len()),
            (1, 2, 0)
        );
        assert_eq!(inbox_titles(&s), ["a", "b"]);
        assert!(
            dir.join("Pack/Pack/disc 2/b.wav").is_file()
                && !dir.join("Pack/Pack/cover.jpg").exists()
        );
        // A zip with an entry that leaves its folder is refused whole; a zip without audio is no failure.
        let evil = crate::zip::write(&[("ok.wav", tone(262.0)), ("../outside.wav", tone(294.0))]);
        std::fs::write(dir.join("Evil.zip"), evil).unwrap();
        std::fs::write(
            dir.join("Docs.zip"),
            crate::zip::write(&[("readme.txt", vec![1])]),
        )
        .unwrap();
        let found = pass(&s, &dir);
        assert_eq!((found.added.len(), found.failed.len()), (0, 1));
        assert!(!dir.join("Evil").exists() && !dir.join("outside.wav").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_duplicate_by_path_or_by_audio_is_linked_not_added() {
        let dir = folder_for("dupe");
        let s = Store::in_memory().unwrap();
        std::fs::write(dir.join("keep.wav"), tone(440.0)).unwrap();
        let had = pass(&s, &dir).added[0];
        // The same audio under another name is linked to the row that has it.
        std::fs::copy(dir.join("keep.wav"), dir.join("keep copy.wav")).unwrap();
        let again = pass(&s, &dir);
        assert_eq!((again.added.len(), again.linked), (0, 1));
        let sig = candidates(&dir, Duration::ZERO)
            .into_iter()
            .find(|(p, _)| p.ends_with("keep copy.wav"))
            .unwrap()
            .1;
        assert_eq!(s.inbox_seen(&sig).unwrap(), Some(Some(had)));
        // A file the Collection lists already (its path is a row) is linked as well.
        std::fs::write(dir.join("owned.wav"), tone(523.0)).unwrap();
        let owned = s
            .system(|| s.collection_put(&[folder::read_track(&dir.join("owned.wav"))], false))
            .unwrap();
        assert_eq!(owned, 1);
        let third = pass(&s, &dir);
        assert_eq!((third.added.len(), third.linked), (0, 1));
        assert_eq!(
            s.stage_counts().unwrap().values().sum::<usize>(),
            2,
            "no row was added"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
