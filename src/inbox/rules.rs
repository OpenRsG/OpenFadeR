//! Where a track's file goes: `<root>/<stage name>/<pattern>`. The pattern is split at `/`; each part may hold `{artist}`
//! `{album}` `{title}` `{genre}` `{label}` (the track's tags; an empty artist or album reads "Unknown Artist" / "Unknown Album")
//! and `{file}` (the file's name), `{name}` (without extension), `{ext}`. The last part is the file name: it gets the file's own
//! extension when it does not end in it, and an empty one keeps the file's name. Every part is made safe as a path component.

use crate::library::Track;
use std::path::{Path, PathBuf};

/// Longest component (characters): well under every file system's limit, extension included.
const MAX: usize = 120;

/// `s` as one path component: separators and characters Windows refuses become `_`, no leading or trailing dots or spaces.
fn clean(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s: String = s
        .trim()
        .trim_matches('.')
        .trim()
        .chars()
        .take(MAX)
        .collect();
    s.trim_end().to_owned()
}

/// `part` with its `{…}` replaced from the track and its file; an unknown name is empty.
fn fill(part: &str, t: &Track, file: &Path) -> String {
    let name = file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let stem = file
        .file_stem()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let ext = file
        .extension()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let or = |s: &str, unknown: &str| {
        if s.trim().is_empty() {
            unknown.to_owned()
        } else {
            s.to_owned()
        }
    };
    let (mut out, mut rest) = (String::new(), part);
    while let Some(open) = rest.find('{') {
        out += &rest[..open];
        let Some(len) = rest[open..].find('}') else {
            rest = &rest[open..];
            break;
        };
        out += &match &rest[open + 1..open + len] {
            "artist" => or(&t.artist, "Unknown Artist"),
            "album" => or(&t.album, "Unknown Album"),
            "title" => t.title.clone(),
            "genre" => t.genre.clone(),
            "label" => t.label.clone(),
            "file" => name.clone(),
            "name" => stem.clone(),
            "ext" => ext.clone(),
            _ => String::new(),
        };
        rest = &rest[open + len + 1..];
    }
    out + rest
}

/// The file `t` would be moved to for stage `stage` (its name) under `root`: the stage's folder, then the pattern's parts.
/// Names the file's own name when the pattern gives none.
pub fn target(root: &Path, stage: &str, pattern: &str, t: &Track) -> PathBuf {
    let file = t.file.as_deref().unwrap_or(Path::new("track"));
    let mut dir = root.join(clean(stage));
    let mut parts: Vec<&str> = pattern.split(['/', '\\']).collect();
    let last = parts.pop().unwrap_or_default();
    for p in parts {
        let p = clean(&fill(p, t, file));
        if !p.is_empty() {
            dir.push(p);
        }
    }
    let mut name = clean(&fill(last, t, file));
    if let Some(ext) = file.extension().map(|e| e.to_string_lossy()) {
        if name.is_empty() {
            name = file
                .file_name()
                .map_or_else(String::new, |n| clean(&n.to_string_lossy()));
        } else if !name
            .to_lowercase()
            .ends_with(&format!(".{}", ext.to_lowercase()))
        {
            name = format!("{name}.{ext}");
        }
    }
    if name.is_empty() {
        name = "track".into();
    }
    dir.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(file: &str, artist: &str, album: &str, title: &str) -> Track {
        Track {
            file: Some(PathBuf::from(file)),
            artist: artist.into(),
            album: album.into(),
            title: title.into(),
            ..Track::default()
        }
    }

    #[test]
    fn patterns_name_artist_album_and_file_and_stay_inside_the_stage_folder() {
        let root = Path::new("/lib");
        let t = track("/dl/raw name.MP3", "AC/DC", "Back in Black", "Hells Bells");
        assert_eq!(
            target(root, "REVIEW", "{artist}/{album}/{file}", &t),
            Path::new("/lib/REVIEW/AC_DC/Back in Black/raw name.MP3")
        );
        // Renaming: no extension in the pattern, the file's own one is added; an empty tag reads Unknown.
        assert_eq!(
            target(
                root,
                "REVIEW",
                "{artist} - {title}",
                &track("/dl/x.flac", "", "", "Song")
            ),
            Path::new("/lib/REVIEW/Unknown Artist - Song.flac")
        );
        assert_eq!(
            target(root, "A:B", "{album}/", &track("/dl/x.flac", "", "", "")),
            Path::new("/lib/A_B/Unknown Album/x.flac")
        );
        // A traversal in a tag or in the pattern stays a plain name.
        let evil = track("/dl/x.mp3", "..", "../..", "t");
        assert_eq!(
            target(root, "S", "../{artist}/{album}/{title}", &evil),
            Path::new("/lib/S/_/t.mp3")
        );
    }
}
