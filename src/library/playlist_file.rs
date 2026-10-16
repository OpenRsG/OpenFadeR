//! Playlist files: reading M3U / M3U8 / PLS into audio paths, and writing a track list as M3U8, PLS or tab-separated text.
//!
//! * M3U / M3U8 (de-facto format, no formal spec): one path or `file://` URI per line, `#…` lines (`#EXTM3U`, `#EXTINF`)
//!   are comments for us. `.m3u8` is UTF-8; plain `.m3u` is read as UTF-8 when valid, else Latin-1 (observed convention).
//! * PLS (INI style, Winamp/Shoutcast convention): `[playlist]`, `FileN=path`, entries ordered by `N`.
//! * Relative paths are taken from the playlist's folder; backslashes are read as `/`; a Windows drive path goes through
//!   `OPENFADER_PATH_MAP` like XML locations.
//! * Text export: tab-separated, one header row.

use super::{location, path_map_from_env, remap_path, PathMap, Track};
use crate::paths::at;
use std::path::{Path, PathBuf};

/// A playlist file read: its name (the file stem) and the audio paths in order.
#[derive(Debug, PartialEq)]
pub struct Parsed {
    pub name: String,
    pub files: Vec<PathBuf>,
}

/// Output format of [`write`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    M3u8,
    Pls,
    Text,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::M3u8 => "m3u8",
            Format::Pls => "pls",
            Format::Text => "txt",
        }
    }
}

fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.trim_start_matches('\u{feff}').to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// One playlist line → a path (`dir` completes a relative one).
fn resolve(entry: &str, dir: &Path, map: &PathMap) -> Option<PathBuf> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    if entry.starts_with("file://") {
        return location(entry, map);
    }
    let drive = matches!(entry.as_bytes(), [d, b':', ..] if d.is_ascii_alphabetic());
    let p = if drive || !map.is_empty() {
        remap_path(entry, map)
    } else {
        entry.replace('\\', "/")
    };
    let p = PathBuf::from(p);
    Some(if p.is_absolute() { p } else { dir.join(p) })
}

/// The paths a playlist text lists; `pls` picks the PLS reader, else M3U.
pub fn parse(text: &str, pls: bool, dir: &Path, map: &PathMap) -> Vec<PathBuf> {
    if pls {
        let mut files: Vec<(u32, &str)> = text
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once('=')?;
                let (prefix, num) = k.trim().split_at_checked(4)?;
                prefix.eq_ignore_ascii_case("file").then_some(())?;
                Some((num.parse().ok()?, v))
            })
            .collect();
        files.sort_by_key(|f| f.0);
        return files
            .into_iter()
            .filter_map(|(_, v)| resolve(v, dir, map))
            .collect();
    }
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| resolve(l, dir, map))
        .collect()
}

/// Reads an `.m3u` / `.m3u8` / `.pls` file (any other extension is tried as M3U).
pub fn read(path: &Path) -> Result<Parsed, String> {
    let bytes = at(std::fs::read(path), path)?;
    let pls = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pls"));
    let files = parse(
        &decode(&bytes),
        pls,
        path.parent().unwrap_or(Path::new("")),
        &path_map_from_env()?,
    );
    let name = path.file_stem().map_or_else(
        || "Playlist".to_owned(),
        |s| s.to_string_lossy().into_owned(),
    );
    Ok(Parsed { name, files })
}

fn one_line(s: &str) -> String {
    s.replace(['\r', '\n', '\t'], " ")
}

fn display_name(t: &Track) -> String {
    if t.artist.is_empty() {
        one_line(&t.title)
    } else {
        format!("{} - {}", one_line(&t.artist), one_line(&t.title))
    }
}

/// Track length for `#EXTINF` / `LengthN`: whole seconds, -1 when unknown.
fn extinf_seconds(t: &Track) -> i64 {
    if t.length_s > 0 {
        t.length_s
    } else {
        -1
    }
}

/// `#EXTM3U` playlist (UTF-8, absolute audio paths) of the tracks that have a file, and how many that is.
pub fn m3u8(tracks: &[Track]) -> (String, usize) {
    let mut out = String::from("#EXTM3U\n");
    let mut n = 0;
    for t in tracks {
        let Some(file) = &t.file else { continue };
        out += &format!(
            "#EXTINF:{},{}\n{}\n",
            extinf_seconds(t),
            display_name(t),
            file.display()
        );
        n += 1;
    }
    (out, n)
}

/// PLS text of the tracks that have a file.
pub fn pls(tracks: &[Track]) -> (String, usize) {
    let mut body = String::new();
    let mut n = 0;
    for t in tracks {
        let Some(file) = &t.file else { continue };
        n += 1;
        body += &format!(
            "File{n}={}\nTitle{n}={}\nLength{n}={}\n",
            file.display(),
            display_name(t),
            extinf_seconds(t)
        );
    }
    (
        format!("[playlist]\n{body}NumberOfEntries={n}\nVersion=2\n"),
        n,
    )
}

/// Tab-separated listing with a header row, every track (file or not).
pub fn text(tracks: &[Track]) -> (String, usize) {
    let mut out = String::from(
        "#\tTrack Title\tArtist\tAlbum\tGenre\tBPM\tKey\tTime\tRating\tComments\tLocation\n",
    );
    for (i, t) in tracks.iter().enumerate() {
        let (bpm, time, rating) = (t.bpm_text(), t.length_text(), t.rating.to_string());
        let file = t
            .file
            .as_ref()
            .map(|f| f.display().to_string())
            .unwrap_or_default();
        let cells = [
            &t.title, &t.artist, &t.album, &t.genre, &bpm, &t.key, &time, &rating, &t.comment,
            &file,
        ];
        out += &format!("{}\t{}\n", i + 1, cells.map(|c| one_line(c)).join("\t"));
    }
    (out, tracks.len())
}

/// Writes `tracks` to `path` in `format`; never overwrites an existing file. Returns how many tracks were written.
pub fn write(path: &Path, tracks: &[Track], format: Format) -> Result<usize, String> {
    use std::io::Write;
    let (body, n) = match format {
        Format::M3u8 => m3u8(tracks),
        Format::Pls => pls(tracks),
        Format::Text => text(tracks),
    };
    let mut f = at(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path),
        path,
    )?;
    at(f.write_all(body.as_bytes()), path)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, file: Option<&str>, length_s: i64) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            file: file.map(PathBuf::from),
            length_s,
            ..Track::default()
        }
    }

    #[test]
    fn m3u_reads_paths_uris_relative_and_windows_entries() {
        let text = "#EXTM3U\n#EXTINF:1,x\n/m/a.mp3\n\nsub\\b.mp3\nfile://localhost/m/c%20d.mp3\n  # not a path\nC:\\Music\\e.mp3\n";
        let map = parse_map("C:/Music=>/mnt/music");
        let got = parse(text, false, Path::new("/lists"), &map);
        assert_eq!(
            got,
            [
                "/m/a.mp3",
                "/lists/sub/b.mp3",
                "/m/c d.mp3",
                "/mnt/music/e.mp3"
            ]
            .map(PathBuf::from)
        );
        // Without a map a drive path stays as written, backslashes read as slashes.
        assert_eq!(
            parse("sub\\b.mp3\n", false, Path::new("/l"), &Vec::new()),
            [PathBuf::from("/l/sub/b.mp3")]
        );
    }

    fn parse_map(s: &str) -> PathMap {
        super::super::parse_path_map(s).unwrap()
    }

    #[test]
    fn pls_orders_by_entry_number() {
        let text =
            "[playlist]\nFile2=/m/b.mp3\nTitle2=B\nfile1=/m/a.mp3\nNumberOfEntries=2\nVersion=2\n";
        assert_eq!(
            parse(text, true, Path::new("/"), &Vec::new()),
            ["/m/a.mp3", "/m/b.mp3"].map(PathBuf::from)
        );
    }

    #[test]
    fn written_files_read_back_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("openfader-plf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tracks = [
            track("One", "A", Some("/m/one.mp3"), 200),
            track("No file", "B", None, 10),
            track("Two\nlines", "", Some("/m/two.flac"), 0),
        ];
        assert_eq!(
            m3u8(&tracks).0,
            "#EXTM3U\n#EXTINF:200,A - One\n/m/one.mp3\n#EXTINF:-1,Two lines\n/m/two.flac\n"
        );
        assert_eq!(m3u8(&[]), ("#EXTM3U\n".to_owned(), 0));
        for format in [Format::M3u8, Format::Pls] {
            let p = dir.join(format!("list.{}", format.extension()));
            assert_eq!(write(&p, &tracks, format).unwrap(), 2);
            assert!(write(&p, &tracks, format).is_err());
            let back = read(&p).unwrap();
            // On Windows a root without a drive takes the playlist folder's drive: compare the tails.
            assert_eq!(back.name, "list");
            assert_eq!(back.files.len(), 2);
            assert!(back.files[0].ends_with("m/one.mp3") && back.files[1].ends_with("m/two.flac"));
        }
        let (txt, n) = text(&tracks);
        assert_eq!((n, txt.lines().count()), (3, 4));
        assert!(txt.lines().nth(1).unwrap().starts_with("1\tOne\tA\t"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn latin1_m3u_and_bom_are_decoded() {
        assert_eq!(decode(b"\xef\xbb\xbf/a.mp3"), "/a.mp3");
        assert_eq!(decode(b"/caf\xe9.mp3"), "/café.mp3");
    }
}
