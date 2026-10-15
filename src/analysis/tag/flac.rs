//! `INITIALKEY` in a FLAC file's Vorbis comment block. A FLAC file is `fLaC`, metadata blocks (`last flag + type u8,
//! length u24 BE`, data), then the audio frames. The Vorbis comment block (type 4) is `vendor_len u32 LE, vendor,
//! count u32 LE, count x (len u32 LE, "NAME=value")`. Writing replaces the `INITIALKEY` comment (any case), keeps every
//! other comment and block, and adds the block after STREAMINFO when the file has none.

const COMMENT: u8 = 4;
const FIELD: &str = "INITIALKEY";

struct Block {
    ty: u8,
    data: std::ops::Range<usize>,
}

/// The metadata blocks and where the audio starts.
fn blocks(b: &[u8]) -> Result<(Vec<Block>, usize), String> {
    if b.get(..4) != Some(b"fLaC") {
        return Err("not a FLAC file".into());
    }
    let (mut out, mut at) = (Vec::new(), 4);
    loop {
        let h = b.get(at..at + 4).ok_or("truncated FLAC metadata")?;
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        let data = at + 4..at + 4 + len;
        if data.end > b.len() {
            return Err("truncated FLAC metadata block".into());
        }
        out.push(Block {
            ty: h[0] & 0x7f,
            data: data.clone(),
        });
        at = data.end;
        if h[0] & 0x80 != 0 {
            return Ok((out, at));
        }
    }
}

/// Vendor string and comments of a Vorbis comment block, each as stored.
fn comments(d: &[u8]) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    let bad = || "bad Vorbis comment block".to_owned();
    let word = |at: usize| {
        d.get(at..at + 4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()) as usize)
            .ok_or_else(bad)
    };
    let vlen = word(0)?;
    let vendor = d.get(4..4 + vlen).ok_or_else(bad)?.to_vec();
    let (n, mut at) = (word(4 + vlen)?, 8 + vlen);
    let mut list = Vec::new();
    for _ in 0..n {
        let len = word(at)?;
        list.push(d.get(at + 4..at + 4 + len).ok_or_else(bad)?.to_vec());
        at += 4 + len;
    }
    Ok((vendor, list))
}

fn named(c: &[u8]) -> bool {
    c.len() > FIELD.len()
        && c[..FIELD.len()].eq_ignore_ascii_case(FIELD.as_bytes())
        && c[FIELD.len()] == b'='
}

pub(super) fn read(b: &[u8]) -> Option<String> {
    let (blocks, _) = blocks(b).ok()?;
    let block = blocks.iter().find(|x| x.ty == COMMENT)?;
    let (_, list) = comments(&b[block.data.clone()]).ok()?;
    let c = list.iter().find(|c| named(c))?;
    Some(String::from_utf8_lossy(&c[FIELD.len() + 1..]).into_owned()).filter(|k| !k.is_empty())
}

/// `b` with `INITIALKEY=key` in its Vorbis comment block.
pub(super) fn write(b: &[u8], key: &str) -> Result<Vec<u8>, String> {
    let (blocks, audio) = blocks(b)?;
    let at = blocks.iter().position(|x| x.ty == COMMENT);
    let (vendor, mut list) = at.map_or(Ok((Vec::new(), Vec::new())), |i| {
        comments(&b[blocks[i].data.clone()])
    })?;
    list.retain(|c| !named(c));
    list.push(format!("{FIELD}={key}").into_bytes());
    let mut data = (vendor.len() as u32).to_le_bytes().to_vec();
    data.extend_from_slice(&vendor);
    data.extend_from_slice(&(list.len() as u32).to_le_bytes());
    for c in &list {
        data.extend_from_slice(&(c.len() as u32).to_le_bytes());
        data.extend_from_slice(c);
    }
    if data.len() >= 1 << 24 {
        return Err("the Vorbis comment block would be too big".into());
    }
    let mut parts: Vec<(u8, &[u8])> = blocks.iter().map(|x| (x.ty, &b[x.data.clone()])).collect();
    match at {
        Some(i) => parts[i].1 = &data,
        None => parts.insert(1, (COMMENT, &data)),
    }
    let mut out = b"fLaC".to_vec();
    for (i, (ty, d)) in parts.iter().enumerate() {
        let last = if i + 1 == parts.len() { 0x80 } else { 0 };
        out.extend_from_slice(&[ty | last]);
        out.extend_from_slice(&(d.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(d);
    }
    out.extend_from_slice(&b[audio..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(ty: u8, last: bool, d: &[u8]) -> Vec<u8> {
        [
            &[ty | if last { 0x80 } else { 0 }][..],
            &(d.len() as u32).to_be_bytes()[1..],
            d,
        ]
        .concat()
    }

    fn comment_block(vendor: &str, list: &[&str]) -> Vec<u8> {
        let mut d = [
            (vendor.len() as u32).to_le_bytes().as_slice(),
            vendor.as_bytes(),
            &(list.len() as u32).to_le_bytes(),
        ]
        .concat();
        for c in list {
            d.extend_from_slice(&(c.len() as u32).to_le_bytes());
            d.extend_from_slice(c.as_bytes());
        }
        d
    }

    const FRAMES: &[u8] = &[0xff, 0xf8, 9, 8, 7];

    #[test]
    fn adds_a_comment_block_after_streaminfo_when_there_is_none() {
        let f = [b"fLaC".as_slice(), &block(0, true, &[0; 34]), FRAMES].concat();
        assert_eq!(read(&f), None);
        let w = write(&f, "8A").unwrap();
        assert_eq!(read(&w).as_deref(), Some("8A"));
        let (bs, audio) = blocks(&w).unwrap();
        assert_eq!(bs.iter().map(|b| b.ty).collect::<Vec<_>>(), [0, COMMENT]);
        assert_eq!(&w[audio..], FRAMES, "the audio frames are untouched");
        assert_eq!(w[4] & 0x80, 0, "STREAMINFO is no longer the last block");
        assert_eq!(write(&w, "8A").unwrap(), w, "same key: same bytes");
    }

    #[test]
    fn replaces_initialkey_in_any_case_and_keeps_the_rest() {
        let c = comment_block("lib 1", &["TITLE=Song", "initialkey=Dm", "ARTIST=Me"]);
        let f = [
            b"fLaC".as_slice(),
            &block(0, false, &[0; 34]),
            &block(1, false, &[0; 8]),
            &block(COMMENT, true, &c),
            FRAMES,
        ]
        .concat();
        assert_eq!(read(&f).as_deref(), Some("Dm"));
        let w = write(&f, "7A").unwrap();
        assert_eq!(read(&w).as_deref(), Some("7A"));
        let (bs, audio) = blocks(&w).unwrap();
        assert_eq!(
            bs.iter().map(|b| b.ty).collect::<Vec<_>>(),
            [0, 1, COMMENT],
            "padding block and order are kept"
        );
        let (vendor, list) = comments(&w[bs[2].data.clone()]).unwrap();
        assert_eq!(vendor, b"lib 1");
        let text: Vec<_> = list
            .iter()
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        assert_eq!(text, ["TITLE=Song", "ARTIST=Me", "INITIALKEY=7A"]);
        assert_eq!(&w[audio..], FRAMES);
    }

    #[test]
    fn broken_files_are_errors() {
        assert!(write(b"RIFF....", "8A").is_err());
        assert!(
            write(b"fLaC\x80\x00\x00\x22", "8A").is_err(),
            "block longer than the file"
        );
        let bad = [
            b"fLaC".as_slice(),
            &block(0, false, &[0; 34]),
            &block(COMMENT, true, &[255, 255, 255, 127]),
        ]
        .concat();
        assert!(write(&bad, "8A").is_err());
    }
}
