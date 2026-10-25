//! Lists what a device holds, through the plugin's readers: `readback <device root> [onelibrary key]` prints the Device
//! Library (export.pdb) tracks and playlists and, when `exportLibrary.db` exists and a key is given (else the content of
//! `.local/onelibrary-key`), the OneLibrary ones. Read-only.
use openfader_plugin::library::{LibraryPlugin, Source};
use rekordbox_device::Device;
use serde_json::json;
use std::path::{Path, PathBuf};

fn show(name: &str, lib: &dyn Source) {
    let tracks = lib.tracks(None).unwrap();
    println!("{name}: {} tracks", tracks.len());
    for t in &tracks {
        println!(
            "  {} | bpm {} | key {} | {:?}",
            t.title,
            t.bpm_text(),
            t.key,
            t.file
        );
    }
    for p in lib.playlists().unwrap() {
        println!("  playlist {}{} ({})", " ".repeat(p.depth), p.name, p.id);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("device root"));
    let key = args.next().or_else(|| {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/onelibrary-key");
        std::fs::read_to_string(p).ok().map(|k| k.trim().to_owned())
    });
    if root.join("PIONEER/rekordbox/export.pdb").is_file() {
        show("export.pdb", &*Device::open(&root, &json!({})).unwrap());
    }
    if let (true, Some(key)) = (
        root.join("PIONEER/rekordbox/exportLibrary.db").is_file(),
        key,
    ) {
        show(
            "exportLibrary.db",
            &*Device::open(&root, &json!({ "onelibrary_key": key })).unwrap(),
        );
    }
}
