//! What a cover picture is: its type from the first bytes, a download with a user agent and a size limit, and base64.

use std::{io::Read, time::Duration};

/// Largest picture accepted (bytes): a cover is a few hundred KB, a 12 MB answer is not a cover.
pub const MAX_BYTES: u64 = 12 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// A picture type the app can show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kind {
    /// File extension.
    pub ext: &'static str,
    /// Media type, as a tag stores it.
    pub mime: &'static str,
}

/// The type of the picture `b` starts like (JPEG, PNG, GIF or WebP); `None` for anything else.
pub fn kind(b: &[u8]) -> Option<Kind> {
    let k = |ext, mime| Some(Kind { ext, mime });
    match b {
        [0xff, 0xd8, 0xff, ..] => k("jpg", "image/jpeg"),
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => k("png", "image/png"),
        [b'G', b'I', b'F', b'8', ..] => k("gif", "image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => k("webp", "image/webp"),
        _ => None,
    }
}

/// The user agent of every request the app makes for a picture: the app and where to find it, as MusicBrainz and others ask.
pub fn user_agent() -> String {
    format!(
        "OpenFadeR/{} (+https://github.com/OpenRsG/OpenFadeR)",
        env!("CARGO_PKG_VERSION")
    )
}

/// The body of `url` (`http` / `https`), at most [`MAX_BYTES`].
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(user_agent())
        .build()
        .into();
    // One retry for a server error: the Cover Art Archive's mirrors answer an odd 502 and 503 that the next request does not.
    let resp = match agent.get(url).call() {
        Err(ureq::Error::StatusCode(500..=599)) => agent.get(url).call(),
        r => r,
    }
    .map_err(|e| format!("{url}: {e}"))?;
    let mut out = Vec::new();
    resp.into_body()
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("{url}: {e}"))?;
    if out.len() as u64 > MAX_BYTES {
        return Err(format!("{url}: more than {} MB", MAX_BYTES >> 20));
    }
    Ok(out)
}

/// The bytes of standard base64 (padding and white space allowed, anything else makes it `None`).
pub fn base64(text: &str) -> Option<Vec<u8>> {
    let (mut out, mut acc, mut bits) = (Vec::with_capacity(text.len() / 4 * 3), 0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_with_and_without_padding_and_rejects_other_text() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64("aGVs\nbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64("").unwrap(), b"");
        assert_eq!(base64("a*b"), None);
    }

    #[test]
    fn pictures_are_told_by_their_first_bytes() {
        assert_eq!(
            kind(&[0xff, 0xd8, 0xff, 0xe0, 0]).unwrap().mime,
            "image/jpeg"
        );
        assert_eq!(kind(b"\x89PNG\r\n\x1a\n....").unwrap().ext, "png");
        assert_eq!(kind(b"RIFF\0\0\0\0WEBPVP8 ").unwrap().ext, "webp");
        assert_eq!(kind(b"<html>"), None);
        assert_eq!(kind(b""), None);
    }
}
