//! WAV and AIFF: an ID3v2 tag inside an `id3 ` (WAV, little-endian sizes) or `ID3 ` (AIFF, big-endian sizes) chunk
//! (<https://github.com/Holzhaus/serato-tags/blob/main/docs/fileformats.md>). An existing chunk is rewritten inside its own
//! size; one that has to grow, or that is missing, is written at the end of the container (the container size is updated).

use super::{id3, read_n, Edit, Error, Stored, Tags};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// The container: byte order, declared size, end of its chunks and the `(position, length)` of the ID3 chunk when it has one.
struct Layout {
    le: bool,
    size: u32,
    end: u64,
    chunk_id: &'static [u8; 4],
    found: Option<(u64, u64)>,
}

impl Layout {
    fn int(&self, b: &[u8]) -> u32 {
        let b = [b[0], b[1], b[2], b[3]];
        if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }

    fn put(&self, n: u32) -> [u8; 4] {
        if self.le {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
    }
}

fn layout(f: &mut File) -> Result<Layout, Error> {
    let file_len = f.metadata()?.len();
    let mut h = [0; 12];
    f.read_exact(&mut h)?;
    let le = match &h[..4] {
        b"RIFF" => true,
        b"FORM" => false,
        _ => return Err(Error::Unsupported),
    };
    let mut l = Layout {
        le,
        size: 0,
        end: 0,
        chunk_id: if le { b"id3 " } else { b"ID3 " },
        found: None,
    };
    l.size = l.int(&h[4..8]);
    l.end = (8 + u64::from(l.size)).min(file_len);

    let mut pos = 12;
    while pos + 8 <= l.end {
        f.seek(SeekFrom::Start(pos))?;
        let mut c = [0; 8];
        f.read_exact(&mut c)?;
        let len = u64::from(l.int(&c[4..8]));
        if c[..4].eq_ignore_ascii_case(l.chunk_id) {
            l.found = Some((pos, len));
            break;
        }
        pos += 8 + len + (len & 1);
    }
    Ok(l)
}

/// The Serato objects of the ID3 chunk.
pub fn read(f: &mut File) -> Result<Stored, Error> {
    let Some((pos, len)) = layout(f)?.found else {
        return Ok(Stored::default());
    };
    f.seek(SeekFrom::Start(pos + 8))?;
    let body = read_n(f, len)?;
    Ok(id3::total(&body)
        .filter(|&t| t <= body.len())
        .and_then(|t| id3::parse(&body[..t]))
        .map_or_else(Stored::default, |t| t.stored()))
}

pub fn plan(f: &mut File, tags: &Tags, fixed: bool) -> Result<Vec<Edit>, Error> {
    let l = layout(f)?;
    let (size, end, chunk_id, found) = (l.size, l.end, l.chunk_id, l.found);

    let (mut room, mut old) = (0, None);
    if let Some((pos, len)) = found {
        f.seek(SeekFrom::Start(pos + 8))?;
        let body = read_n(f, len)?;
        room = len as usize;
        if body.len() != room {
            return Err(Error::Unsupported);
        }
        old = id3::total(&body)
            .filter(|&t| t <= room)
            .and_then(|t| id3::parse(&body[..t]));
        if old.is_none() && tags.blank() {
            return Ok(vec![]);
        }
        if old.is_none() && fixed {
            return Err(Error::NoRoom);
        }
    }
    let Some(tag) = id3::rebuild(old.as_ref(), room, tags, fixed)? else {
        return Ok(vec![]);
    };
    if found.is_some() && tag.len() == room {
        let start = found.map_or(0, |(p, _)| p) + 8;
        return Ok(vec![Edit {
            start,
            end: start + room as u64,
            data: tag,
        }]);
    }
    // A chunk of its own size, replacing the old one or appended at the declared end of the container.
    let mut chunk = [&chunk_id[..], &l.put(tag.len() as u32)].concat();
    chunk.extend(&tag);
    chunk.resize(chunk.len() + (tag.len() & 1), 0);
    let (start, stop) = found.map_or((end, end), |(p, len)| (p, p + 8 + len + (len & 1)));
    let new_size = u64::from(size) + chunk.len() as u64 - (stop - start);
    Ok(vec![
        Edit {
            start: 4,
            end: 8,
            data: l.put(new_size as u32).to_vec(),
        },
        Edit {
            start,
            end: stop,
            data: chunk,
        },
    ])
}
