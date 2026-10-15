//! Zip archives (PKWARE APPNOTE 4.3: local headers, central directory, end record; names UTF-8; no zip64, no encryption).
//! [`Zip`] reads the entries of a file (stored or deflated, streamed, CRC-32 and size checked); [`write`] makes an archive
//! of stored entries. Used by the plugin installer ([`crate::plugin::install`]) and by the store backups ([`crate::store`]).
//! ponytail: sizes and offsets are `u32`, so an archive over 4 GiB is not supported.

use crate::paths;
use flate2::{Crc, read::DeflateDecoder};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Why an archive could not be read; the caller words what the user sees.
#[derive(Debug)]
pub enum Error {
    /// The file is no zip, or its central directory is damaged.
    NotZip,
    /// The entry's CRC-32 or size does not match its header.
    Crc(String),
    /// Anything else, as a message.
    Other(String),
}

fn io(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

struct Entry {
    name: String,
    method: u16,
    crc: u32,
    csize: u64,
    size: u64,
    local: u64,
}

/// The central directory of a zip, with the file to read entries from.
pub struct Zip {
    f: File,
    entries: Vec<Entry>,
}

fn le16(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes([b[at], b[at + 1]]) as usize
}

fn le32(b: &[u8], at: usize) -> u64 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as u64
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc::new();
    crc.update(data);
    crc.sum()
}

impl Zip {
    pub fn open(path: &Path) -> Result<Zip, Error> {
        let mut f = paths::at(File::open(path), path).map_err(Error::Other)?;
        let len = f.metadata().map_err(io)?.len();
        let tail_len = len.min(66_000);
        let mut tail = vec![0; tail_len as usize];
        f.seek(SeekFrom::Start(len - tail_len))
            .and_then(|_| f.read_exact(&mut tail))
            .map_err(io)?;
        let end = (0..tail.len().saturating_sub(21))
            .rev()
            .find(|&i| tail[i..].starts_with(&[0x50, 0x4b, 5, 6]))
            .ok_or(Error::NotZip)?;
        let (n, size, at) = (
            le16(&tail, end + 10),
            le32(&tail, end + 12),
            le32(&tail, end + 16),
        );
        let mut dir = vec![0; size as usize];
        f.seek(SeekFrom::Start(at))
            .and_then(|_| f.read_exact(&mut dir))
            .map_err(|_| Error::NotZip)?;
        let (mut entries, mut p) = (Vec::new(), 0);
        for _ in 0..n {
            if dir.get(p..p + 46).is_none() || le32(&dir, p) != 0x0201_4b50 {
                return Err(Error::NotZip);
            }
            let (nl, xl, cl) = (le16(&dir, p + 28), le16(&dir, p + 30), le16(&dir, p + 32));
            let name = dir.get(p + 46..p + 46 + nl).ok_or(Error::NotZip)?;
            entries.push(Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                method: le16(&dir, p + 10) as u16,
                crc: le32(&dir, p + 16) as u32,
                csize: le32(&dir, p + 20),
                size: le32(&dir, p + 24),
                local: le32(&dir, p + 42),
            });
            p += 46 + nl + xl + cl;
        }
        Ok(Zip { f, entries })
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Every entry name, in archive order (folders end with `/`).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// Whether the archive has an entry `name`.
    pub fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// The uncompressed size of entry `name`.
    pub fn size(&self, name: &str) -> Option<u64> {
        self.find(name).map(|e| e.size)
    }

    /// Streams entry `name` into `out`, checking its CRC-32 and size.
    pub fn extract(&mut self, name: &str, out: &mut dyn Write) -> Result<(), Error> {
        let e = self
            .find(name)
            .ok_or_else(|| Error::Other(format!("{name} is not in the zip")))?;
        let (method, crc, csize, size, local) = (e.method, e.crc, e.csize, e.size, e.local);
        let mut h = [0; 30];
        self.f
            .seek(SeekFrom::Start(local))
            .and_then(|_| self.f.read_exact(&mut h))
            .map_err(io)?;
        if le32(&h, 0) != 0x0403_4b50 {
            return Err(Error::Other(format!("{name}: damaged zip")));
        }
        let data = local + 30 + le16(&h, 26) as u64 + le16(&h, 28) as u64;
        self.f.seek(SeekFrom::Start(data)).map_err(io)?;
        let raw = (&self.f).take(csize);
        let mut src: Box<dyn Read> = match method {
            0 => Box::new(raw),
            8 => Box::new(DeflateDecoder::new(raw)),
            m => {
                return Err(Error::Other(format!(
                    "{name}: zip method {m} is not supported (stored and deflate are)"
                )));
            }
        };
        let (mut sum, mut buf, mut n) = (Crc::new(), vec![0; 1 << 16], 0u64);
        loop {
            let k = src
                .read(&mut buf)
                .map_err(|e| Error::Other(format!("{name}: {e}")))?;
            if k == 0 {
                break;
            }
            sum.update(&buf[..k]);
            out.write_all(&buf[..k])
                .map_err(|e| Error::Other(format!("{name}: {e}")))?;
            n += k as u64;
        }
        if sum.sum() != crc || n != size {
            return Err(Error::Crc(name.to_owned()));
        }
        Ok(())
    }

    /// The whole entry `name`, CRC-checked.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        let mut v = Vec::new();
        self.extract(name, &mut v)?;
        Ok(v)
    }
}

/// A zip of stored entries (method 0), made in memory.
pub fn write(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    let u16le = |v: &mut Vec<u8>, n: u16| v.extend(n.to_le_bytes());
    let u32le = |v: &mut Vec<u8>, n: u32| v.extend(n.to_le_bytes());
    for (name, data) in entries {
        let offset = out.len() as u32;
        let (crc, size) = (crc32(data), data.len() as u32);
        // 1980-01-01 00:00: zip has no "unknown" timestamp.
        for (v, long) in [(&mut out, false), (&mut central, true)] {
            u32le(v, if long { 0x0201_4b50 } else { 0x0403_4b50 });
            if long {
                u16le(v, 20); // made by
            }
            for n in [20, 0x0800, 0, 0, 0x21] {
                u16le(v, n); // needed, flags (UTF-8 names), method (stored), time, date
            }
            u32le(v, crc);
            u32le(v, size);
            u32le(v, size);
            u16le(v, name.len() as u16);
            u16le(v, 0); // extra
            if long {
                for _ in 0..4 {
                    u16le(v, 0); // comment, disk, internal attributes (and the first half of external)
                }
                u16le(v, 0);
                u32le(v, offset);
            }
            v.extend(name.as_bytes());
        }
        out.extend(data);
    }
    let (cd_at, cd_len) = (out.len() as u32, central.len() as u32);
    out.extend(central);
    u32le(&mut out, 0x0605_4b50);
    for n in [0, 0, entries.len() as u16, entries.len() as u16] {
        u16le(&mut out, n);
    }
    u32le(&mut out, cd_len);
    u32le(&mut out, cd_at);
    u16le(&mut out, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_round_trips_and_detects_damage() {
        let dir = std::env::temp_dir().join(format!("openfader-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (good, bad, junk) = (
            dir.join("good.zip"),
            dir.join("bad.zip"),
            dir.join("junk.zip"),
        );
        let z = write(&[
            ("a.txt", b"hello".to_vec()),
            ("dir/ü.bin", vec![0, 1, 2, 255]),
            ("empty", Vec::new()),
        ]);
        std::fs::write(&good, &z).unwrap();
        let mut zip = Zip::open(&good).unwrap();
        assert_eq!(zip.read("a.txt").unwrap(), b"hello");
        assert_eq!(zip.read("dir/ü.bin").unwrap(), [0, 1, 2, 255]);
        assert_eq!(zip.read("empty").unwrap(), b"");
        assert_eq!(
            (zip.has("a.txt"), zip.size("a.txt"), zip.has("b")),
            (true, Some(5), false)
        );
        assert!(matches!(zip.read("b"), Err(Error::Other(_))));

        let mut damaged = z.clone();
        damaged[36] ^= 1; // inside the first entry's data (30 header bytes + the 5-byte name)
        std::fs::write(&bad, damaged).unwrap();
        assert!(
            matches!(Zip::open(&bad).unwrap().read("a.txt"), Err(Error::Crc(n)) if n == "a.txt")
        );
        std::fs::write(&junk, b"PK nope").unwrap();
        assert!(matches!(Zip::open(&junk), Err(Error::NotZip)));
        assert!(matches!(
            Zip::open(&dir.join("missing.zip")),
            Err(Error::Other(_))
        ));
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926); // the standard check value
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
