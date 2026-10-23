//! KEY DETECTOR accuracy on a labelled set: `key_eval AUDIO_DIR LABEL_DIR [THREADS]`, and the constant-Q front end on one file:
//! `key_eval --cqt FILE OUT.f32`.
//!
//! Made for the public GiantSteps key set (`<id>.LOFI.mp3` with `<id>.LOFI.key` holding "C minor"), but any folder of audio files
//! with `<audio file stem>.key` or `<audio file stem>.LOFI.key` labels in LABEL_DIR works. Runs both detectors
//! ([`openfader::analysis::key::Detector`]) over every file, prints one line per file (stem, label, classic, neural, all as
//! Camelot) and the MIREX categories (correct, fifth, relative, parallel, other; weighted score = correct + 0.5 fifth + 0.3
//! relative + 0.2 parallel) with the seconds each detector took. NEURAL needs its model: `OPENFADER_KEY_MODEL` or the models cache.
//!
//! `--cqt` writes the spectrogram as little-endian f32, 105 bins x frames (bin-major), to compare with another implementation.

use openfader::analysis::key::{self, Detector, Key};
use openfader::audio;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;

/// MIREX category of `got` against the label `want`: 0 correct, 1 fifth, 2 relative, 3 parallel, 4 other.
fn category(want: Key, got: Key) -> usize {
    let ((wn, wmajor), (gn, gmajor)) = (want, got);
    let steps = (gn as i32 - wn as i32).rem_euclid(12);
    match (wmajor == gmajor, steps) {
        (true, 0) => 0,
        (true, 1 | 11) => 1,
        (false, 0) => 2,
        // the same tonic: a minor key's Camelot number is its major's minus 3
        (false, s) if (wmajor && s == 9) || (gmajor && s == 3) => 3,
        _ => 4,
    }
}

fn label(dir: &Path, audio: &Path) -> Option<Key> {
    let stem = audio.file_stem()?.to_str()?;
    let base = stem.trim_end_matches(".LOFI");
    [
        format!("{stem}.key"),
        format!("{base}.LOFI.key"),
        format!("{base}.key"),
    ]
    .iter()
    .find_map(|n| std::fs::read_to_string(dir.join(n)).ok())
    .and_then(|t| key::parse(&t.trim().replace(' ', "")))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--cqt") {
        let pcm = audio::decode(Path::new(&args[2])).expect("decode");
        let s = openfader::analysis::key::cqt::log_cqt(&pcm.samples, pcm.rate, &|| false).unwrap();
        let bytes: Vec<u8> = s.data.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(&args[3], bytes).unwrap();
        println!("{} frames", s.frames);
        return;
    }
    let (audio_dir, label_dir) = (
        args.get(1)
            .expect("usage: key_eval AUDIO_DIR LABEL_DIR [THREADS]"),
        args.get(2)
            .expect("usage: key_eval AUDIO_DIR LABEL_DIR [THREADS]"),
    );
    let threads: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
    let mut files: Vec<PathBuf> = std::fs::read_dir(audio_dir)
        .expect("audio folder")
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e == "mp3" || e == "wav" || e == "flac")
        })
        .collect();
    files.sort();
    let next = AtomicUsize::new(0);
    // [detector][category] counts, seconds, and the printed lines
    let tally = Mutex::new(([[0usize; 5]; 2], [0f64; 2], Vec::new()));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some(path) = files.get(next.fetch_add(1, Relaxed)) {
                    let Some(want) = label(Path::new(label_dir), path) else {
                        continue;
                    };
                    let Ok(pcm) = audio::decode(path) else {
                        continue;
                    };
                    let mut row: [(String, usize, f64); 2] = Default::default();
                    for (i, d) in [Detector::Classic, Detector::Neural]
                        .into_iter()
                        .enumerate()
                    {
                        let t = Instant::now();
                        let got = key::detect_with(d, &pcm.samples, pcm.rate).unwrap_or_default();
                        let cat = key::parse(&got).map_or(4, |g| category(want, g));
                        row[i] = (got, cat, t.elapsed().as_secs_f64());
                    }
                    let mut t = tally.lock().unwrap();
                    for i in 0..2 {
                        t.0[i][row[i].1] += 1;
                        t.1[i] += row[i].2;
                    }
                    let stem = path.file_stem().unwrap().to_string_lossy();
                    t.2.push(format!(
                        "{stem}\t{}{}\t{}\t{}",
                        want.0,
                        if want.1 { 'B' } else { 'A' },
                        row[0].0,
                        row[1].0
                    ));
                }
            });
        }
    });
    let (counts, secs, mut lines) = tally.into_inner().unwrap();
    lines.sort();
    println!("{}", lines.join("\n"));
    let n = counts[0].iter().sum::<usize>().max(1) as f64;
    for (i, name) in ["CLASSIC", "NEURAL"].iter().enumerate() {
        let c = counts[i].map(|c| 100.0 * c as f64 / n);
        println!(
            "{name:8} n={n:.0} correct {:.1}%  fifth {:.1}%  relative {:.1}%  parallel {:.1}%  other {:.1}%  weighted {:.1}  ({:.2} s per track)",
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            c[0] + 0.5 * c[1] + 0.3 * c[2] + 0.2 * c[3],
            secs[i] / n
        );
    }
}
