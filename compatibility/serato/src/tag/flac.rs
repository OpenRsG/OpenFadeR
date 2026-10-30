//! FLAC: the VORBIS_COMMENT block holds `SERATO_MARKERS_V2` and `SERATO_BEATGRID` (see [`super::markers::field`]); the metadata
//! blocks (<https://xiph.org/flac/format.html#metadata_block>) are rewritten inside the space they took, with a PADDING block
//! taking up the difference so the first audio frame stays where it is.

use super::markers::{field, field_data, Tags};
use super::{Edit, Error, Stored, SLACK};
use std::fs::File;
use std::io::Read;

const PADDING: u8 = 1;
const VORBIS_COMMENT: u8 = 4;
const MARKERS: &str = "SERATO_MARKERS_V2";
const GRID: &str = "SERATO_BEATGRID";

fn le32(b: &[u8]) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize)
}

/// `(vendor, comments)` of a VORBIS_COMMENT block body.
fn comments(b: &[u8]) -> Option<(&[u8], Vec<&[u8]>)> {
    let n = le32(b)?;
    let vendor = b.get(4..4 + n)?;
    let count = le32(b.get(4 + n..)?)?;
    let (mut pos, mut out) = (8 + n, Vec::new());
    for _ in 0..count {
        let len = le32(b.get(pos..)?)?;
        out.push(b.get(pos + 4..pos + 4 + len)?);
        pos += 4 + len;
    }
    Some((vendor, out))
}

fn named<'a>(c: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let (k, v) = c.split_at(c.iter().position(|&x| x == b'=')?);
    k.eq_ignore_ascii_case(name.as_bytes()).then_some(&v[1..])
}

/// The metadata blocks `(type, body)` of the file.
fn blocks(f: &mut File) -> Result<Vec<(u8, Vec<u8>)>, Error> {
    let mut magic = [0; 4];
    f.read_exact(&mut magic)?;
    if &magic != b"fLaC" {
        return Err(Error::Unsupported);
    }
    let mut blocks: Vec<(u8, Vec<u8>)> = Vec::new();
    loop {
        let mut h = [0; 4];
        f.read_exact(&mut h)?;
        let mut body = vec![0; u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize];
        f.read_exact(&mut body)?;
        blocks.push((h[0] & 0x7f, body));
        if h[0] & 0x80 != 0 {
            break;
        }
    }
    Ok(blocks)
}

/// The Serato comments of the VORBIS_COMMENT block.
pub fn read(f: &mut File) -> Result<Stored, Error> {
    let blocks = blocks(f)?;
    let Some((_, b)) = blocks.iter().find(|(t, _)| *t == VORBIS_COMMENT) else {
        return Ok(Stored::default());
    };
    let (_, list) = comments(b).ok_or(Error::Unsupported)?;
    let get = |name| {
        list.iter()
            .find_map(|c| named(c, name))
            .and_then(|v| field_data(&String::from_utf8_lossy(v)))
    };
    Ok(Stored {
        markers2: get(MARKERS),
        grid: get(GRID),
    })
}

pub fn plan(f: &mut File, tags: &Tags, fixed: bool) -> Result<Vec<Edit>, Error> {
    let blocks = blocks(f)?;
    let region: usize = blocks.iter().map(|(_, b)| 4 + b.len()).sum();

    let old = blocks.iter().find(|(t, _)| *t == VORBIS_COMMENT);
    let parsed = match old {
        Some((_, b)) => Some(comments(b).ok_or(Error::Unsupported)?),
        None => None,
    };
    let (vendor, list) = parsed.unwrap_or((b"OpenFadeR", vec![]));
    let field_of = |name| list.iter().find_map(|c| named(c, name));
    let old_markers = field_of(MARKERS).and_then(|v| field_data(&String::from_utf8_lossy(v)));
    let markers = tags.markers2(old_markers.as_deref());
    if markers.is_none() && tags.grid.is_none() {
        return Ok(vec![]);
    }
    let mut mine = Vec::new();
    if let Some(m) = &markers {
        mine.push(format!(
            "{MARKERS}={}",
            field(super::markers::MARKERS2, m, true)
        ));
    }
    if let Some(g) = &tags.grid {
        mine.push(format!(
            "{GRID}={}",
            field(super::markers::BEATGRID, g, true)
        ));
    }
    let keep = |c: &&[u8]| {
        !(markers.is_some() && named(c, MARKERS).is_some()
            || tags.grid.is_some() && named(c, GRID).is_some())
    };
    let kept: Vec<&[u8]> = list.iter().copied().filter(keep).collect();
    let mut vorbis = (vendor.len() as u32).to_le_bytes().to_vec();
    vorbis.extend(vendor);
    vorbis.extend(((kept.len() + mine.len()) as u32).to_le_bytes());
    for c in kept.into_iter().chain(mine.iter().map(|s| s.as_bytes())) {
        vorbis.extend((c.len() as u32).to_le_bytes());
        vorbis.extend(c);
    }
    if vorbis.len() > 0xff_ffff {
        return Err(Error::NoRoom);
    }

    let mut out: Vec<(u8, &[u8])> = Vec::new();
    for (t, b) in &blocks {
        match *t {
            PADDING => {}
            VORBIS_COMMENT => out.push((*t, &vorbis)),
            _ => out.push((*t, b)),
        }
        if out.len() == 1 && old.is_none() {
            out.push((VORBIS_COMMENT, &vorbis));
        }
    }
    let used: usize = out.iter().map(|(_, b)| 4 + b.len()).sum();
    // The room left must be nothing or a PADDING block (4 bytes of header at least).
    let pad = match region.checked_sub(used) {
        Some(0) => 0,
        Some(n) if n >= 4 => n,
        _ if fixed => return Err(Error::NoRoom),
        _ => 4 + SLACK,
    };
    let padding = vec![0; pad.saturating_sub(4)];
    if pad > 0 {
        out.push((PADDING, &padding));
    }
    let mut data = Vec::with_capacity(region);
    let last = out.len() - 1;
    for (i, (t, b)) in out.into_iter().enumerate() {
        data.push(t | if i == last { 0x80 } else { 0 });
        data.extend(&(b.len() as u32).to_be_bytes()[1..]);
        data.extend(b);
    }
    Ok(vec![Edit {
        start: 4,
        end: 4 + region as u64,
        data,
    }])
}
