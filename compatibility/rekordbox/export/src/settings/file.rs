//! The container of the four settings files and the [`Settings`] model over them (format: see the parent module).

use super::{Field, Kind};
use std::path::Path;

/// Offset of the data: `len_stringdata u4`, the three 32-byte strings, `len_data u4`.
const DATA_AT: usize = 0x68;

/// CRC-16/XMODEM (poly 0x1021, init 0).
pub(super) fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut c, &b| {
        c ^= u16::from(b) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                c << 1 ^ 0x1021
            } else {
                c << 1
            };
        }
        c
    })
}

/// One parsed file: the three header strings and the data bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct File {
    brand: String,
    software: String,
    version: String,
    data: Vec<u8>,
}

impl File {
    fn new(kind: Kind, data: Vec<u8>) -> Self {
        let (brand, version) = kind.header();
        Self {
            brand: brand.into(),
            software: "rekordbox".into(),
            version: version.into(),
            data,
        }
    }

    fn parse(kind: Kind, b: &[u8]) -> Result<Self, String> {
        let name = kind.file_name();
        let word = |at: usize| {
            b.get(at..at + 4)
                .and_then(|w| w.try_into().ok())
                .map(|w| u32::from_le_bytes(w) as usize)
        };
        let bad = |why: &str| Err(format!("{name}: {why}"));
        if word(0) != Some(0x60) {
            return bad("string data length is not 0x60");
        }
        let len = word(DATA_AT - 4).unwrap_or(usize::MAX);
        if len != kind.data_len() || b.len() != DATA_AT + len + 4 {
            return bad(&format!(
                "size {} does not fit a data length of {}",
                b.len(),
                kind.data_len()
            ));
        }
        let text = |i: usize| -> Result<String, String> {
            let f = &b[4 + 32 * i..36 + 32 * i];
            let n = f
                .iter()
                .position(|&c| c == 0)
                .filter(|&n| f[n..].iter().all(|&c| c == 0));
            n.and_then(|n| std::str::from_utf8(&f[..n]).ok())
                .map(str::to_owned)
                .ok_or_else(|| format!("{name}: header string {i} is not a NUL-padded text"))
        };
        let end = DATA_AT + len;
        let covered = if kind == Kind::Djm {
            &b[..end]
        } else {
            &b[DATA_AT..end]
        };
        if u16::from_le_bytes([b[end], b[end + 1]]) != crc16(covered) {
            return bad("checksum mismatch");
        }
        if b[end + 2..] != [0, 0] {
            return bad("trailer is not zero");
        }
        Ok(Self {
            brand: text(0)?,
            software: text(1)?,
            version: text(2)?,
            data: b[DATA_AT..end].to_vec(),
        })
    }

    fn to_bytes(&self, kind: Kind) -> Vec<u8> {
        let mut f = Vec::with_capacity(DATA_AT + self.data.len() + 4);
        f.extend(0x60u32.to_le_bytes());
        for s in [&self.brand, &self.software, &self.version] {
            let mut field = [0u8; 32];
            let n = s.len().min(31);
            field[..n].copy_from_slice(&s.as_bytes()[..n]);
            f.extend(field);
        }
        f.extend((self.data.len() as u32).to_le_bytes());
        f.extend(&self.data);
        let crc = crc16(if kind == Kind::Djm { &f } else { &self.data });
        f.extend(crc.to_le_bytes());
        f.extend([0, 0]);
        f
    }
}

/// The four files, indexed by [`Kind`]. `Default` is what rekordbox 7 writes for a fresh export (observed bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    files: [File; 4],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            files: Kind::ALL.map(|k| File::new(k, k.default_data())),
        }
    }
}

impl Settings {
    fn file(&self, k: Kind) -> &File {
        &self.files[k as usize]
    }

    fn byte_mut(&mut self, f: &Field) -> &mut u8 {
        &mut self.files[f.kind as usize].data[f.offset]
    }

    /// Parses the bytes of one file into the model (headers, length and checksum are verified).
    pub fn parse(&mut self, kind: Kind, bytes: &[u8]) -> Result<(), String> {
        self.files[kind as usize] = File::parse(kind, bytes)?;
        Ok(())
    }

    /// The files under `pioneer` (`<device>/PIONEER`); a missing file keeps its default (real rekordbox 7 exports have no
    /// `DEVSETTING.DAT`), a damaged one is an error.
    pub fn read(pioneer: &Path) -> Result<Self, String> {
        let mut s = Self::default();
        for kind in Kind::ALL {
            let path = pioneer.join(kind.file_name());
            match std::fs::read(&path) {
                Ok(b) => s.parse(kind, &b)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        Ok(s)
    }

    /// `(file name, bytes)` of the four files.
    pub fn to_bytes(&self) -> Vec<(&'static str, Vec<u8>)> {
        Kind::ALL
            .iter()
            .map(|&k| (k.file_name(), self.file(k).to_bytes(k)))
            .collect()
    }

    /// The value byte of `f`.
    pub fn byte(&self, f: &Field) -> u8 {
        self.file(f.kind).data[f.offset]
    }

    /// Index into `f.choices` of the current value; `None` when the file holds a byte the spec does not list.
    #[cfg(test)]
    pub fn get(&self, f: &Field) -> Option<usize> {
        f.choices.iter().position(|c| c.0 == self.byte(f))
    }

    /// Selects `f.choices[i]`.
    #[cfg(test)]
    pub fn set(&mut self, f: &Field, i: usize) {
        *self.byte_mut(f) = f.choices[i].0;
    }

    /// Sets a raw value byte; false (nothing changed) when it is not one of `f.choices`.
    pub fn set_byte(&mut self, f: &Field, b: u8) -> bool {
        let ok = f.choices.iter().any(|c| c.0 == b);
        if ok {
            *self.byte_mut(f) = b;
        }
        ok
    }
}
