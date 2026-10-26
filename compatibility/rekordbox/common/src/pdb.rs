//! Byte readers of the Device Library `export.pdb` (DeviceSQL).
//!
//! Layout inferred from the public Deep Symmetry crate-digger spec
//! (`rekordbox_pdb.ksy`, https://github.com/Deep-Symmetry/crate-digger,
//! commit 7c9d5358d8a3ce663f566380f2dc79ab1393201e); all integers little-endian.

/// Page header size; row offsets are relative to the end of it.
pub const HEAP: usize = 0x28;

pub fn u16_at(d: &[u8], p: usize) -> Result<u16, String> {
    let b = d.get(p..p.wrapping_add(2)).ok_or("pdb: truncated u16")?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

pub fn u32_at(d: &[u8], p: usize) -> Result<u32, String> {
    let b = d.get(p..p.wrapping_add(4)).ok_or("pdb: truncated u32")?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// DeviceSQL string at `p`: short ASCII (odd length byte), 0x40 long ASCII, 0x90 UTF-16LE.
pub fn string_at(d: &[u8], p: usize) -> Result<String, String> {
    let kind = *d.get(p).ok_or("pdb: truncated string")?;
    let (start, len, utf16) = match kind {
        0x40 | 0x90 => {
            let n = (u16_at(d, p + 1)? as usize)
                .checked_sub(4)
                .ok_or("pdb: bad string length")?;
            (p + 4, n, kind == 0x90)
        }
        k if k & 1 == 1 => (p + 1, ((k >> 1) as usize).saturating_sub(1), false),
        _ => return Err("pdb: unknown string kind".into()),
    };
    let b = d
        .get(start..start.wrapping_add(len))
        .ok_or("pdb: truncated string body")?;
    if utf16 {
        if len % 2 != 0 {
            return Err("pdb: odd UTF-16 length".into());
        }
        let u: Vec<u16> = b
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Ok(String::from_utf16_lossy(&u))
    } else {
        Ok(String::from_utf8_lossy(b).into_owned())
    }
}

/// Absolute offsets of every present row of the first table of type `ty` (none if absent).
pub fn rows(d: &[u8], ty: u32) -> Result<Vec<usize>, String> {
    let len = u32_at(d, 4)? as usize;
    let ntables = u32_at(d, 8)? as usize;
    if len < HEAP + 6 {
        return Err("pdb: bad page size".into());
    }
    let mut out = Vec::new();
    for t in 0..ntables {
        let e = 28 + 16 * t;
        if u32_at(d, e)? != ty {
            continue;
        }
        let (mut page, last) = (u32_at(d, e + 8)? as usize, u32_at(d, e + 12)? as usize);
        // Chain ends at `last`; a bounded step count also stops cycles.
        for _ in 0..=d.len() / len {
            let start = page.checked_mul(len).ok_or("pdb: page index overflow")?;
            let pg = d
                .get(start..start.wrapping_add(len))
                .ok_or("pdb: page out of file")?;
            if u32_at(pg, 8)? != ty {
                break;
            }
            let flags = pg[0x1b];
            if flags & 0x40 == 0 {
                let n = (u32_at(pg, 0x18)? & 0x1fff) as usize;
                for i in 0..n {
                    let base = len
                        .checked_sub((i / 16) * 0x24)
                        .ok_or("pdb: row group out of page")?;
                    let present =
                        u16_at(pg, base.checked_sub(4).ok_or("pdb: row group out of page")?)?;
                    if present >> (i % 16) & 1 == 0 {
                        continue;
                    }
                    let o = base
                        .checked_sub(6 + 2 * (i % 16))
                        .ok_or("pdb: row index out of page")?;
                    let ofs = HEAP + u16_at(pg, o)? as usize;
                    if ofs >= len {
                        return Err("pdb: row offset out of page".into());
                    }
                    out.push(start + ofs);
                }
            }
            if page == last {
                break;
            }
            page = u32_at(pg, 0x0c)? as usize;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings() {
        assert_eq!(string_at(&[0x09, b'a', b'b', b'c'], 0).unwrap(), "abc");
        assert_eq!(string_at(&[0x03], 0).unwrap(), "");
        assert_eq!(
            string_at(&[0x40, 7, 0, 0, b'x', b'y', b'z'], 0).unwrap(),
            "xyz"
        );
        assert_eq!(
            string_at(&[0x90, 8, 0, 0, b'h', 0, 0xe9, 0], 0).unwrap(),
            "hé"
        );
        assert!(string_at(&[0x07, b'a'], 0).is_err()); // truncated
        assert!(string_at(&[0x40, 9, 0, 0, b'x'], 0).is_err());
        assert!(string_at(&[0x40, 2, 0, 0], 0).is_err()); // length < header
        assert!(string_at(&[0x90, 7, 0, 0, 1, 2, 3], 0).is_err()); // odd UTF-16
        assert!(string_at(&[0x02], 0).is_err()); // even, not a known kind
    }

    /// 512-byte pages: 0 = header, 1 = empty first page, 2 = data page with two rows of a type-2 table
    /// (the second marked absent in the presence mask).
    fn synthetic() -> Vec<u8> {
        const L: usize = 512;
        let mut d = vec![0u8; 3 * L];
        let w32 = |d: &mut Vec<u8>, p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
        w32(&mut d, 4, L as u32);
        w32(&mut d, 8, 1);
        for (i, v) in [2, 0, 1, 2].into_iter().enumerate() {
            w32(&mut d, 28 + 4 * i, v);
        }
        for (page, next) in [(1, 2), (2, 3)] {
            let p = page * L;
            w32(&mut d, p + 4, page as u32);
            w32(&mut d, p + 8, 2);
            w32(&mut d, p + 12, next);
            d[p + 0x1b] = 0x24;
        }
        let p = 2 * L;
        w32(&mut d, p + 0x18, 2 | 2 << 13); // 2 offsets, 2 rows
        for i in 0..2 {
            let o = p + L - 6 - 2 * i;
            d[o..o + 2].copy_from_slice(&((i * 16) as u16).to_le_bytes());
        }
        d[p + L - 4] = 0b01; // only row 0 present
        d
    }

    #[test]
    fn one_page_table() {
        let d = synthetic();
        assert_eq!(rows(&d, 2).unwrap(), [2 * 512 + HEAP]);
        assert!(rows(&d, 9).unwrap().is_empty());
    }

    #[test]
    fn rejects_truncation_and_bad_offsets() {
        let d = synthetic();
        assert!(rows(&d[..1000], 2).is_err()); // data page cut off
        assert!(rows(&d[..20], 2).is_err());
        let mut bad = d.clone();
        bad[2 * 512 + 512 - 6..2 * 512 + 512 - 4].copy_from_slice(&0xffffu16.to_le_bytes());
        assert!(rows(&bad, 2).is_err()); // row offset past page
        let mut bad = d;
        bad[4..8].copy_from_slice(&8u32.to_le_bytes()); // page size too small
        assert!(rows(&bad, 2).is_err());
    }
}
