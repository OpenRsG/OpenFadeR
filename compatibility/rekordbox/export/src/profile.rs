//! `PIONEER/djprofile.nxs`: the DJ profile ("account key") rekordbox puts on every device it exports to, 160 bytes, no
//! checksum known. Layout (observed: the user's stick copy; the web sources agree: the baken project's analysis of two
//! samples, https://github.com/M-Igashi/baken/pull/238):
//!
//! ```text
//! 0x00 u4   unknown, differs between accounts      (zero in a profile we write: inferred; [`Profile::head`])
//! 0x04 u8   Unix time in milliseconds, big endian  (observed: 2019-01-23 on the user's stick, whose file was written 2026-09-09,
//!                                                    so it is when the profile was made, not the export; baken's two samples
//!                                                    equal their file's mtime)
//! 0x0c 16B  zero
//! 0x1c u4   unknown                                (zero when we write; [`Profile::tail`])
//! 0x20 128B profile name, NUL padded                (observed ASCII; written as UTF-8: inferred)
//! ```
//!
//! The file is optional for players. [`write`] only creates it: a profile a device already has (rekordbox's, with the real
//! account name) is never touched.

use std::path::Path;

pub const FILE: &str = "djprofile.nxs";
const LEN: usize = 160;
const NAME: usize = 0x20;

/// A decoded `djprofile.nxs`; `head` and `tail` are the unknown u4s, kept so a parsed file writes back byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub head: [u8; 4],
    pub millis: u64,
    pub tail: [u8; 4],
    pub name: String,
}

impl Profile {
    /// A new profile for `name` (cut to 127 bytes on a character boundary) made at `millis`.
    pub fn new(name: &str, millis: u64) -> Self {
        let mut n = name.len().min(NAME * 4 - 1);
        while !name.is_char_boundary(n) {
            n -= 1;
        }
        Self {
            head: [0; 4],
            millis,
            tail: [0; 4],
            name: name[..n].to_owned(),
        }
    }

    pub fn parse(b: &[u8]) -> Result<Self, String> {
        if b.len() != LEN {
            return Err(format!("{FILE}: {} bytes, expected {LEN}", b.len()));
        }
        if b[0x0c..0x1c].iter().any(|&x| x != 0) {
            return Err(format!("{FILE}: bytes 0x0c..0x1c are not zero"));
        }
        let name = &b[NAME..];
        let end = name.iter().position(|&x| x == 0).unwrap_or(name.len());
        if name[end..].iter().any(|&x| x != 0) {
            return Err(format!("{FILE}: data after the name's NUL"));
        }
        // `b` is LEN bytes (checked above), so the fixed-width slices convert.
        Ok(Self {
            head: b[..4].try_into().unwrap(),
            millis: u64::from_be_bytes(b[4..12].try_into().unwrap()),
            tail: b[0x1c..0x20].try_into().unwrap(),
            name: String::from_utf8(name[..end].to_vec()).map_err(|e| format!("{FILE}: {e}"))?,
        })
    }

    pub fn to_bytes(&self) -> [u8; LEN] {
        let mut f = [0u8; LEN];
        f[..4].copy_from_slice(&self.head);
        f[4..12].copy_from_slice(&self.millis.to_be_bytes());
        f[0x1c..0x20].copy_from_slice(&self.tail);
        f[NAME..NAME + self.name.len()].copy_from_slice(self.name.as_bytes());
        f
    }
}

/// The account name to write: the login name of the OS user (rekordbox writes its account's name; we have none).
pub(super) fn user() -> String {
    ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "openfader".into())
}

/// Creates `<pioneer>/djprofile.nxs` for `name` unless the device has one; true when it was written. An empty name writes nothing.
pub fn write(pioneer: &Path, name: &str) -> Result<bool, String> {
    let path = pioneer.join(FILE);
    if name.is_empty() || path.exists() {
        return Ok(false);
    }
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    super::put(&path, &Profile::new(name, millis).to_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_unknown_bytes_and_long_names_are_cut_on_a_character() {
        let mut p = Profile::new("Zoë", 1_548_285_281_931);
        p.head = [0, 0x16, 0x81, 0x6b];
        p.tail = [0xe8, 0x86, 0x9d, 0xf3];
        assert_eq!(Profile::parse(&p.to_bytes()).unwrap(), p);
        assert_eq!(&p.to_bytes()[4..12], &1_548_285_281_931u64.to_be_bytes());
        let long = Profile::new(&"é".repeat(100), 0);
        assert_eq!(long.name.len(), 126);
        assert!(Profile::parse(&long.to_bytes()).is_ok());
        assert!(Profile::parse(&[0; 159]).is_err());
    }

    #[test]
    fn write_creates_once_and_never_replaces_the_devices_own() {
        let d = std::env::temp_dir().join(format!("rk-nxs-{}", std::process::id()));
        let p = d.join("PIONEER");
        assert!(!write(&p, "").unwrap());
        assert!(write(&p, "DJ One").unwrap());
        assert!(!write(&p, "DJ Two").unwrap());
        let b = std::fs::read(p.join(FILE)).unwrap();
        assert_eq!(Profile::parse(&b).unwrap().name, "DJ One");
        let _ = std::fs::remove_dir_all(d); // cleanup: Windows may still hold a handle
    }
}
