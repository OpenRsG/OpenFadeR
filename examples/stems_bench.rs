//! STEMS timing and separation check on one audio file: `stems_bench FILE [quality|speed] [PLAY_SECONDS]`.
//!
//! Starts a [`openfader::stems::Job`] like a deck does (play position `PLAY_SECONDS`, default 0), prints the seconds until the
//! frames at the play position are usable, until the whole track is done and per audio minute, then the energy of each stem
//! and of the mix with one stem muted, relative to the original (what the mute buttons do to the sound). Set
//! `OPENFADER_STEMS_MODEL`, point `XDG_CACHE_HOME` at an empty folder to see a cold run (a cache hit separates nothing), and
//! `OPENFADER_STEMS_GPU=0` to keep the GPU out.

use openfader::audio::{self, Mixed, STEM_COUNT, STEM_NAMES, Source};
use openfader::stems::{Job, Quality};
use std::time::{Duration, Instant};

fn rms_db(mut f: impl FnMut(usize) -> [f32; 2], frames: usize) -> f64 {
    let (mut sum, step) = (0f64, 4);
    for i in (0..frames).step_by(step) {
        let [l, r] = f(i);
        sum += (l * l + r * r) as f64;
    }
    10.0 * (sum / (2 * frames.div_ceil(step)) as f64)
        .max(1e-20)
        .log10()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::PathBuf::from(
        args.get(1)
            .expect("usage: stems_bench FILE [quality|speed] [PLAY_SECONDS]"),
    );
    let quality = if args.get(2).map(String::as_str) == Some("speed") {
        Quality::Speed
    } else {
        Quality::Quality
    };
    let play_s: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let pcm = audio::decode(&path).expect("decode");
    let (frames, rate) = (pcm.samples.len() / 2, pcm.rate);
    let minutes = frames as f64 / rate as f64 / 60.0;
    let play = ((play_s * rate as f64) as usize).min(frames - 1);

    if std::env::var_os("WARM").is_some() {
        openfader::stems::warm(true); // what the deck does when STEMS is switched on: the engines open before the track
        std::thread::sleep(Duration::from_secs(8));
    }
    let t0 = Instant::now();
    let pcm = std::sync::Arc::new(audio::Track16::from(pcm)); // the deck's own form: 16-bit, shared with the job
    let job = Job::start(path, pcm.clone(), quality, true);
    job.focus(play, &[]);
    let mut usable = None;
    let result = loop {
        if usable.is_none() && job.stems().is_some_and(|s| s.ready(play)) {
            usable = Some(t0.elapsed());
        }
        if let Some(r) = job.poll() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let total = t0.elapsed();
    result.expect("separation");
    let stems = job.stems().expect("allocated stems");
    println!(
        "{:?} {:.1} min: stems usable at the play position after {:.1} s, whole track after {:.1} s = {:.1} s per audio minute",
        quality,
        minutes,
        usable.unwrap_or(total).as_secs_f32(),
        total.as_secs_f32(),
        total.as_secs_f64() / minutes
    );
    let hwm = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    if let Some(l) = hwm.lines().find(|l| l.starts_with("VmHWM")) {
        println!("peak RSS: {}", l.trim_start_matches("VmHWM:").trim());
    }

    let all = rms_db(|i| pcm.frame_at(i as f64).unwrap(), frames);
    println!("original {all:.1} dB");
    for k in 0..STEM_COUNT {
        let mut only = [0.0; STEM_COUNT];
        only[k] = 1.0;
        let solo = rms_db(
            |i| {
                Mixed {
                    pcm: &pcm,
                    stems,
                    gain: only,
                }
                .frame_at(i as f64)
                .unwrap()
            },
            frames,
        );
        let mut muted = [1.0; STEM_COUNT];
        muted[k] = 0.0;
        let without = rms_db(
            |i| {
                Mixed {
                    pcm: &pcm,
                    stems,
                    gain: muted,
                }
                .frame_at(i as f64)
                .unwrap()
            },
            frames,
        );
        println!(
            "{:6}: solo {:+.1} dB, track with it muted {:+.1} dB (both relative to the original)",
            STEM_NAMES[k],
            solo - all,
            without - all
        );
    }
    openfader::stems::model::shutdown(); // Dawn crashes in `exit()` with the GPU session still open
}
