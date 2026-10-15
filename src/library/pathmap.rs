//! `OPENFADER_PATH_MAP`: rewriting path prefixes written on another OS (a library from Windows opened on Linux).

/// `from => to` path-prefix pairs (see [`parse_path_map`]).
pub type PathMap = Vec<(String, String)>;

/// Parses `C:/Users/x/Music=>/home/y/Music;D:\Tracks=>/mnt/t`: `;`-separated `from=>to` pairs,
/// `from` with backslashes read as `/`, trailing slashes dropped. Empty entries are skipped.
pub fn parse_path_map(spec: &str) -> Result<PathMap, String> {
    let pair = |e: &str| {
        let bad = || format!("path map entry '{e}' is not <from>=><to>");
        let (from, to) = e.split_once("=>").ok_or_else(bad)?;
        let from = from
            .trim()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_owned();
        (!from.is_empty())
            .then(|| (from, to.trim().trim_end_matches('/').to_owned()))
            .ok_or_else(bad)
    };
    spec.split(';')
        .filter(|e| !e.trim().is_empty())
        .map(pair)
        .collect()
}

/// `OPENFADER_PATH_MAP`, parsed; unset = no remapping.
pub(super) fn path_map_from_env() -> Result<PathMap, String> {
    std::env::var("OPENFADER_PATH_MAP").map_or_else(|_| Ok(Vec::new()), |s| parse_path_map(&s))
}

/// With an empty map `path` is returned as is. Otherwise backslashes become `/` and the first
/// pair whose `from` is a whole-component prefix of the path (case-sensitive) is replaced by its
/// `to`; a path no pair matches is returned slash-normalised.
pub fn remap_path(path: &str, map: &PathMap) -> String {
    if map.is_empty() {
        return path.to_owned();
    }
    let p = path.replace('\\', "/");
    for (from, to) in map {
        if let Some(rest) = p
            .strip_prefix(from.as_str())
            .filter(|r| r.is_empty() || r.starts_with('/'))
        {
            return format!("{to}{rest}");
        }
    }
    p
}

/// `file://localhost/C:/Music/a%20b.mp3` → `C:/Music/a b.mp3`, then through `map`; `None` for an empty path.
pub fn location(url: &str, map: &PathMap) -> Option<std::path::PathBuf> {
    let p = url
        .strip_prefix("file://localhost")
        .or_else(|| url.strip_prefix("file://"))
        .unwrap_or(url);
    let b = p.as_bytes();
    let mut raw = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (
            b[i],
            p.get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok()),
        ) {
            (b'%', Some(byte)) => {
                raw.push(byte);
                i += 3;
            }
            (c, _) => {
                raw.push(c);
                i += 1;
            }
        }
    }
    let p = String::from_utf8_lossy(&raw);
    // A drive path follows the URI's slash: `/C:/Music` → `C:/Music`.
    let drive = matches!(p.as_bytes(), [b'/', d, b':', ..] if d.is_ascii_alphabetic());
    let p = if drive { &p[1..] } else { &p[..] };
    (!p.is_empty()).then(|| std::path::PathBuf::from(remap_path(p, map)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_map_remaps_whole_components_and_normalises_backslashes() {
        let map =
            parse_path_map(r"C:\Users\x\Music=>/home/y/Music/ ; ;D:/Tracks => /mnt/t").unwrap();
        assert_eq!(
            map,
            [
                ("C:/Users/x/Music".to_owned(), "/home/y/Music".to_owned()),
                ("D:/Tracks".to_owned(), "/mnt/t".to_owned())
            ]
        );
        assert_eq!(
            remap_path(r"C:\Users\x\Music\Pack 1\a.mp3", &map),
            "/home/y/Music/Pack 1/a.mp3"
        );
        assert_eq!(remap_path("D:/Tracks/b.mp3", &map), "/mnt/t/b.mp3");
        assert_eq!(
            remap_path("C:/Users/x/Music2/c.mp3", &map),
            "C:/Users/x/Music2/c.mp3"
        ); // not a component prefix
        assert_eq!(remap_path(r"E:\Other\d.mp3", &map), "E:/Other/d.mp3"); // unmatched: only slashes normalised
        assert_eq!(
            remap_path(r"/home/me/a\b.mp3", &Vec::new()),
            r"/home/me/a\b.mp3"
        ); // no map: untouched
        assert!(parse_path_map("no arrow").is_err());
        assert!(parse_path_map("=>/x").is_err());
        assert!(parse_path_map("").unwrap().is_empty());
    }
}
