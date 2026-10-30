//! Beat grid of one audio file: `grid_probe FILE [LO-HI] [precision]`. Prints the BPM and the first 12 beat times (ms, bar
//! position), then the low-band onset nearest each of those beats (the first sample where the 200 Hz low-passed signal exceeds
//! a tenth of its peak after 30 ms of lower level), so the grid's anchor can be compared with the kick itself.

use openfader::analysis::detect::{GridOptions, Mode, beat_grid_with};
use openfader::audio;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pcm = audio::decode(Path::new(&args[1])).expect("decode");
    let range = args
        .get(2)
        .and_then(|r| r.split_once('-'))
        .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))
        .unwrap_or((85, 175));
    let precision = args.get(3).is_some_and(|p| p == "precision");
    let opt = GridOptions {
        mode: Mode::Normal,
        precision,
        bpm_range: range,
    };
    let beats = beat_grid_with(&pcm.samples, pcm.rate, &opt);
    let Some(first) = beats.first() else {
        println!("no grid");
        return;
    };
    println!(
        "bpm {:.2} beats {} range {:?} precision {precision}",
        first.bpm_x100 as f64 / 100.0,
        beats.len(),
        range
    );
    // Low band envelope (two 200 Hz one-poles on the mono sum), per sample.
    let a = 1.0 - (-std::f32::consts::TAU * 200.0 / pcm.rate as f32).exp();
    let (mut y1, mut y2) = (0.0f32, 0.0f32);
    let low: Vec<f32> = pcm
        .samples
        .chunks_exact(2)
        .map(|f| {
            y1 += a * (0.5 * (f[0] + f[1]) - y1);
            y2 += a * (y1 - y2);
            y2.abs()
        })
        .collect();
    let peak = low.iter().cloned().fold(0.0f32, f32::max);
    let ms = |i: usize| i as f64 * 1000.0 / pcm.rate as f64;
    for b in beats.iter().take(12) {
        let at = (b.time_ms as f64 / 1000.0 * pcm.rate as f64) as usize;
        // search +-40 ms for the first sample above 10 % of the peak that is preceded by 20 ms below 3 % of it
        let lo = at.saturating_sub(pcm.rate as usize * 40 / 1000);
        let hi = (at + pcm.rate as usize * 40 / 1000).min(low.len());
        let quiet = pcm.rate as usize * 20 / 1000;
        let onset = (lo..hi).find(|&i| {
            low[i] > 0.1 * peak
                && i >= quiet
                && low[i - quiet..i - quiet / 2]
                    .iter()
                    .all(|&v| v < 0.03 * peak)
        });
        match onset {
            Some(i) => println!(
                "beat {:>7} ms bar {}  kick onset {:>9.1} ms  grid-kick {:+.1} ms",
                b.time_ms,
                b.bar_pos,
                ms(i),
                b.time_ms as f64 - ms(i)
            ),
            None => println!(
                "beat {:>7} ms bar {}  (no clear kick onset within 40 ms)",
                b.time_ms, b.bar_pos
            ),
        }
    }
}
