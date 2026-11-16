//! Helpers the tests of library plugins share: the translation check and file-tree snapshots.

/// Panics unless the host would show `text` in every language of the folder `locales` (the rule of the host's `locale.rs`: the
/// text itself is a key, or matches a key whose `{}` stand for values of at least one character).
pub fn assert_translated(locales: &str, text: &str) {
    fn matches(key: &str, text: &str) -> bool {
        let parts: Vec<&str> = key.split("{}").collect();
        let (first, rest) = parts.split_first().unwrap();
        let Some((last, middle)) = rest.split_last() else {
            return false;
        };
        let Some(mut s) = text.strip_prefix(first).and_then(|t| t.strip_suffix(last)) else {
            return false;
        };
        for part in middle {
            let start = s.char_indices().nth(1).map_or(s.len(), |(i, _)| i);
            match s.get(start..).and_then(|r| r.find(part)) {
                Some(i) => s = &s[start + i + part.len()..],
                None => return false,
            }
        }
        !s.is_empty()
    }
    for lang in ["de", "es", "fr", "it", "nl", "pt"] {
        let file = format!("{locales}/{lang}.json");
        let map: std::collections::HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let found = map.get(text).is_some_and(|t| !t.is_empty())
            || map
                .iter()
                .any(|(k, v)| !v.is_empty() && k.contains("{}") && matches(k, text));
        assert!(found, "{lang} has no translation of: {text}");
    }
}

/// Every file and folder below `root` (folders end in `/`) with the bytes of the files; empty when `root` does not exist.
pub fn tree(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(
        base: &std::path::Path,
        dir: &std::path::Path,
        out: &mut std::collections::BTreeMap<String, Vec<u8>>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

pub fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}
