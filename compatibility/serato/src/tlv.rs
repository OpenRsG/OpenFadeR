//! Serato's record format, shared by `database V2` and the crates: a 4-byte ASCII tag, a big-endian `u32` length, the data.
//! The first letter of a tag types its data: `o` holds nested records, `t` and `p` hold UTF-16BE text (`p`: a path), `u` a
//! big-endian `u32`, `b` one byte. Observed in the files Serato writes and in Mixxx's reader
//! (<https://github.com/mixxxdj/mixxx/blob/main/src/library/serato/seratofeature.cpp>).

/// One record: its tag and its data (for an `o` tag, more records).
pub struct Rec<'a> {
    pub tag: [u8; 4],
    pub data: &'a [u8],
}

impl Rec<'_> {
    pub fn text(&self) -> String {
        from_utf16(self.data)
    }
}

/// The records laid end to end in `buf`; `Err` when one is cut short.
pub fn parse(buf: &[u8]) -> Result<Vec<Rec<'_>>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let h = buf.get(pos..pos + 8).ok_or("cut record header")?;
        let len = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize;
        let data = buf
            .get(pos + 8..pos + 8 + len)
            .ok_or_else(|| format!("cut record {}", String::from_utf8_lossy(&h[..4])))?;
        out.push(Rec {
            tag: [h[0], h[1], h[2], h[3]],
            data,
        });
        pos += 8 + len;
    }
    Ok(out)
}

/// The data of the first record tagged `tag`.
pub fn find<'a>(recs: &[Rec<'a>], tag: &[u8; 4]) -> Option<&'a [u8]> {
    recs.iter().find(|r| &r.tag == tag).map(|r| r.data)
}

pub fn rec(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + data.len());
    v.extend_from_slice(tag);
    v.extend_from_slice(&(data.len() as u32).to_be_bytes());
    v.extend_from_slice(data);
    v
}

pub fn text(tag: &[u8; 4], s: &str) -> Vec<u8> {
    rec(
        tag,
        &s.encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>(),
    )
}

pub fn from_utf16(b: &[u8]) -> String {
    String::from_utf16_lossy(
        &b.chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect::<Vec<_>>(),
    )
}
