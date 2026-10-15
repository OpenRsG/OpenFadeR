use super::fetch::file_name;
use super::reply::{Listed, MAX_DEPTH};
use super::*;
use crate::library::Remote;
use crate::plugin::Registry;
use crate::plugin::tests::{registry_of, temp};
use std::{
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

/// A library of fixed playlists: ids "a", "history:" (the Histories folder) and "history:x".
struct Fixed;

impl Library for Fixed {
    fn kind(&self) -> &str {
        "fixed"
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        let p = |id: &str, depth, is_folder| Playlist {
            id: id.into(),
            name: id.into(),
            depth,
            is_folder,
        };
        Ok(vec![
            p("a", 0, false),
            p("history:", 0, true),
            p("history:x", 1, false),
        ])
    }
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
        Ok(vec![Track {
            title: playlist.unwrap_or("all").into(),
            ..Track::default()
        }])
    }
}

/// The plugin `p` with the sources `yt` (searchable) and `two`, answering with `script`, over [`Fixed`]; every notify
/// arrives on the channel.
fn sources(dir: &str, script: &str) -> (Sources, Receiver<Result<(), String>>) {
    let reg = registry_of(
        &temp(dir),
        "p",
        json!({"sources": [{"id": "yt", "name": "YouTube", "search": true}, {"id": "two", "name": "Two"}]}),
        script,
    );
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let notify: Notify = Arc::new(move |r| {
        let _ = tx.lock().send(r);
    });
    (Sources::new(Box::new(Fixed), reg.plugins, notify), rx)
}

fn ids(s: &Sources) -> Vec<String> {
    s.playlists().unwrap().into_iter().map(|p| p.id).collect()
}

fn answered(rx: &Receiver<Result<(), String>>) -> Result<(), String> {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("the plugin answered")
}

/// Answers `playlists`, `tracks` and `search` requests with canned JSON.
const CANNED: &str = r#"read line
case "$line" in
*'"op":"playlists"'*) printf '{"playlists":[{"id":"f","name":"F","folder":true},{"id":"l","name":"L","parent":"f"},{"id":"o","name":"Orphan","parent":"gone"}]}' ;;
*'"op":"tracks"'*) printf '{"tracks":[{"id":"t1","title":"One","artist":"A","bpm":128.5,"length":200.4,"key":"8A","url":"http://x"},{"title":"no id, no file"},{"file":"/m/two.mp3","title":"Two"}]}' ;;
esac"#;

#[test]
fn source_folders_come_before_the_histories_and_ids_never_meet_the_inner_ones() {
    let (s, _rx) = sources("tree", "");
    let ids = ids(&s);
    assert_eq!(
        &ids[..6],
        [
            "a",
            "plugin:p:yt",
            "plugin:p:yt:search",
            "plugin:p:two",
            "history:",
            "history:x"
        ]
    );
    let inner = Fixed.playlists().unwrap();
    for id in ids.iter().filter(|i| is_plugin(i)) {
        assert!(inner.iter().all(|p| p.id != *id));
    }
    // Inner ids and "all tracks" go straight through.
    assert_eq!(s.tracks(Some("a")).unwrap()[0].title, "a");
    assert_eq!(s.tracks(None).unwrap()[0].title, "all");
}

#[test]
fn a_read_never_waits_for_the_plugin_and_the_next_one_has_the_answer() {
    let (s, rx) = sources("async", &format!("sleep 0.5\n{CANNED}"));
    let t = Instant::now();
    assert_eq!(ids(&s).len(), 6, "no playlists yet");
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
    answered(&rx).unwrap();
    // The two sources ask separately; the folder tree gains the nested lists once both are cached.
    answered(&rx).unwrap();
    let all = s.playlists().unwrap();
    let at = all.iter().position(|p| p.id == "plugin:p:yt").unwrap();
    let rows: Vec<(&str, usize)> = all[at..at + 6]
        .iter()
        .map(|p| (p.id.as_str(), p.depth))
        .collect();
    assert_eq!(
        rows,
        [
            ("plugin:p:yt", 0),
            ("plugin:p:yt:search", 1),
            ("plugin:p:yt:list:f", 1),
            ("plugin:p:yt:list:l", 2), // under its folder
            ("plugin:p:yt:list:o", 1), // the parent is not listed: top level
            ("plugin:p:two", 0),
        ]
    );
    assert!(all[at + 2].is_folder && !all[at + 3].is_folder);

    let list = "plugin:p:yt:list:l";
    let t = Instant::now();
    assert!(s.tracks(Some(list)).unwrap().is_empty());
    assert!(t.elapsed() < Duration::from_millis(300));
    answered(&rx).unwrap();
    let got = s.tracks(Some(list)).unwrap();
    // The row with neither an id nor a file is dropped.
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].title, "One");
    assert_eq!((got[0].bpm_x100, got[0].length_s), (12850, 200));
    assert_eq!(got[0].comment, "http://x");
    assert_eq!(
        got[0].remote,
        Some(Remote {
            plugin: "p".into(),
            source: "yt".into(),
            id: "t1".into()
        })
    );
    assert_eq!(got[0].file, None);
    assert_eq!(got[1].file, Some("/m/two.mp3".into()));
    assert_eq!(got[1].remote, None);
}

#[test]
fn searches_fill_their_entry_and_only_the_latest_answer_counts() {
    let script = r#"read line
case "$line" in
*'"query":"slow"'*) sleep 1; printf '{"tracks":[{"id":"s","title":"slow"}]}' ;;
*) printf '{"tracks":[{"id":"f","title":"fast"}]}' ;;
esac"#;
    let (s, rx) = sources("search", script);
    let entry = "plugin:p:yt:search";
    assert!(!s.search("a", "x"), "not a search entry");
    assert!(!s.search("plugin:p:yt", "x"), "a folder is no search entry");
    assert!(
        s.search(entry, "  "),
        "a blank query is accepted and asks nothing"
    );
    assert!(s.search(entry, "slow"));
    assert!(s.search(entry, "fast"));
    answered(&rx).unwrap();
    assert_eq!(s.tracks(Some(entry)).unwrap()[0].title, "fast");
    std::thread::sleep(Duration::from_millis(1400));
    assert!(rx.try_recv().is_err(), "the stale answer was not announced");
    assert_eq!(s.tracks(Some(entry)).unwrap()[0].title, "fast");
}

#[test]
fn failures_are_reported_and_a_track_list_asks_again_but_a_playlist_list_does_not() {
    let (s, rx) = sources("fail", r#"printf '{"error":"boom"}'"#);
    s.playlists().unwrap();
    for _ in 0..2 {
        assert!(answered(&rx).unwrap_err().contains("boom"));
    }
    ids(&s);
    assert!(
        rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "no new request"
    );

    let list = "plugin:p:yt:list:l";
    for _ in 0..2 {
        assert!(s.tracks(Some(list)).unwrap().is_empty());
        assert!(answered(&rx).unwrap_err().contains("boom"));
    }
}

#[test]
fn nesting_drops_parent_cycles_and_stays_bounded() {
    let row = |id: &str, parent: Option<&str>| Listed {
        id: id.into(),
        name: id.into(),
        folder: true,
        parent: parent.map(Into::into),
    };
    let rows = [row("a", Some("b")), row("b", Some("a")), row("c", None)];
    let got: Vec<&str> = nest(&rows).iter().map(|(l, _)| l.id.as_str()).collect();
    assert_eq!(got, ["c"]);
    let own = [row("x", None), row("x", Some("x"))];
    assert!(nest(&own).len() <= MAX_DEPTH + 1);
}

/// A plugin whose `fetch` writes `$dir/$name.mp3` (and counts its runs in `$dir/.runs`).
const FETCHER: &str = r#"read line
dir=$(printf '%s' "$line" | sed 's/.*"dir":"\([^"]*\)".*/\1/')
name=$(printf '%s' "$line" | sed 's/.*"name":"\([^"]*\)".*/\1/')
echo run >> "$dir/.runs"
printf 'audio' > "$dir/$name.mp3"
printf '{"file":"%s.mp3"}' "$name""#;

fn remote(id: &str) -> Remote {
    Remote {
        plugin: "p".into(),
        source: "yt".into(),
        id: id.into(),
    }
}

#[test]
fn a_finished_download_is_reused_but_an_unfinished_one_is_not() {
    let d = temp("fetch");
    let reg = registry_of(&d, "p", json!({}), FETCHER);
    let root = d.join("dl");
    let first = fetch_in(&reg, &root, &remote("abc")).unwrap();
    assert_eq!(first, root.join("p/yt/abc.mp3"));
    assert_eq!(fetch_in(&reg, &root, &remote("abc")).unwrap(), first);
    let runs = std::fs::read_to_string(root.join("p/yt/.runs")).unwrap();
    assert_eq!(runs.lines().count(), 1, "the second fetch used the file");

    // A half-written file does not count, nor does an empty one.
    std::fs::write(root.join("p/yt/def.part"), "x").unwrap();
    std::fs::write(root.join("p/yt/def.mp3"), "").unwrap();
    assert_eq!(
        fetch_in(&reg, &root, &remote("def")).unwrap(),
        root.join("p/yt/def.mp3")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("p/yt/def.mp3")).unwrap(),
        "audio"
    );
}

#[test]
fn ids_make_safe_distinct_file_names() {
    assert_eq!(file_name("../a b/é"), "%2E%2E%2Fa%20b%2F%C3%A9");
    assert_eq!(file_name("dQw4w9WgXcQ"), "dQw4w9WgXcQ");
    let d = temp("safe-name");
    let reg = registry_of(&d, "p", json!({}), FETCHER);
    let f = fetch_in(&reg, &d.join("dl"), &remote("../../escape")).unwrap();
    assert_eq!(f.parent().unwrap(), d.join("dl/p/yt"));
}

#[test]
fn a_bad_fetch_reply_is_an_error() {
    let d = temp("fetch-bad");
    for (script, want) in [
        (r#"printf '{}'"#, "no `file`"),
        (r#"printf '{"file":"nope.mp3"}'"#, "was not written"),
        (r#"printf '{"error":"geo-blocked"}'"#, "geo-blocked"),
    ] {
        let reg = registry_of(&d, "p", json!({}), script);
        let e = fetch_in(&reg, &d.join("dl"), &remote("x")).unwrap_err();
        assert!(e.contains(want), "{e}");
    }
    let none = Registry::default();
    assert!(
        fetch_in(&none, &d, &remote("x"))
            .unwrap_err()
            .contains("not installed")
    );
}

#[test]
fn only_a_search_entry_id_is_a_search() {
    assert!(is_search("plugin:p:yt:search"));
    assert!(!is_search("plugin:p:yt:list:search"));
    assert!(!is_search("plugin:p:yt"));
    assert!(!is_search("search"));
}
