//! Folder library on generated WAVs only.
mod common;

use openfader::library::open;
use std::path::Path;

fn wav(path: &Path, info_title: Option<&str>) {
    let data = vec![0u8; 8000]; // 1 s, 8 kHz mono 8-bit
    let mut list = Vec::new();
    if let Some(t) = info_title {
        let mut s = t.as_bytes().to_vec();
        s.push(0);
        if s.len() % 2 == 1 {
            s.push(0);
        }
        list.extend(b"LIST");
        list.extend((4 + 8 + s.len() as u32).to_le_bytes());
        list.extend(b"INFO");
        list.extend(b"INAM");
        list.extend((s.len() as u32).to_le_bytes());
        list.extend(s);
    }
    let mut b = Vec::new();
    b.extend(b"RIFF");
    b.extend((4 + 24 + list.len() as u32 + 8 + data.len() as u32).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend([1, 0, 1, 0]);
    b.extend(8000u32.to_le_bytes());
    b.extend(8000u32.to_le_bytes());
    b.extend([1, 0, 8, 0]);
    b.extend(list);
    b.extend(b"data");
    b.extend((data.len() as u32).to_le_bytes());
    b.extend(data);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b).unwrap();
}

#[test]
fn scans_nested_folders() {
    let root = common::fresh_dir("folder");
    wav(&root.join("top.wav"), None);
    wav(&root.join("House/a.wav"), None);
    wav(&root.join("House/Deep/b.wav"), Some("Tagged Title"));
    wav(&root.join("Techno/c.wav"), None);
    std::fs::write(root.join("Techno/notes.txt"), "x").unwrap();
    std::fs::create_dir_all(root.join("Empty")).unwrap();

    let lib = open(&root).unwrap();
    assert_eq!(lib.kind(), "Folder");
    let all = lib.tracks(None).unwrap();
    assert_eq!(
        all.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        ["Tagged Title", "a", "c", "top"]
    );
    assert_eq!(all[0].length_s, 1);

    let tree: Vec<_> = lib
        .playlists()
        .unwrap()
        .iter()
        .map(|p| (p.name.clone(), p.depth, p.is_folder))
        .collect();
    let want = [("House", 0, true), ("Deep", 1, false), ("Techno", 0, false)]
        .map(|(n, d, f)| (n.to_string(), d, f));
    assert_eq!(tree, want);

    let house = lib.tracks(Some("House")).unwrap();
    assert_eq!(house.len(), 2);
    assert_eq!(lib.tracks(Some("House/Deep")).unwrap().len(), 1);
    assert!(lib.analysis(&all[0]).is_none());
    std::fs::remove_dir_all(&root).unwrap();
}
