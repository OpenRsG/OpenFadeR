use super::*;
use serde_json::json;
use std::fs;

/// A fresh empty folder under the system temp dir.
pub(super) fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rk-plugin-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A plugin folder `dir/name` whose command is `sh run.sh` with `script` as `run.sh`.
fn plugin(dir: &Path, name: &str, script: &str) -> PathBuf {
    let p = dir.join(name);
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("plugin.json"),
        json!({"name": name, "command": ["sh", "run.sh"], "actions": [{"id": "a", "label": "A"}]})
            .to_string(),
    )
    .unwrap();
    fs::write(p.join("run.sh"), script).unwrap();
    p
}

/// The registry of one plugin `name` in `dir`: `manifest` (an object with sources, events …) plus `name` and the command
/// `sh run.sh`, whose `run.sh` is `script`.
pub(super) fn registry_of(
    dir: &Path,
    name: &str,
    manifest: serde_json::Value,
    script: &str,
) -> Registry {
    let p = dir.join(name);
    fs::create_dir_all(&p).unwrap();
    let mut m = manifest;
    m["name"] = json!(name);
    m["command"] = json!(["sh", "run.sh"]);
    fs::write(p.join("plugin.json"), m.to_string()).unwrap();
    fs::write(p.join("run.sh"), script).unwrap();
    let r = discover(&[p]);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    r
}

#[test]
fn discovery_takes_plugin_folders_and_folders_of_them_first_name_wins() {
    let (a, b) = (temp("disc-a"), temp("disc-b"));
    plugin(&a, "one", "");
    plugin(&a, "two", "");
    fs::create_dir_all(a.join("broken")).unwrap();
    fs::write(a.join("broken").join("plugin.json"), "{").unwrap();
    let single = plugin(&b, "one", ""); // same name, later in the search path
    let r = discover(&[a.clone(), single, a.join("missing")]);
    let names: Vec<&str> = r.plugins.iter().map(|p| p.manifest.name.as_str()).collect();
    assert_eq!(names, ["one", "two"]);
    assert_eq!(r.get("one").unwrap().dir, a.join("one"));
    assert_eq!(r.errors.len(), 2, "{:?}", r.errors); // the bad manifest and the duplicate
}

#[test]
fn manifests_need_a_command_for_process_features_and_a_plain_name() {
    let d = temp("manifest");
    for (n, m) in [
        (
            "nocmd",
            json!({"name": "x", "actions": [{"id": "a", "label": "A"}]}),
        ),
        ("colon", json!({"name": "a:b"})),
        (
            "source-colon",
            json!({"name": "y", "command": ["x"], "sources": [{"id": "a:b", "name": "A"}]}),
        ),
    ] {
        fs::create_dir_all(d.join(n)).unwrap();
        fs::write(d.join(n).join("plugin.json"), m.to_string()).unwrap();
    }
    let r = discover(&[d]);
    assert!(r.plugins.is_empty());
    assert_eq!(r.errors.len(), 3);
}

#[test]
fn library_declarations_are_checked_and_pick_the_file_of_this_os() {
    let d = temp("library");
    let setting = |id: &str| json!({"id": id, "type": "text", "label": "L"});
    let good = json!({
        "lib": {(std::env::consts::OS): "libx.so"}, "formats": ["xml"],
        "collection_writer": "Export", "collection_reader": "Import",
        "browser": {"label": "X", "path_setting": "file"}
    });
    for (n, lib, ok) in [
        ("good", good.clone(), true),
        ("no-os", json!({"lib": {"nowhere": "x"}}), false),
        ("upper", json!({"lib": "x", "formats": ["XML"]}), false),
        ("dotted", json!({"lib": "x", "formats": [".xml"]}), false),
        (
            "writer-dir",
            json!({"lib": "x", "formats": ["dir"], "collection_writer": "E"}),
            false,
        ),
        (
            "writer-none",
            json!({"lib": "x", "collection_writer": "E"}),
            false,
        ),
        (
            "no-setting",
            json!({"lib": "x", "browser": {"label": "X", "path_setting": "nope"}}),
            false,
        ),
    ] {
        fs::create_dir_all(d.join(n)).unwrap();
        let m = json!({"name": n, "library": lib, "settings": [setting("file")]});
        fs::write(d.join(n).join("plugin.json"), m.to_string()).unwrap();
        assert_eq!(discover(&[d.join(n)]).plugins.len(), usize::from(ok), "{n}");
    }
    let m: Manifest = serde_json::from_value(json!({"name": "g", "library": good})).unwrap();
    assert_eq!(m.library.unwrap().lib.file(), Some("libx.so"));
}

#[test]
fn knob_and_pad_modes_parse_without_a_command_and_bad_ones_reject_the_plugin() {
    let d = temp("modes");
    let write = |n: &str, m: serde_json::Value| {
        fs::create_dir_all(d.join(n)).unwrap();
        fs::write(d.join(n).join("plugin.json"), m.to_string()).unwrap();
    };
    write(
        "ok",
        json!({"name": "ok", "knob_modes": [{"name": "LOCK", "stop": 0.5}],
               "pad_modes": [{"name": "GATE", "press": ["jump", "play", {"beat_jump": -4}], "release": ["pause", {"loop": 0.5}, "loop_off"]}]}),
    );
    write(
        "stop",
        json!({"name": "s", "knob_modes": [{"name": "X", "stop": 1.5}]}),
    );
    write(
        "nosteps",
        json!({"name": "n", "pad_modes": [{"name": "X"}]}),
    );
    write(
        "badstep",
        json!({"name": "b", "pad_modes": [{"name": "X", "press": ["explode"]}]}),
    );
    let r = discover(&[d]);
    assert_eq!(r.errors.len(), 3, "{:?}", r.errors);
    assert_eq!(r.knob_modes().map(|k| k.stop).collect::<Vec<_>>(), [0.5]);
    let p = r.pad_modes().next().unwrap();
    assert_eq!(
        (p.press.as_slice(), p.release.as_slice()),
        (
            &[PadStep::Jump, PadStep::Play, PadStep::BeatJump(-4.0)][..],
            &[PadStep::Pause, PadStep::Loop(0.5), PadStep::LoopOff][..]
        )
    );
}

#[test]
fn call_sends_the_request_on_stdin_and_reads_the_reply() {
    let d = temp("call");
    let p = plugin(&d, "echo", "read line; printf '{\"got\":%s}' \"$line\"");
    let r = discover(&[p]);
    let reply = r.plugins[0]
        .call(&json!({"op": "x", "n": 1}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(reply, json!({"got": {"op": "x", "n": 1, "settings": {}}}));
}

#[test]
fn call_reports_error_replies_bad_output_failures_and_timeouts() {
    let d = temp("errors");
    for (name, script, want) in [
        ("err", "printf '{\"error\":\"nope\"}'", "err: nope"),
        ("junk", "echo hello", "junk: reply is not JSON"),
        ("fail", "exit 3", "fail: exited with"),
        ("slow", "sleep 5", "slow: no reply within 0 s"),
    ] {
        plugin(&d, name, script);
        let r = discover(&[d.join(name)]);
        let e = r.plugins[0]
            .call(&json!({}), Duration::from_millis(300))
            .unwrap_err();
        assert!(e.starts_with(want), "{name}: {e}");
    }
}

#[test]
fn a_persistent_process_serves_every_request_and_ends_with_the_app() {
    let d = temp("persistent");
    let action = json!({"persistent": true, "actions": [{"id": "a", "label": "A"}]});
    let counter = r#"n=0
while IFS= read -r line; do
  n=$((n+1)); id=${line##*\"id\":}; id=${id%%[!0-9]*}
  printf '{"id":%s,"n":%s,"pid":%s}\n' "$id" "$n" "$$"
done"#;
    let r = registry_of(&d, "keep", action.clone(), counter);
    let ask = || r.plugins[0].call(&json!({"op": "x"}), Duration::from_secs(5)).unwrap();
    let (a, b) = (ask(), ask());
    assert_eq!((&a["n"], &b["n"]), (&json!(1), &json!(2)));
    assert_eq!(a["pid"], b["pid"], "one process for both requests");
    let r2 = registry_of(&d, "mute", action, "while read -r l; do :; done");
    let e = r2.plugins[0].call(&json!({}), Duration::from_millis(300)).unwrap_err();
    assert!(e.starts_with("mute: no reply within"), "{e}");
    process::stop_all();
    #[cfg(unix)]
    assert_ne!(unsafe { libc::kill(a["pid"].as_i64().unwrap() as i32, 0) }, 0, "still running after stop_all");
}
