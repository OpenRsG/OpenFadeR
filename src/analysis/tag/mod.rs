//! Writes the detected musical key into the audio file's own tag (Preferences > Analysis > Key Analysis > "Write the value
//! to the ID3 tag") and reads it back. Only called when that setting is on; nothing here runs otherwise.
//!
//! Formats (the tag each one carries the key in):
//! - MP3: ID3v2 frame `TKEY` ("Initial key", ID3v2.3 / 2.4 informal standard frame list) at the start of the file;
//! - WAV (`id3 ` chunk) and AIFF (`ID3 ` chunk, [`chunked`]): the same ID3v2 tag inside the container;
//! - FLAC: Vorbis comment `INITIALKEY` ([`flac`]), the field DJ players commonly read for FLAC.
//!
//! Anything else (AAC / M4A, whose key atom would need `moov` offsets rewritten) is refused with an error and never
//! touched. Every write rebuilds the file beside the original and renames it over it, keeps the file's permissions,
//! keeps every other frame / comment, and does nothing when the file already holds the same key. ID3v2.2, unsynchronised,
//! extended-header and footer tags are refused rather than risk damaging them.
//!
//! [`write_cover`] puts a cover picture into the same tags (an ID3v2 `APIC` frame for MP3, WAV and AIFF, a `PICTURE` block for
//! FLAC) under the same rules; the artwork feature calls it only when "Write fetched artwork into the file's tags" is on.

mod chunked;
mod flac;

use crate::paths::at;
use std::{fs, path::Path};

/// The key the file's tag holds, `None` for no tag, no key or an unsupported or unreadable file.
pub fn read_key(path: &Path) -> Option<String> {
    let b = fs::read(path).ok()?;
    match format_of(path)? {
        Format::Mp3 => key_of_tag(&parse_id3(&b).ok()??),
        Format::Riff => chunked::read(&b, true),
        Format::Aiff => chunked::read(&b, false),
        Format::Flac => flac::read(&b),
    }
}

/// Puts `key` ("8A", "Am", ...) into the file's tag; `Ok(false)` = the file already had it (not rewritten), `Err` = the file
/// is not touched (unsupported format, unusual tag, I/O error).
pub fn write_key(path: &Path, key: &str) -> Result<bool, String> {
    if key.is_empty() {
        return Err("no key to write".into());
    }
    let format = format_of(path).ok_or_else(|| {
        format!(
            "{}: this format has no key tag support (MP3, WAV, AIFF and FLAC do)",
            path.display()
        )
    })?;
    let b = at(fs::read(path), path)?;
    let new = match format {
        Format::Mp3 => {
            let tag = parse_id3(&b)?;
            if tag.as_ref().and_then(key_of_tag).as_deref() == Some(key) {
                return Ok(false);
            }
            let skip = tag.as_ref().map_or(0, |t| t.len);
            [with_key(tag, key), b[skip..].to_vec()].concat()
        }
        Format::Riff => chunked::write(&b, true, key)?,
        Format::Aiff => chunked::write(&b, false, key)?,
        Format::Flac => flac::write(&b, key)?,
    };
    let Some(new) = new_if_changed(&b, new) else {
        return Ok(false);
    };
    replace(path, &new).map(|()| true)
}

/// Puts `picture` (a JPEG or PNG file's bytes, media type `mime`) into the file's tag as its front cover, replacing the
/// front cover it had; every other frame, comment and picture stays. `Ok(false)` = the file already had exactly that, `Err` =
/// the file is not touched (a format without picture support here, an unusual tag, I/O error). MP3, WAV and AIFF carry it in
/// an ID3v2 `APIC` frame, FLAC in a `PICTURE` block ([`flac`]). Anything else (AAC / M4A) is refused.
pub fn write_cover(path: &Path, picture: &[u8], mime: &str) -> Result<bool, String> {
    let format = format_of(path).ok_or_else(|| {
        format!(
            "{}: this format has no cover support (MP3, WAV, AIFF and FLAC do)",
            path.display()
        )
    })?;
    let b = at(fs::read(path), path)?;
    let new = match format {
        Format::Mp3 => {
            let tag = parse_id3(&b)?;
            let skip = tag.as_ref().map_or(0, |t| t.len);
            [with_picture(tag, picture, mime), b[skip..].to_vec()].concat()
        }
        Format::Riff => chunked::write_with(&b, true, |tag| with_picture(tag, picture, mime))?,
        Format::Aiff => chunked::write_with(&b, false, |tag| with_picture(tag, picture, mime))?,
        Format::Flac => flac::write_picture(&b, picture, mime)?,
    };
    let Some(new) = new_if_changed(&b, new) else {
        return Ok(false);
    };
    replace(path, &new).map(|()| true)
}

fn new_if_changed(old: &[u8], new: Vec<u8>) -> Option<Vec<u8>> {
    (old != new.as_slice()).then_some(new)
}

enum Format {
    Mp3,
    Riff,
    Aiff,
    Flac,
}

fn format_of(path: &Path) -> Option<Format> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "mp3" => Some(Format::Mp3),
        "wav" => Some(Format::Riff),
        "aif" | "aiff" => Some(Format::Aiff),
        "flac" => Some(Format::Flac),
        _ => None,
    }
}

/// Writes `bytes` over `path` through a temporary file beside it, keeping the permissions.
fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    let done = fs::write(&tmp, bytes)
        .and_then(|()| fs::set_permissions(&tmp, fs::metadata(path)?.permissions()))
        .and_then(|()| fs::rename(&tmp, path));
    at(done, path).inspect_err(|_| drop(fs::remove_file(&tmp)))
}

/// One ID3v2 frame: id, flags and body, as stored.
struct Frame {
    id: [u8; 4],
    flags: [u8; 2],
    body: Vec<u8>,
}

/// An ID3v2.3 / 2.4 tag.
struct Id3 {
    version: u8,
    frames: Vec<Frame>,
    /// Bytes the tag occupied (header, frames and padding).
    len: usize,
}

fn synchsafe(b: &[u8]) -> usize {
    b.iter().fold(0, |a, &x| a << 7 | (x & 0x7f) as usize)
}

fn to_synchsafe(n: usize) -> [u8; 4] {
    [
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]
}

/// The tag at the start of `b`; `Ok(None)` = `b` does not start with one.
fn parse_id3(b: &[u8]) -> Result<Option<Id3>, String> {
    if b.len() < 10 || &b[..3] != b"ID3" {
        return Ok(None);
    }
    let (version, flags) = (b[3], b[5]);
    if !(3..=4).contains(&version) {
        return Err(format!("ID3v2.{version} tags are not supported"));
    }
    if flags & 0xd0 != 0 {
        return Err("unsynchronised, extended-header and footer ID3 tags are not supported".into());
    }
    let len = 10 + synchsafe(&b[6..10]);
    let body = b.get(10..len).ok_or("truncated ID3 tag")?;
    let (mut frames, mut at) = (Vec::new(), 0);
    while at + 10 <= body.len() && body[at] != 0 {
        let size = if version == 4 {
            synchsafe(&body[at + 4..at + 8])
        } else {
            u32::from_be_bytes(body[at + 4..at + 8].try_into().unwrap()) as usize
        };
        let end = (at + 10)
            .checked_add(size)
            .filter(|&e| e <= body.len())
            .ok_or("bad ID3 frame size")?;
        frames.push(Frame {
            id: body[at..at + 4].try_into().unwrap(),
            flags: [body[at + 8], body[at + 9]],
            body: body[at + 10..end].to_vec(),
        });
        at = end;
    }
    Ok(Some(Id3 {
        version,
        frames,
        len,
    }))
}

/// The text of a frame body: encoding byte, then the text.
fn text_of(body: &[u8]) -> Option<String> {
    let (&enc, t) = body.split_first()?;
    let s = match enc {
        0 => t.iter().map(|&c| c as char).collect(),
        3 => String::from_utf8_lossy(t).into_owned(),
        1 | 2 => {
            let (be, t) = match t {
                [0xff, 0xfe, r @ ..] => (false, r),
                [0xfe, 0xff, r @ ..] => (true, r),
                r => (enc == 2, r),
            };
            let u: Vec<u16> = t
                .chunks_exact(2)
                .map(|c| {
                    if be {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&u)
        }
        _ => return None,
    };
    Some(s.trim_end_matches('\0').to_owned())
}

fn key_of_tag(t: &Id3) -> Option<String> {
    t.frames
        .iter()
        .find(|f| &f.id == b"TKEY")
        .and_then(|f| text_of(&f.body))
        .filter(|k| !k.is_empty())
}

/// The tag bytes of `tag` (a new ID3v2.3 tag when `None`) with its `TKEY` set to `key`, every other frame kept.
fn with_key(tag: Option<Id3>, key: &str) -> Vec<u8> {
    with_frame(
        tag,
        |f| &f.id != b"TKEY",
        |version| {
            let body = if key.is_ascii() {
                [&[0u8][..], key.as_bytes()].concat()
            } else if version == 4 {
                [&[3u8][..], key.as_bytes()].concat()
            } else {
                [
                    &[1u8, 0xff, 0xfe][..],
                    &key.encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<u8>>(),
                ]
                .concat()
            };
            Frame {
                id: *b"TKEY",
                flags: [0, 0],
                body,
            }
        },
    )
}

/// `APIC` picture type "front cover" (ID3v2 informal standard, attached picture frame).
const FRONT_COVER: u8 = 3;

/// The picture type of an `APIC` body: encoding byte, media type and a 0 byte, then the type byte.
fn picture_type(body: &[u8]) -> Option<u8> {
    let nul = body.get(1..)?.iter().position(|&b| b == 0)?;
    body.get(nul + 2).copied()
}

/// The tag bytes of `tag` (a new ID3v2.3 tag when `None`) with `picture` (media type `mime`) as its front cover, every other
/// frame and picture kept.
fn with_picture(tag: Option<Id3>, picture: &[u8], mime: &str) -> Vec<u8> {
    with_frame(
        tag,
        |f| !(&f.id == b"APIC" && picture_type(&f.body) == Some(FRONT_COVER)),
        |_| Frame {
            id: *b"APIC",
            flags: [0, 0],
            // encoding 0 (ISO-8859-1), media type, picture type, empty description
            body: [&[0u8][..], mime.as_bytes(), &[0, FRONT_COVER, 0], picture].concat(),
        },
    )
}

/// The tag bytes of `tag` (a new ID3v2.3 tag when `None`): the frames `keep` accepts, then the frame `new` makes (given
/// the tag's version).
fn with_frame(
    tag: Option<Id3>,
    keep: impl Fn(&Frame) -> bool,
    new: impl FnOnce(u8) -> Frame,
) -> Vec<u8> {
    let (version, mut frames) = tag.map_or((3, Vec::new()), |t| (t.version, t.frames));
    frames.retain(keep);
    frames.push(new(version));
    let size: usize = frames.iter().map(|f| 10 + f.body.len()).sum();
    let mut out = [b"ID3".as_slice(), &[version, 0, 0], &to_synchsafe(size)].concat();
    for f in &frames {
        out.extend_from_slice(&f.id);
        out.extend_from_slice(&if version == 4 {
            to_synchsafe(f.body.len())
        } else {
            (f.body.len() as u32).to_be_bytes()
        });
        out.extend_from_slice(&f.flags);
        out.extend_from_slice(&f.body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir that is removed afterwards (never the user's music).
    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(name: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("rk-tag-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
            let p = self.0.join(name);
            fs::write(&p, bytes).unwrap();
            p
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const AUDIO: &[u8] = &[0xff, 0xfb, 0x90, 0x64, 1, 2, 3, 4, 5, 6, 7, 8];

    fn frame(id: &[u8; 4], text: &str, version: u8) -> Vec<u8> {
        let body = [&[0u8][..], text.as_bytes()].concat();
        let size = if version == 4 {
            to_synchsafe(body.len())
        } else {
            (body.len() as u32).to_be_bytes()
        };
        [id.as_slice(), &size, &[0, 0], &body].concat()
    }

    fn tag(version: u8, frames: &[Vec<u8>]) -> Vec<u8> {
        let body = frames.concat();
        [
            b"ID3".as_slice(),
            &[version, 0, 0],
            &to_synchsafe(body.len() + 6),
            &body,
            &[0; 6],
        ]
        .concat()
    }

    #[test]
    fn mp3_without_a_tag_gets_one_and_the_audio_is_untouched() {
        let d = Dir::new("mp3new");
        let p = d.file("a.mp3", AUDIO);
        assert_eq!(read_key(&p), None);
        assert_eq!(write_key(&p, "8A"), Ok(true));
        let b = fs::read(&p).unwrap();
        assert_eq!(&b[..4], b"ID3\x03");
        assert!(b.ends_with(AUDIO));
        assert_eq!(read_key(&p).as_deref(), Some("8A"));
        assert_eq!(write_key(&p, "8A"), Ok(false), "same key: not rewritten");
        assert_eq!(write_key(&p, "9B"), Ok(true));
        assert_eq!(read_key(&p).as_deref(), Some("9B"));
        assert_eq!(
            fs::read(&p)
                .unwrap()
                .windows(4)
                .filter(|w| w == b"TKEY")
                .count(),
            1
        );
    }

    #[test]
    fn an_existing_tag_keeps_its_other_frames_in_v23_and_v24() {
        let d = Dir::new("mp3old");
        for version in [3, 4] {
            let old = tag(
                version,
                &[
                    frame(b"TIT2", "Title", version),
                    frame(b"TKEY", "Dm", version),
                    frame(b"TPE1", "Artist", version),
                ],
            );
            let p = d.file(
                &format!("v{version}.mp3"),
                &[old.clone(), AUDIO.to_vec()].concat(),
            );
            assert_eq!(read_key(&p).as_deref(), Some("Dm"));
            assert_eq!(write_key(&p, "7A"), Ok(true));
            let b = fs::read(&p).unwrap();
            let t = parse_id3(&b).unwrap().unwrap();
            assert_eq!(t.version, version);
            let ids: Vec<_> = t.frames.iter().map(|f| f.id).collect();
            assert_eq!(ids, [*b"TIT2", *b"TPE1", *b"TKEY"]);
            assert_eq!(text_of(&t.frames[0].body).as_deref(), Some("Title"));
            assert_eq!(read_key(&p).as_deref(), Some("7A"));
            assert!(
                b.ends_with(AUDIO),
                "v2.{version}: audio after the tag is untouched"
            );
        }
    }

    #[test]
    fn unsupported_files_and_odd_tags_are_refused_and_left_alone() {
        let d = Dir::new("refuse");
        let m4a = d.file("a.m4a", b"....ftypM4A ");
        assert!(
            write_key(&m4a, "8A")
                .unwrap_err()
                .contains("no key tag support")
        );
        assert_eq!(fs::read(&m4a).unwrap(), b"....ftypM4A ");
        let v22 = [
            b"ID3\x02\x00\x00".as_slice(),
            &to_synchsafe(4),
            &[0; 4],
            AUDIO,
        ]
        .concat();
        let p = d.file("old.mp3", &v22);
        assert!(write_key(&p, "8A").unwrap_err().contains("v2.2"));
        assert_eq!(fs::read(&p).unwrap(), v22);
        let unsync = [
            b"ID3\x03\x00\x80".as_slice(),
            &to_synchsafe(4),
            &[0; 4],
            AUDIO,
        ]
        .concat();
        let p = d.file("unsync.mp3", &unsync);
        assert!(write_key(&p, "8A").is_err());
        assert_eq!(fs::read(&p).unwrap(), unsync);
        assert!(write_key(&d.file("e.mp3", AUDIO), "").is_err());
        assert!(write_key(&d.0.join("missing.mp3"), "8A").is_err());
    }

    #[test]
    fn wav_and_flac_files_are_written_through_a_temp_file_in_place() {
        let d = Dir::new("files");
        let wav = d.file(
            "a.WAV",
            &[
                b"RIFF\x1c\x00\x00\x00WAVE".as_slice(),
                b"data\x08\x00\x00\x00abcdefgh",
            ]
            .concat(),
        );
        let flac = d.file(
            "a.flac",
            &[b"fLaC".as_slice(), &[0x80, 0, 0, 34], &[0; 34], AUDIO].concat(),
        );
        for p in [&wav, &flac] {
            let before = fs::metadata(p).unwrap().permissions();
            assert_eq!(write_key(p, "11A"), Ok(true));
            assert_eq!(read_key(p).as_deref(), Some("11A"));
            assert_eq!(fs::metadata(p).unwrap().permissions(), before);
            assert_eq!(write_key(p, "11A"), Ok(false));
        }
        assert_eq!(
            fs::read_dir(&d.0).unwrap().count(),
            2,
            "no temp files are left"
        );
    }

    /// An `APIC` frame of the given picture type.
    fn apic(kind: u8, data: &[u8]) -> Vec<u8> {
        let body = [&[0u8][..], b"image/png", &[0, kind, 0], data].concat();
        [
            b"APIC".as_slice(),
            &(body.len() as u32).to_be_bytes(),
            &[0, 0],
            &body,
        ]
        .concat()
    }

    #[test]
    fn a_cover_replaces_only_the_front_cover_in_mp3_flac_and_wav_and_m4a_is_refused() {
        let d = Dir::new("cover");
        let new = b"\x89PNG\r\n\x1a\nNEW";
        // MP3: the front cover is replaced, the other picture and frames stay.
        let old = tag(
            3,
            &[
                frame(b"TIT2", "Title", 3),
                apic(3, b"OLD"),
                apic(4, b"BACK"),
            ],
        );
        let mp3 = d.file("a.mp3", &[old, AUDIO.to_vec()].concat());
        assert_eq!(write_cover(&mp3, new, "image/png"), Ok(true));
        let b = fs::read(&mp3).unwrap();
        let t = parse_id3(&b).unwrap().unwrap();
        let pictures: Vec<_> = t
            .frames
            .iter()
            .filter(|f| &f.id == b"APIC")
            .map(|f| {
                (
                    picture_type(&f.body),
                    f.body.ends_with(b"BACK"),
                    f.body.ends_with(b"NEW"),
                )
            })
            .collect();
        assert_eq!(pictures, [(Some(4), true, false), (Some(3), false, true)]);
        assert!(t.frames.iter().any(|f| &f.id == b"TIT2"));
        assert!(b.ends_with(AUDIO));
        assert_eq!(
            write_cover(&mp3, new, "image/png"),
            Ok(false),
            "the same cover is not written twice"
        );
        // WAV: in the ID3 chunk.
        let wav = d.file(
            "a.wav",
            &[
                b"RIFF\x1c\x00\x00\x00WAVE".as_slice(),
                b"data\x08\x00\x00\x00abcdefgh",
            ]
            .concat(),
        );
        assert_eq!(write_cover(&wav, new, "image/png"), Ok(true));
        assert!(fs::read(&wav).unwrap().windows(4).any(|w| w == b"APIC"));
        assert_eq!(write_cover(&wav, new, "image/png"), Ok(false));
        // FLAC: PICTURE blocks of other types stay, the audio stays.
        let block = |ty: u8, last: bool, data: &[u8]| {
            [
                &[ty | if last { 0x80 } else { 0 }][..],
                &(data.len() as u32).to_be_bytes()[1..],
                data,
            ]
            .concat()
        };
        let picture = |kind: u32, data: &[u8]| {
            [
                &kind.to_be_bytes()[..],
                &[0, 0, 0, 0],
                &[0, 0, 0, 0],
                &[0; 16],
                &(data.len() as u32).to_be_bytes(),
                data,
            ]
            .concat()
        };
        let flac = d.file(
            "a.flac",
            &[
                b"fLaC".to_vec(),
                block(0, false, &[0; 34]),
                block(6, false, &picture(3, b"OLD")),
                block(6, true, &picture(4, b"BACK")),
                AUDIO.to_vec(),
            ]
            .concat(),
        );
        assert_eq!(write_cover(&flac, new, "image/png"), Ok(true));
        let b = fs::read(&flac).unwrap();
        let (blocks, audio) = flac::blocks(&b).unwrap();
        let kinds: Vec<_> = blocks
            .iter()
            .map(|x| {
                (
                    x.ty,
                    b[x.data.clone()].windows(4).any(|w| w == b"BACK"),
                    b[x.data.clone()].ends_with(b"NEW"),
                )
            })
            .collect();
        assert_eq!(
            kinds,
            [(0, false, false), (6, true, false), (6, false, true)]
        );
        assert_eq!(&b[audio..], AUDIO);
        assert_eq!(write_cover(&flac, new, "image/png"), Ok(false));
        // Not touched.
        let m4a = d.file("a.m4a", b"....ftypM4A ");
        assert!(write_cover(&m4a, new, "image/png")
            .unwrap_err()
            .contains("no cover support"));
        assert_eq!(fs::read(&m4a).unwrap(), b"....ftypM4A ");
    }

    #[test]
    fn non_ascii_keys_round_trip() {
        let d = Dir::new("utf");
        for version in [3, 4] {
            let old = tag(version, &[frame(b"TIT2", "T", version)]);
            let p = d.file(&format!("u{version}.mp3"), &[old, AUDIO.to_vec()].concat());
            write_key(&p, "B♭m").unwrap();
            assert_eq!(read_key(&p).as_deref(), Some("B♭m"));
        }
    }
}
