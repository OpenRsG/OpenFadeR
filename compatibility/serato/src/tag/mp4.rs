//! M4A / MP4: freeform `----` atoms with mean `com.serato.dj`, names `markersv2` (base64 with a line feed every 72 characters)
//! and `beatgrid` (base64, no line feeds), inside `moov/udta/meta/ilst` (<https://github.com/Holzhaus/serato-tags/blob/main/docs/fileformats.md>,
//! atom layout: <https://developer.apple.com/documentation/quicktime-file-format/metadata_item_list_atom>). The decoded payload
//! is the one of FLAC ([`super::markers::field`]); Serato's `markers` atom (the old fixed-size format) is not written.
//!
//! The `ilst` is rebuilt inside the `meta` atom's size, a `free` atom taking up the difference, so `moov` and the file keep their
//! size. A `moov` that has to grow shifts the chunk offsets (`stco`/`co64`) of media behind it.

use super::markers::{field, field_data, Tags, BEATGRID, MARKERS2};
use super::{read_n, Edit, Error, Stored, SLACK};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

type Name = [u8; 4];
const MEAN: &[u8] = b"\0\0\0\0com.serato.dj";

/// `(type, start, end)` of the atoms laid end to end in `buf`.
fn atoms(buf: &[u8]) -> Vec<(Name, usize, usize)> {
    let (mut pos, mut out) = (0, Vec::new());
    while let Some(h) = buf.get(pos..pos + 8) {
        let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
        if size < 8 || pos + size > buf.len() {
            break;
        }
        out.push(([h[4], h[5], h[6], h[7]], pos, pos + size));
        pos += size;
    }
    out
}

fn make(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    v.extend(ty);
    v.extend(payload);
    v
}

/// The payload of the atom at `path` below `buf`; the children of a `meta` atom (a full atom with 4 bytes before them).
fn find<'a>(mut buf: &'a [u8], path: &[&Name]) -> Option<&'a [u8]> {
    for ty in path {
        let (_, s, e) = atoms(buf).into_iter().find(|a| a.0 == **ty)?;
        buf = &buf[s + 8..e];
        if *ty == b"meta" {
            buf = buf.get(4..)?;
        }
    }
    Some(buf)
}

/// `(name, value)` of a `----` item with our mean.
fn serato(item: &[u8]) -> Option<(String, &[u8])> {
    let kids = atoms(item);
    let get = |t: &[u8; 4]| {
        kids.iter()
            .find(|k| &k.0 == t)
            .map(|&(_, s, e)| &item[s + 8..e])
    };
    (get(b"mean")? == MEAN).then_some(())?;
    let name = String::from_utf8_lossy(get(b"name")?.get(4..)?).into_owned();
    Some((name, get(b"data")?.get(8..)?))
}

fn item(name: &str, value: &str) -> Vec<u8> {
    let name = make(b"name", &[&[0; 4][..], name.as_bytes()].concat());
    let data = make(
        b"data",
        &[&[0, 0, 0, 1, 0, 0, 0, 0][..], value.as_bytes()].concat(),
    );
    make(b"----", &[make(b"mean", MEAN), name, data].concat())
}

/// Adds `delta` to the chunk offsets that point behind `from`.
fn shift(buf: &mut [u8], from: u64, delta: u64) -> Result<(), Error> {
    for (ty, s, e) in atoms(buf) {
        match &ty {
            b"trak" | b"mdia" | b"minf" | b"stbl" => shift(&mut buf[s + 8..e], from, delta)?,
            b"stco" | b"co64" => {
                let wide = &ty == b"co64";
                let w = if wide { 8 } else { 4 };
                let n = buf
                    .get(s + 12..s + 16)
                    .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
                for i in 0..n {
                    let at = s + 16 + i * w;
                    let raw = buf.get(at..at + w).ok_or(Error::Unsupported)?;
                    let v = raw.iter().fold(0u64, |a, &b| a << 8 | u64::from(b));
                    if v >= from {
                        let v = v + delta;
                        if !wide && v > u64::from(u32::MAX) {
                            return Err(Error::Unsupported);
                        }
                        buf[at..at + w].copy_from_slice(&v.to_be_bytes()[8 - w..]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// `(position, size, bytes)` of the `moov` atom.
fn moov(f: &mut File) -> Result<(u64, u64, Vec<u8>), Error> {
    let len = f.metadata()?.len();
    let (mut pos, mut moov) = (0, None);
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos))?;
        let mut h = [0; 8];
        f.read_exact(&mut h)?;
        let size = match u32::from_be_bytes([h[0], h[1], h[2], h[3]]) {
            0 => len - pos,
            1 => return Err(Error::Unsupported),
            s => u64::from(s),
        };
        if size < 8 {
            break;
        }
        if &h[4..8] == b"moov" {
            moov = Some((pos, size));
            break;
        }
        pos += size;
    }
    let (start, size) = moov.ok_or(Error::Unsupported)?;
    f.seek(SeekFrom::Start(start))?;
    let old = read_n(f, size)?;
    if old.len() as u64 != size {
        return Err(Error::Unsupported);
    }
    Ok((start, size, old))
}

/// The Serato items of `moov/udta/meta/ilst`.
pub fn read(f: &mut File) -> Result<Stored, Error> {
    let (_, _, old) = moov(f)?;
    let items = find(&old[8..], &[b"udta", b"meta", b"ilst"]).unwrap_or_default();
    let (mut markers2, mut grid) = (None, None);
    for (_, s, e) in atoms(items) {
        match serato(&items[s + 8..e]) {
            Some((n, v)) if n == "markersv2" => {
                markers2 = field_data(&String::from_utf8_lossy(v));
            }
            Some((n, v)) if n == "beatgrid" => grid = field_data(&String::from_utf8_lossy(v)),
            _ => {}
        }
    }
    Ok(Stored { markers2, grid })
}

pub fn plan(f: &mut File, tags: &Tags, fixed: bool) -> Result<Vec<Edit>, Error> {
    let (start, size, old) = moov(f)?;
    let payload = &old[8..];

    let items = find(payload, &[b"udta", b"meta", b"ilst"]).unwrap_or_default();
    let ours = |a: &(Name, usize, usize)| serato(&items[a.1 + 8..a.2]);
    let old_markers = atoms(items)
        .iter()
        .filter_map(ours)
        .find(|(n, _)| n == "markersv2")
        .and_then(|(_, v)| field_data(&String::from_utf8_lossy(v)));
    let markers = tags.markers2(old_markers.as_deref());
    if markers.is_none() && tags.grid.is_none() {
        return Ok(vec![]);
    }
    let mut ilst = Vec::new();
    for a in atoms(items) {
        let replaced = ours(&a).is_some_and(|(n, _)| {
            n == "markersv2" && markers.is_some() || n == "beatgrid" && tags.grid.is_some()
        });
        if !replaced {
            ilst.extend(&items[a.1..a.2]);
        }
    }
    if let Some(m) = &markers {
        ilst.extend(item("markersv2", &field(MARKERS2, m, true)));
    }
    if let Some(g) = &tags.grid {
        ilst.extend(item("beatgrid", &field(BEATGRID, g, false)));
    }
    let ilst = make(b"ilst", &ilst);

    let hdlr = make(b"hdlr", &[&[0; 8][..], b"mdirappl", &[0; 9]].concat());
    let rebuild = |kids: Option<&[u8]>| -> Result<Vec<u8>, Error> {
        if kids.is_none() && fixed {
            return Err(Error::NoRoom);
        }
        let kids = kids.unwrap_or_default();
        let (mut out, mut placed) = (Vec::new(), false);
        if !atoms(kids).iter().any(|a| &a.0 == b"hdlr") {
            out.extend(&hdlr);
        }
        for (ty, s, e) in atoms(kids) {
            match &ty {
                b"ilst" => {
                    out.extend(&ilst);
                    placed = true;
                }
                b"free" | b"skip" => {}
                _ => out.extend(&kids[s..e]),
            }
        }
        if !placed {
            out.extend(&ilst);
        }
        match kids.len().checked_sub(out.len()) {
            Some(0) => {}
            Some(n) if n >= 8 => out.extend(make(b"free", &vec![0; n - 8])),
            _ if fixed => return Err(Error::NoRoom),
            _ => out.extend(make(b"free", &vec![0; SLACK])),
        }
        Ok(out)
    };
    let new_payload = edit(payload, &[b"udta", b"meta"], &rebuild)?;
    let mut new = make(b"moov", &new_payload);
    if new.len() as u64 != size {
        if fixed {
            return Err(Error::NoRoom);
        }
        let delta = new.len() as u64 - size;
        shift(&mut new[8..], start + size, delta)?;
    }
    Ok(vec![Edit {
        start,
        end: start + size,
        data: new,
    }])
}

/// `buf` with the atom at `path` rebuilt by `f` (from the children of the last atom, `None` when it does not exist and is
/// created); missing atoms on the way are created.
fn edit(
    buf: &[u8],
    path: &[&Name],
    f: &dyn Fn(Option<&[u8]>) -> Result<Vec<u8>, Error>,
) -> Result<Vec<u8>, Error> {
    let (ty, rest) = path.split_first().ok_or(Error::Unsupported)?;
    let hit = atoms(buf).into_iter().find(|a| a.0 == **ty);
    let skip = if *ty == b"meta" { 4 } else { 0 };
    let body = hit
        .map(|(_, s, e)| &buf[s + 8..e])
        .filter(|b| b.len() >= skip);
    let (prefix, kids) = match body {
        Some(b) => (b[..skip].to_vec(), Some(&b[skip..])),
        None => (vec![0; skip], None),
    };
    let new = if rest.is_empty() {
        f(kids)?
    } else {
        edit(kids.unwrap_or_default(), rest, f)?
    };
    let atom = make(ty, &[prefix, new].concat());
    Ok(match hit {
        Some((_, s, e)) => [&buf[..s], &atom, &buf[e..]].concat(),
        None => [buf, &atom].concat(),
    })
}
