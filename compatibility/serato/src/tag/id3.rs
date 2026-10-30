//! ID3v2.3 / v2.4 tags (<https://id3.org/id3v2.4.0-structure>): the frames Serato's GEOB objects live in, kept byte for byte
//! except the ones we replace. Used for MP3 (a tag at the head of the file) and for the ID3 chunk of WAV and AIFF.
//!
//! The tag is rebuilt into the room of the old one: the padding after the frames takes up whatever the new frames do not.
//! A tag with the unsynchronisation flag is decoded first and written back without it; extended headers and footers are dropped
//! (a footer's room becomes padding). ID3v2.2 tags cannot be extended with GEOB frames and count as unusable.

use super::markers::{Tags, BEATGRID, MARKERS2};
use super::{Error, Stored, SLACK};

pub struct Frame {
    id: [u8; 4],
    flags: [u8; 2],
    body: Vec<u8>,
}

pub struct Tag {
    version: u8,
    frames: Vec<Frame>,
}

fn synchsafe(b: &[u8]) -> usize {
    b.iter().fold(0, |a, &x| a << 7 | usize::from(x & 0x7f))
}

fn to_synchsafe(n: usize) -> [u8; 4] {
    [
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]
}

/// Bytes the tag at the start of `h` (at least its 10 header bytes) takes, header and footer included; `None`: no tag.
pub fn total(h: &[u8]) -> Option<usize> {
    if h.len() < 10 || &h[..3] != b"ID3" {
        return None;
    }
    let footer = h[3] == 4 && h[5] & 0x10 != 0;
    Some(10 + synchsafe(&h[6..10]) + if footer { 10 } else { 0 })
}

/// The tag in `buf` (exactly [`total`] bytes); `None`: version 2.2, or frames that do not add up.
pub fn parse(buf: &[u8]) -> Option<Tag> {
    let (version, flags) = (buf[3], buf[5]);
    let end = 10 + synchsafe(&buf[6..10]);
    if !matches!(version, 3 | 4) || end > buf.len() {
        return None;
    }
    let mut body = buf[10..end].to_vec();
    if flags & 0x80 != 0 {
        let mut plain = Vec::with_capacity(body.len());
        for (i, &c) in body.iter().enumerate() {
            if !(c == 0 && i > 0 && body[i - 1] == 0xff) {
                plain.push(c);
            }
        }
        body = plain;
    }
    let mut pos = 0;
    if flags & 0x40 != 0 {
        let h = body.get(..4)?;
        pos = if version == 4 {
            synchsafe(h)
        } else {
            4 + u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize
        };
    }
    let mut frames = Vec::new();
    while let Some(h) = body.get(pos..pos + 10) {
        let id = [h[0], h[1], h[2], h[3]];
        if !id
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            break;
        }
        let size = if version == 4 {
            synchsafe(&h[4..8])
        } else {
            u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize
        };
        let data = body.get(pos + 10..pos + 10 + size)?;
        frames.push(Frame {
            id,
            flags: [h[8], h[9]],
            body: data.to_vec(),
        });
        pos += 10 + size;
    }
    Some(Tag { version, frames })
}

/// `(description, data)` of a GEOB frame body with a single-byte text encoding.
fn geob(body: &[u8]) -> Option<(&[u8], &[u8])> {
    if !matches!(body.first()?, 0 | 3) {
        return None;
    }
    let mut pos = 1;
    for _ in 0..2 {
        pos += body.get(pos..)?.iter().position(|&c| c == 0)? + 1;
    }
    let n = body.get(pos..)?.iter().position(|&c| c == 0)?;
    Some((&body[pos..pos + n], &body[pos + n + 1..]))
}

impl Tag {
    /// Data of the GEOB frame described as `desc`.
    pub fn find(&self, desc: &str) -> Option<&[u8]> {
        self.frames
            .iter()
            .filter(|f| &f.id == b"GEOB")
            .find_map(|f| {
                geob(&f.body)
                    .filter(|(d, _)| *d == desc.as_bytes())
                    .map(|(_, data)| data)
            })
    }

    /// The Serato objects of the tag.
    pub fn stored(&self) -> Stored {
        let get = |d| self.find(d).map(<[u8]>::to_vec);
        Stored {
            markers2: get(MARKERS2),
            grid: get(BEATGRID),
        }
    }
}

fn geob_frame(desc: &str, data: &[u8]) -> Frame {
    let body = [
        &[0][..],
        b"application/octet-stream\0\0",
        desc.as_bytes(),
        b"\0",
        data,
    ]
    .concat();
    Frame {
        id: *b"GEOB",
        flags: [0, 0],
        body,
    }
}

/// The tag with `tags` applied to `old` (`room`: the bytes the old tag, or the chunk holding it, takes; 0 without one), as
/// exactly `room` bytes when it fits. `fixed`: it must fit ([`Error::NoRoom`] else); otherwise the tag grows with [`SLACK`]
/// bytes of padding. `None`: there is nothing to write.
pub fn rebuild(
    old: Option<&Tag>,
    room: usize,
    tags: &Tags,
    fixed: bool,
) -> Result<Option<Vec<u8>>, Error> {
    let markers = tags.markers2(old.and_then(|t| t.find(MARKERS2)));
    if markers.is_none() && tags.grid.is_none() {
        return Ok(None);
    }
    let version = old.map_or(4, |t| t.version);
    let ours = |f: &Frame| {
        let Some((d, _)) = (&f.id == b"GEOB").then(|| geob(&f.body)).flatten() else {
            return false;
        };
        (markers.is_some() && (d == MARKERS2.as_bytes() || d == b"Serato Markers_"))
            || (tags.grid.is_some() && d == BEATGRID.as_bytes())
    };
    let (m, g);
    let mut frames: Vec<&Frame> =
        old.map_or(vec![], |t| t.frames.iter().filter(|f| !ours(f)).collect());
    if let Some(d) = &markers {
        m = geob_frame(MARKERS2, d);
        frames.push(&m);
    }
    if let Some(d) = &tags.grid {
        g = geob_frame(BEATGRID, d);
        frames.push(&g);
    }
    let mut body = Vec::new();
    for f in frames {
        body.extend(f.id);
        body.extend(if version == 4 {
            to_synchsafe(f.body.len())
        } else {
            (f.body.len() as u32).to_be_bytes()
        });
        body.extend(f.flags);
        body.extend(&f.body);
    }
    let need = 10 + body.len();
    let total = match (need <= room, fixed) {
        (true, _) => room,
        (false, true) => return Err(Error::NoRoom),
        (false, false) => need + SLACK,
    };
    let mut out = vec![b'I', b'D', b'3', version, 0, 0];
    out.extend(to_synchsafe(total - 10));
    out.extend(body);
    out.resize(total, 0);
    Ok(Some(out))
}
