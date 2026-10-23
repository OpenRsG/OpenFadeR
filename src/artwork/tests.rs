use super::*;
use crate::plugin::{discover, Registry};
use std::fs;

/// A 1×1 PNG, base64.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rk-artwork-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// The plugin `name` in `dir` whose `sh` script is `script`; `artwork` says whether it answers artwork requests.
fn mock(dir: &Path, name: &str, artwork: bool, script: &str) -> Registry {
    let p = dir.join(name);
    fs::create_dir_all(&p).unwrap();
    let manifest = json!({"name": name, "command": ["sh", "run.sh"], "artwork": artwork});
    fs::write(p.join("plugin.json"), manifest.to_string()).unwrap();
    fs::write(p.join("run.sh"), script).unwrap();
    let r = discover(&[p]);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    r
}

/// A plugin that answers every request with `reply`, keeps the last request in `<dir>/<name>.req` and counts the requests
/// ([`calls`]).
fn answering(dir: &Path, name: &str, reply: &str) -> Registry {
    let (req, calls) = (
        dir.join(format!("{name}.req")),
        dir.join(format!("{name}.calls")),
    );
    let script = format!(
        "cat > '{}'\necho x >> '{}'\ncat <<'EOF'\n{reply}\nEOF\n",
        req.display(),
        calls.display()
    );
    mock(dir, name, true, &script)
}

fn calls(dir: &Path, name: &str) -> usize {
    fs::read_to_string(dir.join(format!("{name}.calls"))).map_or(0, |s| s.lines().count())
}

fn track(id: i64, artist: &str, album: &str) -> Track {
    Track {
        id: Some(id),
        artist: artist.into(),
        album: album.into(),
        title: format!("song {id}"),
        length_s: 215,
        ..Track::default()
    }
}

fn instant() -> Gate {
    Gate::new(Duration::ZERO)
}

fn url_of(c: &Candidate) -> &str {
    match &c.art {
        Art::Url(u) => u,
        Art::Bytes(_) => "bytes",
    }
}

#[test]
fn candidates_parse_urls_bytes_sizes_and_sources_and_skip_what_cannot_be_used() {
    let reply = json!({"candidates": [
        {"url": "https://x.test/a.jpg", "thumb": "https://x.test/a-s.jpg", "size": 1200, "source": "Archive"},
        {"data": PNG},
        {"url": "file:///etc/passwd"},
        {"url": "ftp://x.test/a.jpg"},
        {"size": 500},
        {"data": "***"},
        {"url": "http://x.test/b.png", "size": "big", "thumb": "javascript:1"},
    ]});
    let c = Candidate::parse("p", &reply);
    assert_eq!(c.len(), 3);
    assert_eq!(url_of(&c[0]), "https://x.test/a.jpg");
    assert_eq!((c[0].size, c[0].source.as_str()), (1200, "Archive"));
    assert_eq!(c[0].thumb.as_deref(), Some("https://x.test/a-s.jpg"));
    assert!(matches!(&c[1].art, Art::Bytes(b) if image::kind(b).is_some()));
    assert_eq!(
        (c[1].source.as_str(), c[1].size),
        ("p", 0),
        "the plugin's name stands in for a source"
    );
    assert_eq!((c[2].size, c[2].thumb.clone()), (0, None));
    assert!(Candidate::parse("p", &json!({})).is_empty());
    let many = json!({"candidates": (0..20).map(|i| json!({"url": format!("https://x.test/{i}")})).collect::<Vec<_>>()});
    assert_eq!(Candidate::parse("p", &many).len(), PER_PLUGIN);
}

#[test]
fn every_artwork_plugin_is_asked_in_turn_with_the_tags_and_a_failure_does_not_stop_the_others() {
    let d = temp("search");
    let one = r#"{"candidates": [{"url": "https://a.test/1.jpg", "size": 600}]}"#;
    let two =
        r#"{"candidates": [{"url": "https://b.test/1.jpg"}, {"url": "https://b.test/2.jpg"}]}"#;
    let regs = [
        answering(&d, "a", one),
        mock(&d, "broken", true, "cat > /dev/null\nexit 3\n"),
        answering(&d, "b", two),
        // Not an artwork plugin: never asked.
        mock(&d, "other", false, "echo x >> other.calls\necho '{}'\n"),
    ];
    let plugins: Vec<&Plugin> = regs.iter().flat_map(|r| &r.plugins).collect();
    let q = Query::of(&track(1, " Daft Punk ", "Discovery"));
    let found = search(plugins, &q, &instant());
    assert_eq!(found.asked, 3);
    assert_eq!(found.errors.len(), 1, "{:?}", found.errors);
    assert!(found.errors[0].contains("broken"));
    assert_eq!(
        found.candidates.iter().map(url_of).collect::<Vec<_>>(),
        [
            "https://a.test/1.jpg",
            "https://b.test/1.jpg",
            "https://b.test/2.jpg"
        ],
        "plugin order, then each plugin's own order"
    );
    let asked: Value = serde_json::from_str(&fs::read_to_string(d.join("a.req")).unwrap()).unwrap();
    assert_eq!(
        (
            &asked["op"],
            &asked["artist"],
            &asked["album"],
            &asked["title"],
            &asked["duration"]
        ),
        (
            &json!("artwork"),
            &json!("Daft Punk"),
            &json!("Discovery"),
            &json!("song 1"),
            &json!(215)
        )
    );
    assert_eq!(calls(&d, "other"), 0);
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn the_gate_keeps_its_gap_between_requests_to_one_plugin_only() {
    let gate = Gate::new(Duration::from_millis(150));
    let start = Instant::now();
    gate.wait("a");
    gate.wait("b");
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "the first request of each plugin goes at once"
    );
    gate.wait("a");
    assert!(start.elapsed() >= Duration::from_millis(150));
    gate.wait("a");
    assert!(start.elapsed() >= Duration::from_millis(300));
}

#[test]
fn a_saved_cover_is_stored_for_the_album_shows_on_its_other_tracks_and_undo_brings_the_old_one_back(
) {
    let d = temp("save");
    let store = Store::in_memory().unwrap();
    let (one, two, other) = (
        track(1, "Daft Punk", "Discovery"),
        track(2, "daft punk", "DISCOVERY"),
        track(3, "Daft Punk", "Homework"),
    );
    assert_eq!(resolve(&store, &one), None);
    let png = image::base64(PNG).unwrap();
    let path = save(&store, &d, &one, &png).unwrap();
    assert_eq!(fs::read(&path).unwrap(), png);
    assert_eq!(path.parent(), Some(d.as_path()));
    assert_eq!(resolve(&store, &one), Some(path.clone()));
    assert_eq!(
        resolve(&store, &two),
        Some(path.clone()),
        "the album has one cover"
    );
    assert_eq!(resolve(&store, &other), None);
    // Another picture replaces it; the first stays on disk for undo.
    let again = save(&store, &d, &two, b"GIF89a....").unwrap();
    assert_ne!(again, path);
    assert_eq!(resolve(&store, &one), Some(again));
    assert_eq!(store.undo().unwrap().as_deref(), Some("Cover art"));
    assert_eq!(resolve(&store, &one), Some(path));
    assert!(save(&store, &d, &one, b"<html>").is_err());
    // The library's own artwork comes after the kept cover, and only files that exist count.
    let listed = d.join("listed.png");
    fs::write(&listed, &png).unwrap();
    let with_art = Track {
        artwork: Some(listed.clone()),
        ..other.clone()
    };
    assert_eq!(resolve(&store, &with_art), Some(listed));
    let gone = Track {
        artwork: Some(d.join("gone.png")),
        ..other
    };
    assert_eq!(resolve(&store, &gone), None);
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn a_folder_image_shows_for_a_track_without_a_cover_and_a_chosen_cover_beats_it() {
    let d = temp("folder");
    let store = Store::in_memory().unwrap();
    fs::write(d.join("cover.jpg"), [0xff, 0xd8, 0xff]).unwrap();
    let t = Track {
        file: Some(d.join("a.mp3")),
        ..track(1, "A", "B")
    };
    assert_eq!(resolve(&store, &t), Some(d.join("cover.jpg")));
    let kept = save(&store, &d.join("kept"), &t, &image::base64(PNG).unwrap()).unwrap();
    assert_eq!(resolve(&store, &t), Some(kept));
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn fetching_missing_covers_asks_once_per_album_keeps_the_first_picture_and_remembers_a_miss() {
    let d = temp("missing");
    let store = Store::in_memory().unwrap();
    let hit = answering(
        &d,
        "hit",
        &json!({"candidates": [{"data": PNG}]}).to_string(),
    );
    let nothing = answering(&d, "nothing", r#"{"candidates": []}"#);
    let tracks = [
        track(1, "Daft Punk", "Discovery"),
        track(2, "Daft Punk", "Discovery"),
        track(3, "Orbital", "Brown Album"),
        track(4, "Nobody", ""),
    ];
    let cache = d.join("cache");
    let gate = instant();
    let fetch = |plugins: Vec<Plugin>| Fetch {
        store: &store,
        plugins,
        gate: &gate,
        dir: &cache,
    };
    let run = |f: &Fetch, tracks: &[Track]| {
        let mut embedded = Vec::new();
        let out = f.missing(tracks, &mut |_, _| true, &mut |t, b| {
            embedded.push((t.id, b.len()))
        });
        (out, embedded)
    };

    // The plugin has a picture: the album is asked once, and both its tracks are handed to `embed`.
    let f = fetch(hit.artwork().cloned().collect());
    let (out, embedded) = run(&f, &tracks[..2]);
    assert_eq!(
        (out.found, out.missed, out.skipped, out.failed),
        (1, 0, 0, 0),
        "{out:?}"
    );
    assert_eq!(calls(&d, "hit"), 1);
    assert_eq!(
        embedded.iter().map(|e| e.0).collect::<Vec<_>>(),
        [Some(1), Some(2)]
    );
    assert!(resolve(&store, &tracks[1]).is_some());
    // An album that shows a picture is not asked about again.
    let (out, _) = run(&f, &tracks[..2]);
    assert_eq!((out.found, out.skipped), (0, 1));
    assert_eq!(calls(&d, "hit"), 1);

    // Nothing offers a picture: remembered, so the plugin is not asked again.
    let f = fetch(nothing.artwork().cloned().collect());
    let (out, _) = run(&f, &tracks[2..]);
    assert_eq!((out.found, out.missed, out.skipped), (0, 2, 0), "{out:?}");
    assert_eq!(calls(&d, "nothing"), 2);
    let (out, _) = run(&f, &tracks[2..]);
    assert_eq!((out.missed, out.skipped), (0, 2), "{out:?}");
    assert_eq!(calls(&d, "nothing"), 2, "the miss was remembered");
    assert!(store
        .artwork_missed(&store.artwork_key(&tracks[2]))
        .unwrap());
    assert_eq!(resolve(&store, &tracks[2]), None);

    // A plugin that fails (no network) is no miss, nor is having no plugin: the next run asks again.
    let broken = mock(
        &d,
        "broken",
        true,
        "cat > /dev/null\necho '{\"error\": \"offline\"}'\n",
    );
    let fresh = [track(9, "Fresh", "Album")];
    let key = store.artwork_key(&fresh[0]);
    let (out, _) = run(&fetch(broken.artwork().cloned().collect()), &fresh);
    assert_eq!((out.failed, out.missed), (1, 0));
    assert!(out.error.unwrap().contains("offline"));
    assert!(!store.artwork_missed(&key).unwrap());
    let (out, _) = run(&fetch(Vec::new()), &fresh);
    assert_eq!((out.failed, out.missed), (1, 0));
    assert!(!store.artwork_missed(&key).unwrap());

    // The run can be stopped between two lookups.
    let more = [track(20, "A", "One"), track(21, "B", "Two")];
    let before = calls(&d, "hit");
    let out =
        fetch(hit.artwork().cloned().collect()).missing(&more, &mut |done, _| done < 1, &mut |_, _| {});
    assert_eq!(out.found, 1);
    assert_eq!(calls(&d, "hit"), before + 1);
    fs::remove_dir_all(d).unwrap();
}
