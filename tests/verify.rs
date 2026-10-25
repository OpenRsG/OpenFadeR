//! `openfader verify`: the stick checker as a command (src/app/device/verify.rs), run as the built binary. The exit code is the
//! verdict (0 ready, 1 needs fixing or empty, 2 unreadable, 3 could not be checked) and nothing is written to the stick. The
//! test with a library on a pretend stick needs the bundled device plugin (`extension/plugin/build.sh`) and skips without it.

mod common;

use openfader::library::{self, snapshot::Snapshot};
use openfader::plugin::device::{self, Control, ExportOptions, Feature};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

/// Runs `openfader verify args…`; `stick` is a folder to count as a mounted device. Returns the exit code and what it printed.
fn verify(args: &[&str], stick: Option<&Path>) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openfader"));
    cmd.arg("verify").args(args);
    match stick {
        Some(s) => cmd.env("OPENFADER_DEVICE_ROOTS", s),
        None => cmd.env_remove("OPENFADER_DEVICE_ROOTS"),
    };
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn what_cannot_be_checked_exits_3_and_help_exits_0() {
    let dir = common::fresh_dir("verify-none");
    let folder = dir.to_str().unwrap();
    // A folder that exists but is no device, a path that is not there, no path, an option that does not exist.
    for args in [
        &[folder][..],
        &["/no/such/stick"],
        &[],
        &["--bogus", folder],
    ] {
        let (code, out, err) = verify(args, None);
        assert_eq!(code, 3, "{args:?}: {out}{err}");
        assert!(
            out.is_empty() && err.starts_with("openfader verify:"),
            "{args:?}: {err}"
        );
    }
    let (code, out, _) = verify(&["--help"], None);
    assert_eq!(code, 0);
    assert!(
        out.contains("Usage: openfader verify <mount>") && out.contains("--players"),
        "{out}"
    );
}

#[test]
fn a_stick_with_a_library_is_ready_until_audio_goes_missing_and_a_cut_off_export_is_reported_not_recovered(
) {
    let Some(plugin) = common::device_plugin() else {
        return;
    };
    let root = common::fresh_dir("verify-stick");
    let (music, stick) = (root.join("music"), root.join("stick"));
    std::fs::create_dir_all(&music).unwrap();
    std::fs::create_dir_all(&stick).unwrap();
    for (file, secs) in [("a.wav", 4.0), ("b.wav", 5.0)] {
        common::click_wav(&music.join(file), 120.0, secs);
    }
    let provider = device::provider(plugin).unwrap();
    let snap = Snapshot::take(&*library::open(&music).unwrap()).unwrap();
    provider
        .export(
            &stick,
            &snap,
            &ExportOptions::default(),
            &Arc::new(Control::default()),
        )
        .unwrap();
    let has_check = provider
        .info(&stick)
        .unwrap()
        .features
        .contains(&Feature::Check);

    // A stick that was just exported is ready.
    let (code, out, err) = verify(&["--json", stick.to_str().unwrap()], Some(&stick));
    assert_eq!(code, 0, "{out}{err}");
    let j: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (j["verdict"].as_str(), j["exit_code"].as_u64()),
        (Some("ready"), Some(0))
    );
    assert!(j["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["present"] == true && l["readable"] == true));
    let (code, text, _) = verify(&[stick.to_str().unwrap()], Some(&stick));
    assert!(
        code == 0 && text.contains("READY") && text.contains("LIBRARIES"),
        "{text}"
    );

    // An audio file the library lists is gone from the stick: needs fixing, with the file named. (A plugin with its own check
    // is trusted to find it, the host's own look at the library only stands in for one without.)
    let audio = std::fs::read_dir(stick.join("Contents"))
        .map(|_| find_audio(&stick))
        .unwrap_or_default();
    assert!(!audio.is_empty(), "the export put audio on the stick");
    std::fs::remove_file(&audio[0]).unwrap();
    if !has_check {
        let (code, out, err) = verify(&["--json", stick.to_str().unwrap()], Some(&stick));
        assert_eq!(code, 1, "{out}{err}");
        let j: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j["verdict"], "needs_fixing");
        assert!(j["problems"][0]["files"][0]
            .as_str()
            .unwrap()
            .ends_with(audio[0].file_name().unwrap().to_str().unwrap()));
    }

    // A change cut off half way is reported and left as it is: the command never writes.
    let journal = stick.join(".openfader/journal.json");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        r#"{"owner":"rekordbox-device","state":"staging","files":[],"started":0}"#,
    )
    .unwrap();
    let (code, out, _) = verify(&["--json", stick.to_str().unwrap()], Some(&stick));
    assert_eq!(code, 1, "{out}");
    let j: Value = serde_json::from_str(&out).unwrap();
    assert!(j["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "openfader:interrupted"));
    assert!(journal.is_file(), "verify must not recover");
}

fn find_audio(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            found.extend(find_audio(&p));
        } else if p.extension().is_some_and(|x| x == "wav") {
            found.push(p);
        }
    }
    found
}
