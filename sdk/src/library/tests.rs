use super::*;

struct Fake;
struct Open;

impl Source for Open {
    fn kind(&self) -> String {
        "fake".into()
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(vec![Playlist {
            id: "p".into(),
            name: "P".into(),
            depth: 0,
            is_folder: false,
        }])
    }
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        if playlist == Some("boom") {
            panic!("boom");
        }
        Ok(vec![Track {
            title: playlist.unwrap_or("all").into(),
            ..Track::default()
        }])
    }
}

impl LibraryPlugin for Fake {
    fn probe(path: &Path) -> Option<Probe> {
        path.ends_with("x.fake").then(|| Probe {
            score: 80,
            kind: "fake".into(),
        })
    }
    fn open(_: &Path, _: &Value) -> Result<Box<dyn Source>, String> {
        Ok(Box::new(Open))
    }
}

/// One request through the C ABI functions, the way the host calls them.
fn call(req: Value) -> Value {
    let body = req.to_string();
    let buf = unsafe { call_c::<Fake>(body.as_ptr(), body.len(), std::ptr::null()) };
    let v =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }).unwrap();
    unsafe { free_c(buf) };
    v
}

#[test]
fn the_api_table_is_the_one_the_entry_point_returns() {
    static API: LibraryApi = api::<Fake>();
    let body = br#"{"op": "probe", "path": "/a/x.fake"}"#;
    let buf = unsafe { (API.call)(body.as_ptr(), body.len(), std::ptr::null()) };
    let v: Value =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }).unwrap();
    unsafe { (API.free)(buf) };
    assert_eq!((API.abi, v["score"].clone()), (LIBRARY_ABI, json!(80)));
}

#[test]
fn ops_round_trip_and_failures_stay_answers() {
    assert_eq!(
        call(json!({"op": "probe", "path": "/a/x.fake"}))["score"],
        80
    );
    assert_eq!(call(json!({"op": "probe", "path": "/a/y"}))["score"], 0);
    let h = call(json!({"op": "open", "path": "/a/x.fake", "settings": {}}))["handle"]
        .as_u64()
        .unwrap();
    assert_eq!(
        call(json!({"op": "playlists", "handle": h}))["playlists"][0]["id"],
        "p"
    );
    assert_eq!(
        call(json!({"op": "tracks", "handle": h, "playlist": null}))["tracks"][0]["title"],
        "all"
    );
    assert_eq!(
        call(json!({"op": "search", "handle": h, "playlist": "p", "query": "q"}))["started"],
        false
    );
    // A panic in the plugin, an unknown op and a closed handle are `error` answers, not crashes.
    assert!(call(json!({"op": "tracks", "handle": h, "playlist": "boom"}))["error"].is_string());
    assert!(call(json!({"op": "export", "handle": h}))["error"].is_string());
    assert!(
        call(json!({"op": "write_collection", "path": "/x", "snapshot": {}}))["error"].is_string()
    );
    call(json!({"op": "close", "handle": h}));
    assert!(call(json!({"op": "playlists", "handle": h}))["error"].is_string());
    assert!(call(json!("not an object"))["error"].is_string());
}

#[test]
fn beat_numbers_map_onto_the_grid_and_extend_past_it() {
    let beats: Vec<_> = (0..4)
        .map(|i| Beat {
            bar_pos: i as u16 % 4 + 1,
            bpm_x100: 12000,
            time_ms: 100 + i * 500,
        })
        .collect();
    let p = Phrase {
        beat: 2,
        end_beat: 6,
        kind: Kind::Verse,
        variant: 0,
        fill: None,
    };
    // beat 6 = two beats past the last (index 3 → 5): 1600 + 2·500
    assert_eq!(p.span_ms(&beats), (600.0, 2600.0));
    assert_eq!(Phrase { beat: 3, ..p }.span_ms(&[]), (0.0, 0.0));
}

#[test]
fn device_names_stay_fat_safe_and_keep_their_extension() {
    assert_eq!(clean_name("AC/DC: Live?.mp3", "x"), "AC_DC_ Live_.mp3");
    assert_eq!(clean_name("  ...  ", "track"), "track");
    let long = format!("{}.flac", "é".repeat(200));
    let short = clean_name(&long, "x");
    assert_eq!(short.chars().count(), 120);
    assert!(short.ends_with(".flac"));
    // A dot inside a long title is no extension: the name is just cut.
    let titled = format!("Mr. {}", "a".repeat(200));
    assert_eq!(clean_name(&titled, "x").chars().count(), 120);
}

#[test]
fn an_export_cut_off_resumes_with_the_files_that_are_complete() {
    let dir = std::env::temp_dir().join(format!("of-place-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (src, stick) = (dir.join("src"), dir.join("stick"));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&stick).unwrap();
    std::fs::write(src.join("a.mp3"), vec![7u8; 2_500_000]).unwrap();
    let track = Track {
        file: Some(src.join("a.mp3")),
        artist: "A".into(),
        album: "B".into(),
        ..Track::default()
    };
    let folder = stick.join("Contents/A/B");
    std::fs::create_dir_all(&folder).unwrap();
    // What a run cut off by a lost device left: a half file under the part name, and the part of a track no longer exported.
    std::fs::write(folder.join("a.part"), b"half").unwrap();
    std::fs::write(folder.join("gone.part"), b"old").unwrap();
    assert_eq!(
        place_audio(&stick, &track).unwrap(),
        ("Contents/A/B/a.mp3".into(), true)
    );
    assert_eq!(
        std::fs::read(folder.join("a.mp3")).unwrap().len(),
        2_500_000
    );
    assert!(!folder.join("a.part").exists() && !folder.join("gone.part").exists());
    // The run again: the complete file is there, nothing is copied.
    let before = std::fs::metadata(folder.join("a.mp3"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        place_audio(&stick, &track).unwrap(),
        ("Contents/A/B/a.mp3".into(), false)
    );
    assert_eq!(
        std::fs::metadata(folder.join("a.mp3"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    // A file of that name with another size is another track: this one goes beside it.
    std::fs::write(src.join("a.mp3"), vec![7u8; 100]).unwrap();
    assert_eq!(
        place_audio(&stick, &track).unwrap(),
        ("Contents/A/B/a (2).mp3".into(), true)
    );
    assert_eq!(
        std::fs::read(folder.join("a.mp3")).unwrap().len(),
        2_500_000
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_input_output_error_is_tried_once_more_and_nothing_else_is() {
    let none = std::time::Duration::ZERO;
    let eio = || Err::<u8, _>("/x/a.part: Input/output error (os error 5)".to_owned());
    let mut calls = 0;
    assert_eq!(
        retry_eio(none, || {
            calls += 1;
            if calls == 1 { eio() } else { Ok(9) }
        }),
        Ok(9)
    );
    calls = 0;
    assert!(
        retry_eio(none, || {
            calls += 1;
            eio()
        })
        .is_err()
    );
    assert_eq!(calls, 2, "a second failure is the answer");
    calls = 0;
    assert!(
        retry_eio(none, || {
            calls += 1;
            Err::<u8, _>("No space left on device (os error 28)".to_owned())
        })
        .is_err()
    );
    assert_eq!(calls, 1);
}
