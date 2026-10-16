//! Shared helpers for the integration tests. Tests against a real device skip unless `OPENFADER_USB` names a mounted device whose
//! library a bundled plugin reads (the plugins are built by `extension/plugin/build.sh`; the ignored `.local/device-key` holds the
//! key of its encrypted library, if any), so machine paths and keys stay out of the repository.
#![allow(dead_code)] // each test binary uses a subset

use openfader::library::{self, Library};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The manifests of the bundled plugins: their folder and `plugin.json`.
fn manifests() -> Vec<(PathBuf, Value)> {
    std::fs::read_dir(root().join("extension/plugin"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path().join("plugin.json")).ok()?;
            Some((e.path(), serde_json::from_str(&text).ok()?))
        })
        .collect()
}

/// Makes the bundled plugins the app's plugins for this test binary and returns the name of the device plugin: the one whose
/// manifest says `library.device`. `None` when it is not built (this OS's library file is missing). The `secret` settings of
/// that plugin are filled from `.local/device-key`. Call before anything reads the plugin registry.
pub fn device_plugin() -> Option<&'static str> {
    static READY: LazyLock<Option<String>> = LazyLock::new(|| {
        let (dir, manifest) = manifests()
            .into_iter()
            .find(|(_, m)| m["library"]["device"] == true)?;
        let lib = manifest["library"]["lib"][std::env::consts::OS].as_str()?;
        if !dir.join(lib).is_file() {
            eprintln!("skip: run extension/plugin/build.sh first");
            return None;
        }
        let name = manifest["name"].as_str()?.to_owned();
        let key = std::fs::read_to_string(root().join(".local/device-key")).unwrap_or_default();
        let secrets: serde_json::Map<String, Value> = manifest["settings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["type"] == "secret")
            .filter_map(|s| Some((s["id"].as_str()?.to_owned(), json!(key.trim()))))
            .collect();
        let config =
            std::env::temp_dir().join(format!("openfader-test-config-{}", std::process::id()));
        std::fs::create_dir_all(config.join("openfader")).unwrap();
        std::fs::write(
            config.join("openfader/plugins.json"),
            json!({"settings": {&name: secrets}}).to_string(),
        )
        .unwrap();
        // SAFETY: set once, before the plugin registry is first read; the tests of a binary share this value. The store and
        // the analysis of the tests go to the same temp folder, never the user's.
        unsafe {
            std::env::set_var("OPENFADER_PLUGINS", root().join("extension/plugin"));
            std::env::set_var("XDG_CONFIG_HOME", &config);
            std::env::set_var("XDG_DATA_HOME", config.join("data"));
            std::env::set_var("XDG_CACHE_HOME", config.join("cache"));
        }
        Some(name)
    });
    READY.as_deref()
}

/// The mounted device `OPENFADER_USB` names, when a bundled plugin finds a library on it.
pub fn usb_root() -> Option<PathBuf> {
    let r = PathBuf::from(std::env::var_os("OPENFADER_USB").filter(|v| !v.is_empty())?);
    device_plugin()?;
    let found =
        openfader::plugin::device::find(&r).is_some_and(|(_, info)| !info.libraries.is_empty());
    if !found {
        eprintln!("skip: no plugin reads a library on OPENFADER_USB");
    }
    found.then_some(r)
}

/// The library of the device [`usb_root`] names, read by its plugin, with the tracks.
pub fn usb_library() -> Option<(Box<dyn Library + Send>, Vec<library::Track>)> {
    let lib = library::open(&usb_root()?).unwrap();
    let tracks = lib.tracks(None).unwrap();
    Some((lib, tracks))
}

/// `n` beats of a 120 BPM 4/4 grid starting at 250 ms.
pub fn grid(n: u32) -> Vec<openfader::analysis::Beat> {
    (0..n)
        .map(|i| openfader::analysis::Beat {
            bar_pos: (i % 4) as u16 + 1,
            bpm_x100: 12000,
            time_ms: 250 + i * 500,
        })
        .collect()
}

/// An empty per-process temp directory `rk-{tag}-{pid}`, recreated on every call.
pub fn fresh_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Writes a `secs`-long 44.1 kHz stereo 16-bit WAV with a 2 ms 1 kHz click on every beat at `bpm` (first click at 0.173 s).
pub fn click_wav(path: &std::path::Path, bpm: f64, secs: f64) {
    const RATE: usize = 44100;
    let mut pcm = vec![0i16; (secs * RATE as f64) as usize * 2];
    let mut t = 0.173;
    while t < secs - 0.05 {
        let at = (t * RATE as f64) as usize;
        for i in 0..90 {
            let v = (0.8
                * (-(i as f32) / 30.0).exp()
                * (std::f32::consts::TAU * 1000.0 * i as f32 / RATE as f32).sin()
                * 32767.0) as i16;
            pcm[(at + i) * 2] = v;
            pcm[(at + i) * 2 + 1] = v;
        }
        t += 60.0 / bpm;
    }
    let data: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut w = [
        b"RIFF".as_slice(),
        &(36 + data.len() as u32).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
    ]
    .concat();
    w.extend_from_slice(&[1, 0, 2, 0]); // PCM, 2 channels
    w.extend_from_slice(&(RATE as u32).to_le_bytes());
    w.extend_from_slice(&(RATE as u32 * 4).to_le_bytes());
    w.extend_from_slice(&[4, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    std::fs::write(path, w).unwrap();
}
