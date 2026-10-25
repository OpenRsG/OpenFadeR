//! The content print of a file: its size and a hash of three slices of its bytes (the first, the middle and the last 64 KiB; the
//! whole file when it is no longer than the three together). Cheap (at most 192 KiB read, no decoding) and stable: the same bytes
//! under another name or in another folder give the same print; a changed byte inside a slice, or a changed size, gives another.
//! Bytes outside the slices are not covered: two files that differ only there share a print (the caller also compares sizes and
//! names where it matters). Where the audio itself is meant, see [`crate::analysis::fingerprint`].

use crate::analysis::cache::fnv1a;
use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

/// Bytes read per slice.
const SLICE: u64 = 64 * 1024;

/// The print of the file at `path`: `<size in hex>-<hash in hex>`.
pub fn of_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut bytes = Vec::with_capacity(3 * SLICE as usize);
    let (starts, len) = if size <= 3 * SLICE {
        (vec![0], size)
    } else {
        (vec![0, (size - SLICE) / 2, size - SLICE], SLICE)
    };
    for at in starts {
        f.seek(SeekFrom::Start(at))?;
        f.by_ref().take(len).read_to_end(&mut bytes)?;
    }
    Ok(format!("{size:x}-{:016x}", fnv1a(bytes)))
}

/// `(size, mtime in unix s)` of `path`, as [`crate::store::Store::set_file_print`] takes them.
pub fn stamp(path: &Path) -> io::Result<(i64, i64)> {
    let m = std::fs::metadata(path)?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    Ok((m.len() as i64, mtime))
}

/// Makes and stores the print of Collection row `id`'s file when it has none; errors (a file that is gone) are not worth saying.
pub fn remember(store: &crate::store::Store, id: i64, path: &Path) {
    if store.file_print(id).ok().flatten().is_some() {
        return;
    }
    if let (Ok(print), Ok((size, mtime))) = (of_file(path), stamp(path)) {
        let _ = store.set_file_print(id, &print, size, mtime);
    }
}

/// Pause between two files of [`fill_pass`]: the background fill must not compete with playback for the disk.
const PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// One pass of the background fill: prints up to `limit` Collection rows that have no print ([`crate::store::Store::unprinted`]),
/// pausing between files. Every row it handles goes into `done`, so a file that cannot be read (or whose size differs from the
/// row's) is not tried again by later passes of one run. Returns how many it printed.
pub fn fill_pass(store: &crate::store::Store, limit: usize, done: &mut HashSet<i64>) -> usize {
    let Ok(rows) = store.unprinted(limit + done.len()) else {
        return 0;
    };
    let mut printed = 0;
    let fresh: Vec<_> = rows
        .into_iter()
        .filter(|(id, _)| !done.contains(id))
        .take(limit)
        .collect();
    for (id, path) in fresh {
        done.insert(id);
        if let (Ok(print), Ok((size, mtime))) = (of_file(&path), stamp(&path)) {
            printed += store.set_file_print(id, &print, size, mtime).is_ok() as usize;
        }
        std::thread::sleep(PAUSE);
    }
    printed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_same_print_wherever_they_live_and_a_sampled_byte_changes_it() {
        let dir = std::env::temp_dir().join(format!("openfader-print-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let data: Vec<u8> = (0..400_000u32).map(|i| (i * 7 + i / 251) as u8).collect();
        let (a, b) = (dir.join("a.mp3"), dir.join("sub/renamed.mp3"));
        std::fs::write(&a, &data).unwrap();
        std::fs::write(&b, &data).unwrap();
        let print = of_file(&a).unwrap();
        assert_eq!(print, of_file(&a).unwrap(), "stable");
        assert_eq!(
            print,
            of_file(&b).unwrap(),
            "a moved or renamed copy matches"
        );
        for at in [10, 200_000, 399_990] {
            let mut d = data.clone();
            d[at] ^= 1;
            std::fs::write(&b, &d).unwrap();
            assert_ne!(print, of_file(&b).unwrap(), "byte {at} is in a slice");
        }
        std::fs::write(&b, &data[..data.len() - 1]).unwrap();
        assert_ne!(print, of_file(&b).unwrap(), "another size");
        // A small file is hashed whole; an empty one has a print too.
        std::fs::write(&b, b"abc").unwrap();
        assert!(of_file(&b).unwrap().starts_with("3-"));
        std::fs::write(&b, b"").unwrap();
        assert!(of_file(&b).unwrap().starts_with("0-"));
        assert!(of_file(&dir.join("gone")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_fill_pass_prints_the_rows_without_a_print_and_tries_a_bad_file_once() {
        let dir = std::env::temp_dir().join(format!("openfader-fill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::store::Store::in_memory().unwrap();
        let row = |name: &str| {
            let file = dir.join(name);
            let t = crate::library::Track {
                file: Some(file.clone()),
                ..Default::default()
            };
            (store.collection_upsert(&t, None).unwrap(), file)
        };
        let ((a, fa), (b, fb), (c, _)) = (row("a.mp3"), row("b.mp3"), row("gone.mp3"));
        std::fs::write(&fa, b"audio a").unwrap();
        std::fs::write(&fb, b"audio b").unwrap();
        store.set_file_print(b, "kept", 0, 0).unwrap();
        let mut done = HashSet::new();
        assert_eq!(fill_pass(&store, 10, &mut done), 1);
        assert_eq!(
            store.file_print(a).unwrap(),
            Some(of_file(&fa).unwrap()),
            "printed"
        );
        assert_eq!(store.file_print(b).unwrap().as_deref(), Some("kept"));
        assert_eq!(store.file_print(c).unwrap(), None, "unreadable: no print");
        assert_eq!(
            fill_pass(&store, 10, &mut done),
            0,
            "nothing is tried twice"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
