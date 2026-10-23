//! Takes what a person would not want to publish out of a report: file paths and secrets.
//!
//! A path is a word that starts with `/`, `~/`, `X:\`, `X:/` or `\\` (a quoted one runs to its closing quote, any other to the next
//! space) and becomes `<path>`. A secret is the value of a `key=value` whose key names a secret, the word after `bearer` /
//! `token` / `password`…, the password of a `scheme://user:pass@host` address, a word with a well-known token prefix, or any
//! 32+ character word of letters and digits (so a full hash goes too). It becomes `<redacted>`. A path with spaces that is not
//! quoted keeps its tail after the first space: the report window shows the text before anything leaves the machine.

use std::collections::HashSet;

const SECRET_KEYS: [&str; 8] = [
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
];
/// A word after one of these is a secret itself (`Authorization: Bearer abc`).
const SECRET_BEFORE: [&str; 4] = ["bearer", "token", "password", "secret"];
const TOKEN_PREFIXES: [&str; 10] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "sk-",
    "xoxb-",
    "xoxp-",
    "AKIA",
];

/// `text` with its paths and secrets replaced; line breaks are kept.
pub fn redact(text: &str) -> String {
    let home = crate::paths::home()
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| h.len() > 3);
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = match &home {
            Some(h) => line.replace(h.as_str(), "~"),
            None => line.to_owned(),
        };
        out.push_str(&secrets(&paths(&line)));
    }
    out
}

/// The settings that differ from the defaults, as `key: value` lines: `current` and `default` are settings-file texts
/// (`key = value` lines). A secret's value and a path's text are replaced.
pub fn settings(current: &str, default: &str) -> String {
    let base: HashSet<&str> = default.lines().map(str::trim).collect();
    let mut out = Vec::new();
    for line in current.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || base.contains(line) {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .map_or((line, ""), |(k, v)| (k.trim(), v.trim()));
        let value = if is_secret_key(key) {
            "<redacted>".to_owned()
        } else if path_at(value, 0) {
            "<path>".to_owned()
        } else {
            redact(value)
        };
        out.push(format!("{key}: {value}"));
    }
    out.join("\n")
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|w| key.contains(w))
}

/// Whether a path starts at byte `i` of `s` (not inside a word, a URL or another path).
fn path_at(s: &str, i: usize) -> bool {
    let b = s.as_bytes();
    if i > 0
        && (b[i - 1].is_ascii_alphanumeric()
            || matches!(b[i - 1], b'_' | b'.' | b':' | b'/' | b'\\' | b'-'))
    {
        return false;
    }
    let word = |j: usize| {
        b.get(j)
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'~' | b'-'))
    };
    match b.get(i) {
        Some(b'/') => word(i + 1),
        Some(b'~') => b.get(i + 1) == Some(&b'/'),
        Some(b'\\') => b.get(i + 1) == Some(&b'\\') && word(i + 2),
        Some(c) if c.is_ascii_alphabetic() => {
            b.get(i + 1) == Some(&b':') && matches!(b.get(i + 2), Some(b'\\' | b'/')) && word(i + 3)
        }
        _ => false,
    }
}

fn paths(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if !line.is_char_boundary(i) || !path_at(line, i) {
            let c = line[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let quote = line[..i]
            .chars()
            .next_back()
            .filter(|c| matches!(c, '"' | '\'' | '`'));
        let rest = &line[i..];
        let (end, tail) = match quote.and_then(|q| rest.find(q)) {
            Some(n) => (n, ""),
            None => {
                let n = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let word = &rest[..n];
                let kept =
                    word.trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '>', '"', '\'']);
                (kept.len(), &word[kept.len()..])
            }
        };
        out.push_str("<path>");
        out.push_str(tail);
        i += end + tail.len();
    }
    out
}

fn looks_like_token(w: &str) -> bool {
    if TOKEN_PREFIXES.iter().any(|p| w.starts_with(p)) && w.len() >= 12 {
        return true;
    }
    w.len() >= 32
        && w.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+' | b'=' | b'.'))
        && w.bytes().any(|c| c.is_ascii_digit())
        && w.bytes().any(|c| c.is_ascii_alphabetic())
}

/// The password of `scheme://user:pass@host`.
fn userinfo(word: &str) -> String {
    let Some(start) = word.find("://").map(|n| n + 3) else {
        return word.to_owned();
    };
    let host_end = word[start..].find('/').map_or(word.len(), |n| start + n);
    match word[start..host_end].rfind('@') {
        Some(at) => format!("{}<redacted>{}", &word[..start], &word[start + at..]),
        None => word.to_owned(),
    }
}

fn secrets(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut after_secret = false;
    for piece in line.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        let space = &piece[word.len()..];
        let core = word.trim_matches(|c: char| {
            matches!(c, '"' | '\'' | '(' | ')' | ',' | ';' | '[' | ']' | '{' | '}' | '<' | '>')
        });
        let at = word.find(core).unwrap_or(0);
        let (lead, trail) = (&word[..at], &word[at + core.len()..]);
        let body = match core.split_once('=') {
            _ if core.is_empty() => String::new(),
            _ if after_secret || looks_like_token(core) => "<redacted>".to_owned(),
            Some((k, v)) if is_secret_key(k) || looks_like_token(v) => format!("{k}=<redacted>"),
            _ => userinfo(core),
        };
        after_secret = SECRET_BEFORE.contains(&core.trim_end_matches([':', '=']).to_ascii_lowercase().as_str());
        out.extend([lead, &body, trail, space]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_tokens_are_taken_out_but_urls_and_fractions_stay() {
        let line = "cannot open \"/home/alice/My Music/a.mp3\": not found; also C:\\Users\\bob\\x.wav, ~/y.flac and https://github.com/OpenRsG/OpenFadeR 3/4";
        let r = redact(line);
        for leak in ["alice", "My Music", "bob", "y.flac"] {
            assert!(!r.contains(leak), "{leak} leaked: {r}");
        }
        assert!(
            r.contains("https://github.com/OpenRsG/OpenFadeR") && r.contains("3/4"),
            "{r}"
        );
        let r = redact("Authorization: Bearer abc123 url=https://me:hunter2@host/x api_key=zzz ghp_0123456789abcdefghij");
        for leak in ["abc123", "hunter2", "zzz", "ghp_0123"] {
            assert!(!r.contains(leak), "{leak} leaked: {r}");
        }
        assert!(r.contains("@host/x"), "{r}");
        assert!(
            !redact("sha 0123456789abcdef0123456789abcdef01234567").contains("0123456789abcdef")
        );
    }

    #[test]
    fn only_the_settings_that_differ_are_listed_and_secrets_and_paths_are_taken_out() {
        let default = "# header\nkey_lock = off\nlanguage = auto\n";
        let current = "# header\nkey_lock = on\nlanguage = auto\nmusic_folder = /home/alice/Music\nupdate_token = s3cr3t\n";
        let s = settings(current, default);
        assert_eq!(
            s,
            "key_lock: on\nmusic_folder: <path>\nupdate_token: <redacted>"
        );
    }
}
