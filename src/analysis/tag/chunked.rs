//! The ID3v2 tag inside a WAV (RIFF: little-endian sizes, chunk `id3 `) or AIFF (FORM: big-endian sizes, chunk `ID3 `)
//! file. Both are a 12-byte header followed by chunks `id, size u32, data, pad to even`; the header size counts the bytes
//! after it. Writing drops the old tag chunk (its frames are carried into the new tag) and appends the new one after
//! the last chunk. Both spellings of the chunk id are read.

use super::{key_of_tag, parse_id3, with_key};

struct Chunk {
    start: usize,
    data: std::ops::Range<usize>,
    /// After the chunk's pad byte (or its data when the file ends without the pad).
    end: usize,
}

fn u32_at(b: &[u8], at: usize, le: bool) -> usize {
    let w: [u8; 4] = b[at..at + 4].try_into().unwrap();
    (if le {
        u32::from_le_bytes(w)
    } else {
        u32::from_be_bytes(w)
    }) as usize
}

fn size_bytes(n: usize, le: bool) -> [u8; 4] {
    let n = n as u32;
    if le { n.to_le_bytes() } else { n.to_be_bytes() }
}

/// The chunks and where the container ends (the header's size, clipped to the file).
fn chunks(b: &[u8], le: bool) -> Result<(Vec<Chunk>, usize), String> {
    let (magic, forms): (&[u8], &[&[u8]]) = if le {
        (b"RIFF", &[b"WAVE"])
    } else {
        (b"FORM", &[b"AIFF", b"AIFC"])
    };
    if b.len() < 12 || &b[..4] != magic || !forms.contains(&&b[8..12]) {
        return Err(format!("not a {} file", if le { "WAV" } else { "AIFF" }));
    }
    let end = (8 + u32_at(b, 4, le)).min(b.len());
    let (mut out, mut at) = (Vec::new(), 12);
    while at + 8 <= end {
        let n = u32_at(b, at + 4, le);
        let data_end = (at + 8)
            .checked_add(n)
            .filter(|&e| e <= end)
            .ok_or("a chunk runs past the end of the file")?;
        let next = (data_end + (n & 1)).min(end);
        out.push(Chunk {
            start: at,
            data: at + 8..data_end,
            end: next,
        });
        at = next;
    }
    Ok((out, end))
}

fn is_id3(b: &[u8], c: &Chunk) -> bool {
    b[c.start..c.start + 4].eq_ignore_ascii_case(b"id3 ")
}

pub(super) fn read(b: &[u8], le: bool) -> Option<String> {
    let (chunks, _) = chunks(b, le).ok()?;
    let c = chunks.iter().find(|c| is_id3(b, c))?;
    key_of_tag(&parse_id3(&b[c.data.clone()]).ok()??)
}

/// `b` with its tag chunk holding `key` (a new tag chunk when there was none).
pub(super) fn write(b: &[u8], le: bool, key: &str) -> Result<Vec<u8>, String> {
    let (chunks, end) = chunks(b, le)?;
    let old = chunks.iter().find(|c| is_id3(b, c));
    let tag = with_key(
        old.map(|c| parse_id3(&b[c.data.clone()]))
            .transpose()?
            .flatten(),
        key,
    );
    let mut out = b[..12].to_vec();
    for c in chunks.iter().filter(|c| !is_id3(b, c)) {
        out.extend_from_slice(&b[c.start..c.end]);
        if c.data.len() % 2 == 1 && c.end == c.data.end {
            out.push(0);
        }
    }
    out.extend_from_slice(if le { b"id3 " } else { b"ID3 " });
    out.extend_from_slice(&size_bytes(tag.len(), le));
    out.extend_from_slice(&tag);
    if tag.len() % 2 == 1 {
        out.push(0);
    }
    let size = size_bytes(out.len() - 8, le);
    out[4..8].copy_from_slice(&size);
    out.extend_from_slice(&b[end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &[u8; 4], data: &[u8], le: bool) -> Vec<u8> {
        let mut c = [id.as_slice(), &size_bytes(data.len(), le), data].concat();
        if data.len() % 2 == 1 {
            c.push(0);
        }
        c
    }

    fn container(form: &[u8; 4], le: bool, chunks: &[Vec<u8>]) -> Vec<u8> {
        let body = [form.as_slice(), &chunks.concat()].concat();
        [
            if le { b"RIFF" } else { b"FORM" }.as_slice(),
            &size_bytes(body.len(), le),
            &body,
        ]
        .concat()
    }

    fn id3(frames: &[(&[u8; 4], &str)]) -> Vec<u8> {
        let body: Vec<u8> = frames
            .iter()
            .flat_map(|(id, t)| {
                [
                    id.as_slice(),
                    &(t.len() as u32 + 1).to_be_bytes(),
                    &[0, 0, 0],
                    t.as_bytes(),
                ]
                .concat()
            })
            .collect();
        [
            b"ID3\x03\x00\x00".as_slice(),
            &super::super::to_synchsafe(body.len()),
            &body,
        ]
        .concat()
    }

    #[test]
    fn wav_and_aiff_get_a_tag_chunk_and_keep_everything_else() {
        for (le, form, name) in [(true, b"WAVE", b"id3 "), (false, b"AIFF", b"ID3 ")] {
            let (fmt, data) = (chunk(b"fmt ", &[1; 16], le), chunk(b"data", &[7; 11], le)); // odd length: padded
            let plain = container(form, le, &[fmt.clone(), data.clone()]);
            assert_eq!(read(&plain, le), None);
            let once = write(&plain, le, "8A").unwrap();
            assert_eq!(read(&once, le).as_deref(), Some("8A"));
            let (cs, end) = chunks(&once, le).unwrap();
            assert_eq!(end, once.len(), "the container size matches the file");
            assert_eq!(cs.len(), 3);
            assert_eq!(
                &once[cs[0].start..cs[1].end],
                [fmt.clone(), data.clone()].concat().as_slice(),
                "the other chunks are untouched"
            );
            assert_eq!(&once[cs[2].start..cs[2].start + 4], name);
            // writing again replaces the tag chunk instead of adding one, and the old frames survive
            let tagged = container(
                form,
                le,
                &[
                    fmt.clone(),
                    chunk(name, &id3(&[(b"TIT2", "Song"), (b"TKEY", "Dm")]), le),
                    data.clone(),
                ],
            );
            assert_eq!(read(&tagged, le).as_deref(), Some("Dm"));
            let twice = write(&tagged, le, "9B").unwrap();
            assert_eq!(read(&twice, le).as_deref(), Some("9B"));
            let (cs, _) = chunks(&twice, le).unwrap();
            assert_eq!(cs.iter().filter(|c| is_id3(&twice, c)).count(), 1);
            assert!(
                twice.windows(4).any(|w| w == b"TIT2"),
                "other frames are kept"
            );
            assert_eq!(
                write(&twice, le, "9B").unwrap(),
                twice,
                "same key: same bytes"
            );
        }
    }

    #[test]
    fn broken_containers_are_errors() {
        assert!(
            write(b"RIFF\x04\x00\x00\x00WAVE", true, "8A").is_ok(),
            "an empty WAV can be tagged"
        );
        assert!(write(b"nope", true, "8A").is_err());
        assert!(
            write(b"FORM\x00\x00\x00\x04AIFF", true, "8A").is_err(),
            "AIFF is not RIFF"
        );
        let cut = [
            b"RIFF\x14\x00\x00\x00WAVE".as_slice(),
            b"data\xff\x00\x00\x00abcd",
        ]
        .concat();
        assert!(write(&cut, true, "8A").is_err());
    }
}
