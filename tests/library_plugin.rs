//! The library plugins through the host: the bundled plugins (built by `extension/plugin/build.sh`; the test skips when
//! they are not) are probed for a path, answer playlists, tracks and analysis over the C ABI, write the Collection back as
//! a file that opens again, and list a library of their own setting in the Media Browser.

use openfader::library::{self, snapshot::Snapshot, Library};
use openfader::plugin;
use std::path::Path;

const COLLECTION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS Version="1.0.0">
  <COLLECTION Entries="2">
    <TRACK TrackID="1" Name="Zed" Artist="A" TotalTime="10" AverageBpm="120.00" Rating="255" Location="file://localhost/tmp/zed.mp3">
      <TEMPO Inizio="0.000" Bpm="120.00" Metro="4/4" Battito="1"/>
      <POSITION_MARK Name="drop" Type="0" Start="2.000" Num="0" Red="1" Green="2" Blue="3"/>
    </TRACK>
    <TRACK TrackID="2" Name="Alpha" Artist="B" TotalTime="5" AverageBpm="90.00" Rating="0" Location="file://localhost/tmp/alpha.mp3"/>
  </COLLECTION>
  <PLAYLISTS><NODE Type="0" Name="ROOT" Count="1"><NODE Type="1" Name="Set" KeyType="0" Entries="2"><TRACK Key="2"/><TRACK Key="1"/></NODE></NODE></PLAYLISTS>
</DJ_PLAYLISTS>"#;

const ITUNES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Tracks</key><dict>
<key>1</key><dict><key>Track ID</key><integer>1</integer><key>Name</key><string>Tune</string>
<key>Location</key><string>file://localhost/tmp/tune.mp3</string></dict></dict>
<key>Playlists</key><array></array></dict></plist>"#;

fn titles(l: &dyn Library, playlist: Option<&str>) -> Vec<String> {
    l.tracks(playlist)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect()
}

#[test]
fn bundled_plugins_read_and_write_over_the_abi() {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("extension/plugin");
    // Every bundled library plugin must be built (extension/plugin/build.sh), else the test has nothing to read with; without the
    // plugins checkout (start.sh fetches it) there is nothing to test.
    let Ok(entries) = std::fs::read_dir(&plugins) else {
        eprintln!("skipped: no extension/plugin checkout");
        return;
    };
    let unbuilt = entries.flatten().any(|e| {
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(e.path().join("plugin.json")).unwrap_or_default(),
        )
        .unwrap_or_default();
        manifest["library"]["lib"][std::env::consts::OS]
            .as_str()
            .is_some_and(|lib| !e.path().join(lib).is_file())
    });
    if unbuilt {
        eprintln!("skipped: run extension/plugin/build.sh first");
        return;
    }
    let dir = std::env::temp_dir().join(format!("openfader-libplugin-{}", std::process::id()));
    let plist = dir.join("Library.xml");
    std::fs::create_dir_all(dir.join("config/openfader")).unwrap();
    std::fs::write(&plist, ITUNES).unwrap();
    std::fs::write(
        dir.join("config/openfader/plugins.json"),
        serde_json::json!({"settings": {"itunes": {"file": plist}}}).to_string(),
    )
    .unwrap();
    // SAFETY: the only test of this binary; set before the plugin registry and its settings are first read.
    unsafe {
        std::env::set_var("OPENFADER_PLUGINS", &plugins);
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
        std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
    }

    let xml = dir.join("collection.xml");
    std::fs::write(&xml, COLLECTION).unwrap();
    let lib = library::open(&xml).unwrap();
    assert!(!lib.kind().is_empty());
    assert_eq!(titles(&*lib, None), ["Alpha", "Zed"]);
    let pls = lib.playlists().unwrap();
    assert_eq!(pls.len(), 1);
    assert_eq!(titles(&*lib, Some(&pls[0].id)), ["Alpha", "Zed"]);
    let zed = lib.tracks(None).unwrap().remove(1);
    assert_eq!(zed.file.as_deref(), Some(Path::new("/tmp/zed.mp3")));
    let a = lib.analysis(&zed).expect("grid and cues");
    assert_eq!((a.beats.len(), a.cues.len()), (20, 1));
    assert_eq!(a.cues[0].color, Some([1, 2, 3]));

    // The Collection written by the plugin opens again with the same content, and is never overwritten.
    let writer = plugin::library::writers()
        .into_iter()
        .next()
        .expect("a collection writer");
    let snap = Snapshot::take(&*lib).unwrap().to_wire();
    let out = dir.join("out.xml");
    assert_eq!(
        plugin::library::write_collection(&writer.plugin, &out, &snap).unwrap(),
        2
    );
    let again = library::open(&out).unwrap();
    assert_eq!(titles(&*again, None), ["Alpha", "Zed"]);
    assert_eq!(again.analysis(&zed).unwrap().beats.len(), 20);
    assert!(plugin::library::write_collection(&writer.plugin, &out, &snap).is_err());

    // iTunes is told apart by content, and its `file` setting lists it in the Media Browser.
    let it = library::open(&plist).unwrap();
    assert_eq!(
        (it.kind(), titles(&*it, None)),
        ("iTunes", vec!["Tune".to_owned()])
    );
    assert!(plugin::library::sources()
        .iter()
        .any(|s| s.plugin == "itunes" && s.path == plist));

    // Nothing claims a text file; the error says so.
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, "hello").unwrap();
    assert!(library::open(&txt)
        .err()
        .unwrap()
        .contains("no library plugin"));
    // Windows: the store this process opened in `dir` stays open until the process ends.
    let _ = std::fs::remove_dir_all(&dir);
}
