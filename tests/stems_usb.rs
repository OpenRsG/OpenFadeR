//! STEMS on a real track of the device library named by `OPENFADER_USB` (read-only): separation runs, stems add up
//! to the track, the cache serves the second run. Skips without the USB or without the model
//! (`$XDG_CACHE_HOME/openfader/models/htdemucs.onnx`, see `stems::model`). `OPENFADER_STEMS_TRACK` (default 0) picks the
//! track, `OPENFADER_STEMS_SECS` (default 30; 0 = the whole track) how much of it, cut from the middle.

mod common;

use openfader::audio::{STEM_NAMES, Stems, Track16, decode};
use openfader::stems::{Job, Quality, model};
use std::time::{Duration, Instant};

/// Waits for the job's outcome (the stems are the job's `stems()`).
fn wait(job: &Job) -> Result<(), String> {
    loop {
        if let Some(r) = job.poll() {
            return r;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn dump(s: &Stems, k: usize) -> Vec<i16> {
    let mut v = vec![0; 2 * s.frames()];
    s.read(k, 0, &mut v);
    v
}

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-20).log10()
}

#[test]
fn separates_a_usb_track_into_stems_that_add_up() {
    let Some((_, tracks)) = common::usb_library() else {
        return;
    };
    if !model::usable() {
        eprintln!("skip: {}", model::missing_hint());
        return;
    }
    // A scratch cache so the run neither reads nor writes the user's; the model keeps its place.
    let model_file = model::path().unwrap();
    let cache = std::env::temp_dir().join(format!("rk-stems-usb-{}", std::process::id()));
    unsafe {
        std::env::set_var("OPENFADER_STEMS_MODEL", &model_file);
        std::env::set_var("XDG_CACHE_HOME", &cache);
    }
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let file = tracks[env("OPENFADER_STEMS_TRACK", 0)]
        .file
        .clone()
        .unwrap();
    let full = decode(&file).unwrap();
    let frames = full.samples.len() / 2;
    let want = match env("OPENFADER_STEMS_SECS", 30) {
        0 => frames,
        s => (s * full.rate as usize).min(frames),
    };
    let from = (frames - want) / 2;
    let pcm = openfader::audio::Pcm {
        rate: full.rate,
        samples: full.samples[2 * from..2 * (from + want)].to_vec(),
    };
    let secs = want as f64 / pcm.rate as f64;

    let t = Instant::now();
    let job = Job::start(
        file.clone(),
        std::sync::Arc::new(Track16::from(pcm.clone())),
        Quality::Quality,
        true,
    );
    wait(&job).unwrap();
    let took = t.elapsed();
    let stems = job.stems().clone();
    assert_eq!(stems.frames(), want);
    assert!(stems.is_complete() && (0..want).step_by(4999).all(|i| stems.ready(i)));

    // Σ stems against the track: error energy relative to the track's, and each stem's level.
    let (mut mix_e, mut err_e) = (0f64, 0f64);
    let mut stem_e = [0f64; 4];
    let parts: Vec<Vec<i16>> = (0..4).map(|k| dump(&stems, k)).collect();
    for i in 0..stems.frames() * 2 {
        let m = pcm.samples[i] as f64;
        let mut sum = 0.0;
        for k in 0..4 {
            let v = parts[k][i] as f64 / 32768.0;
            stem_e[k] += v * v;
            sum += v;
        }
        mix_e += m * m;
        err_e += (m - sum) * (m - sum);
    }
    let n = (stems.frames() * 2) as f64;
    let recon = db(err_e / mix_e);
    eprintln!(
        "stems: {secs:.0} s of audio in {:.1} s ({:.1} s per minute of audio)",
        took.as_secs_f64(),
        took.as_secs_f64() * 60.0 / secs
    );
    eprintln!(
        "stems: sum of stems vs track: error {recon:.1} dB relative to the track (track {:.1} dBFS rms)",
        db(mix_e / n)
    );
    for k in 0..4 {
        eprintln!(
            "stems: {:5} {:6.1} dBFS rms ({:+.1} dB vs the track)",
            STEM_NAMES[k],
            db(stem_e[k] / n),
            db(stem_e[k] / mix_e)
        );
    }
    assert!(
        recon < -10.0,
        "stems should add up to the track within -10 dB, got {recon:.1}"
    );
    assert!(
        stem_e.iter().all(|e| db(e / mix_e) > -40.0),
        "every stem of a full-band track carries some energy"
    );

    // Second run: the cache answers (the model is not loaded), with the same samples.
    let t = Instant::now();
    let job2 = Job::start(
        file,
        std::sync::Arc::new(Track16::from(pcm)),
        Quality::Quality,
        true,
    );
    wait(&job2).unwrap();
    eprintln!("stems: cache hit in {:.2} s", t.elapsed().as_secs_f64());
    assert!(t.elapsed() < Duration::from_secs(10) && took > t.elapsed());
    assert!(
        (0..4).all(|k| dump(job2.stems(), k) == dump(&stems, k)),
        "cached stems equal the separated ones"
    );
    std::fs::remove_dir_all(cache).unwrap();
}
