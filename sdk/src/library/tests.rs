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
