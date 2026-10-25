//! Shared helpers for the integration tests. Tests against a real device skip unless `OPENFADER_USB` names a mounted device whose
//! library a plugin reads (the ignored `.local/device-key` holds the key of its encrypted library, if any), so machine paths and
//! keys stay out of the repository.
#![allow(dead_code)] // each test binary uses a subset

use openfader::library::{self, Library};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Points the app's config, data and cache at a temp folder (never the user's) and returns the name of the device plugin: the
/// built-in `rekordbox-device`, always there ([`openfader::plugin::builtin`]). Its `secret` settings are filled from the
/// ignored `.local/device-key`. Call before anything reads the plugin registry.
pub fn device_plugin() -> &'static str {
    const NAME: &str = "rekordbox-device";
    static READY: LazyLock<()> = LazyLock::new(|| {
        let manifest: Value =
            serde_json::from_str(include_str!("../../crates/rekordbox-device/plugin.json")).unwrap();
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
            json!({"settings": {NAME: secrets}}).to_string(),
        )
        .unwrap();
        // SAFETY: set once, before the plugin registry is first read; the tests of a binary share this value. The store and
        // the analysis of the tests go to the same temp folder, never the user's.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &config);
            std::env::set_var("XDG_DATA_HOME", config.join("data"));
            std::env::set_var("XDG_CACHE_HOME", config.join("cache"));
        }
    });
    *READY;
    NAME
}

/// The mounted device `OPENFADER_USB` names, when a plugin finds a library on it.
pub fn usb_root() -> Option<PathBuf> {
    let r = PathBuf::from(std::env::var_os("OPENFADER_USB").filter(|v| !v.is_empty())?);
    device_plugin();
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
