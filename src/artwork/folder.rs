//! Folder images: the cover a folder of music already holds beside the files (`cover`, `folder` or `front` with a picture
//! extension, any case). Found with no network; the app shows them for tracks that have no cover of their own ([`super::resolve`]).
//! A folder is looked at once per [`FRESH`] (a browser row asks for it on every redraw).

use parking_lot::Mutex;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, Instant},
};

/// File names, best first.
const NAMES: [&str; 3] = ["cover", "folder", "front"];
const EXTS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "gif"];
/// How long a folder's answer is reused.
const FRESH: Duration = Duration::from_secs(30);
/// Folders remembered before the memory is started over.
const MAX_FOLDERS: usize = 4096;

static SEEN: LazyLock<Mutex<HashMap<PathBuf, (Instant, Option<PathBuf>)>>> =
    LazyLock::new(Mutex::default);

/// The image beside `file`, if its folder has one.
pub fn of(file: &Path) -> Option<PathBuf> {
    let dir = file.parent()?;
    if let Some((at, found)) = SEEN.lock().get(dir)
        && at.elapsed() < FRESH
    {
        return found.clone();
    }
    let found = in_dir(dir);
    let mut seen = SEEN.lock();
    if seen.len() >= MAX_FOLDERS {
        seen.clear();
    }
    seen.insert(dir.to_owned(), (Instant::now(), found.clone()));
    found
}

/// The best image in `dir`: by name (`cover` before `folder` before `front`), then by extension.
pub fn in_dir(dir: &Path) -> Option<PathBuf> {
    let rank = |p: &Path| {
        let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        Some((
            NAMES.iter().position(|n| *n == stem)?,
            EXTS.iter().position(|e| *e == ext)?,
        ))
    };
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter_map(|p| Some((rank(&p)?, p)))
        .min()
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_image_is_found_by_name_whatever_the_case_and_cover_wins() {
        let d = std::env::temp_dir().join(format!("rk-folder-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(in_dir(&d), None);
        for n in ["notes.txt", "photo.jpg", "cover.txt"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        assert_eq!(in_dir(&d), None, "only cover, folder and front count, and only pictures");
        std::fs::write(d.join("Front.PNG"), b"x").unwrap();
        assert_eq!(in_dir(&d), Some(d.join("Front.PNG")));
        std::fs::write(d.join("folder.jpg"), b"x").unwrap();
        assert_eq!(in_dir(&d), Some(d.join("folder.jpg")));
        std::fs::write(d.join("COVER.jpeg"), b"x").unwrap();
        assert_eq!(of(&d.join("song.mp3")), Some(d.join("COVER.jpeg")));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
