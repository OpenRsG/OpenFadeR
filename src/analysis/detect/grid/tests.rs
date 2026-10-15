use super::*;
use std::f32::consts::TAU;

const RATE: u32 = 44100;

/// Adds a decaying burst (`len` samples, `amp`, `hz`) at `t` seconds to both channels.
fn burst(s: &mut [f32], t: f64, hz: f32, amp: f32, len: usize) {
    let at = (t * RATE as f64).round() as usize;
    for i in 0..len {
        let v = amp
            * (-(i as f32) / (len as f32 / 3.0)).exp()
            * (TAU * hz * i as f32 / RATE as f32).sin();
        s[(at + i) * 2] += v;
        s[(at + i) * 2 + 1] += v;
    }
}

/// Click times (s) from `first`: each next one `60/bpm_at(t)` later, up to `secs - 0.05`.
fn times(bpm_at: impl Fn(f64) -> f64, first: f64, secs: f64) -> Vec<f64> {
    let (mut t, mut out) = (first, Vec::new());
    while t < secs - 0.05 {
        out.push(t);
        t += 60.0 / bpm_at(t);
    }
    out
}

/// Stereo track of `secs` with a 2 ms 1 kHz click at each of `at`.
fn render(at: &[f64], secs: f64) -> Vec<f32> {
    let mut s = vec![0f32; (secs * RATE as f64) as usize * 2];
    for &t in at {
        burst(&mut s, t, 1000.0, 0.8, 90);
    }
    s
}

/// Distance in ms from `time_ms` to the nearest of `at` (seconds).
fn off_by(at: &[f64], time_ms: u32) -> f64 {
    at.iter()
        .map(|t| (t * 1000.0 - time_ms as f64).abs())
        .fold(f64::MAX, f64::min)
}

fn opts(mode: Mode, precision: bool) -> GridOptions {
    GridOptions {
        mode,
        precision,
        ..Default::default()
    }
}

/// Mean tempo (BPM) of beats `a..b`.
fn mean_bpm(s: &[Beat]) -> f64 {
    s.iter().map(|b| b.bpm_x100 as f64).sum::<f64>() / s.len() as f64 / 100.0
}

#[test]
fn click_track_tempo_and_phase() {
    for bpm in [128.0, 174.0] {
        let beats = beat_grid(&render(&times(|_| bpm, 0.173, 30.0), 30.0), RATE);
        assert!(beats.len() > 50, "{bpm}: {} beats", beats.len());
        let got = beats[0].bpm_x100 as f64 / 100.0;
        assert!((got - bpm).abs() <= 0.1, "{bpm}: detected {got}");
        assert!(
            beats.iter().all(|b| b.bpm_x100 == beats[0].bpm_x100),
            "{bpm}: a steady click track gets a constant grid"
        );
        let period_ms = 60000.0 / bpm;
        let d = (beats[0].time_ms as f64 - 173.0).rem_euclid(period_ms);
        assert!(d.min(period_ms - d) <= 10.0, "{bpm}: phase off by {d} ms");
        for (i, b) in beats.iter().enumerate() {
            assert_eq!(
                b.bar_pos as usize,
                (i + beats[0].bar_pos as usize - 1) % 4 + 1
            );
            let want = beats[0].time_ms as f64 + i as f64 * period_ms;
            assert!(
                (b.time_ms as f64 - want).abs() < 2.0,
                "grid is not constant"
            );
        }
    }
}

/// A-minor pad (A2 A3 C4 E4 A4, slightly detuned pairs so it beats and breathes) under the clicks.
fn with_pad(mut s: Vec<f32>, amp: f32) -> Vec<f32> {
    for (i, f) in s.chunks_exact_mut(2).enumerate() {
        let t = i as f32 / RATE as f32;
        let v: f32 = [110.0f32, 220.0, 261.63, 329.63, 440.0, 110.7, 220.9, 330.1]
            .iter()
            .map(|hz| (TAU * hz * t).sin())
            .sum::<f32>()
            * amp;
        f[0] += v;
        f[1] += v;
    }
    s
}

#[test]
fn chord_pad_under_clicks_keeps_the_click_tempo() {
    for amp in [0.05, 0.1, 0.2] {
        let beats = beat_grid(
            &with_pad(render(&times(|_| 126.0, 0.173, 30.0), 30.0), amp),
            RATE,
        );
        let got = beats.first().map_or(0.0, |b| b.bpm_x100 as f64 / 100.0);
        assert!((got - 126.0).abs() <= 0.1, "pad {amp}: detected {got}");
    }
}

#[test]
fn ramping_tempo_gets_a_piecewise_grid_in_dynamic_and_in_auto() {
    let clicks = times(|t| 120.0 + 6.0 * t / 60.0, 0.173, 60.0);
    let audio = render(&clicks, 60.0);
    for opt in [opts(Mode::Dynamic, false), opts(Mode::Auto, true)] {
        let beats = beat_grid_with(&audio, RATE, &opt);
        assert!(
            beats.len().abs_diff(clicks.len()) <= 2,
            "{opt:?}: {} beats for {} clicks",
            beats.len(),
            clicks.len()
        );
        for b in &beats {
            let d = off_by(&clicks, b.time_ms);
            assert!(
                d <= 15.0,
                "{opt:?}: beat at {} ms is {d} ms from a click",
                b.time_ms
            );
        }
        let (start, end) = (mean_bpm(&beats[..8]), mean_bpm(&beats[beats.len() - 8..]));
        assert!(
            (start - 120.5).abs() < 1.5 && (end - 125.5).abs() < 1.5,
            "{opt:?}: tempo {start} -> {end}, want about 120.5 -> 125.5"
        );
        let mut tempi: Vec<u16> = beats.iter().map(|b| b.bpm_x100).collect();
        tempi.dedup();
        assert!(
            tempi.len() >= 3,
            "{opt:?}: a ramp is several tempo segments, got {tempi:?}"
        );
        assert!(
            tempi.len() < beats.len() / 6,
            "{opt:?}: segments, not a tempo per beat: {}",
            tempi.len()
        );
    }
}

#[test]
fn normal_mode_keeps_one_tempo_on_a_ramp_and_auto_without_precision_is_normal() {
    let audio = render(&times(|t| 120.0 + 6.0 * t / 60.0, 0.173, 60.0), 60.0);
    for opt in [opts(Mode::Normal, false), opts(Mode::Auto, false)] {
        let beats = beat_grid_with(&audio, RATE, &opt);
        assert!(
            beats.len() > 100 && beats.iter().all(|b| b.bpm_x100 == beats[0].bpm_x100),
            "{opt:?}: one tempo for the whole track"
        );
    }
}

#[test]
fn a_steady_track_is_one_tempo_in_dynamic_and_auto_decides_normal() {
    let audio = render(&times(|_| 128.0, 0.173, 40.0), 40.0);
    for opt in [opts(Mode::Dynamic, false), opts(Mode::Auto, true)] {
        let beats = beat_grid_with(&audio, RATE, &opt);
        assert!(beats.len() > 60, "{opt:?}");
        assert!(
            beats.iter().all(|b| b.bpm_x100 == beats[0].bpm_x100),
            "{opt:?}: merged into one segment"
        );
        assert!(
            (beats[0].bpm_x100 as f64 / 100.0 - 128.0).abs() <= 0.1,
            "{opt:?}"
        );
        let period = 60000.0 / 128.0;
        for (i, b) in beats.iter().enumerate() {
            assert!(
                (b.time_ms as f64 - (beats[0].time_ms as f64 + i as f64 * period)).abs() < 2.0,
                "{opt:?}: regular grid"
            );
        }
    }
}

#[test]
fn a_tempo_step_is_two_segments_with_their_own_tempo() {
    let clicks = times(|t| if t < 30.0 { 120.0 } else { 132.0 }, 0.173, 60.0);
    let beats = beat_grid_with(&render(&clicks, 60.0), RATE, &opts(Mode::Dynamic, false));
    assert!(
        beats.len().abs_diff(clicks.len()) <= 2,
        "{} beats for {} clicks",
        beats.len(),
        clicks.len()
    );
    let (early, late) = (mean_bpm(&beats[..20]), mean_bpm(&beats[beats.len() - 20..]));
    assert!(
        (early - 120.0).abs() < 1.0 && (late - 132.0).abs() < 1.0,
        "tempo {early} -> {late}"
    );
    for b in &beats {
        assert!(
            off_by(&clicks, b.time_ms) <= 15.0,
            "beat at {} ms is off the clicks",
            b.time_ms
        );
    }
}

#[test]
fn bpm_range_folds_octaves() {
    let range = |lo: u16, hi: u16| (lo as f64, hi as f64);
    assert_eq!(fold_bpm(174.0, range(70, 140)), 87.0);
    assert_eq!(fold_bpm(60.0, range(70, 140)), 120.0);
    assert_eq!(fold_bpm(140.0, range(70, 140)), 140.0);
    assert_eq!(fold_bpm(87.0, range(85, 175)), 87.0);
    assert_eq!(fold_bpm(200.0, range(85, 175)), 100.0);
    assert_eq!(fold_bpm(50.0, range(110, 220)), 200.0);

    let audio = render(&times(|_| 174.0, 0.173, 30.0), 30.0);
    let at = |lo, hi| {
        beat_grid_with(
            &audio,
            RATE,
            &GridOptions {
                bpm_range: (lo, hi),
                ..Default::default()
            },
        )[0]
        .bpm_x100 as f64
            / 100.0
    };
    assert!((at(85, 175) - 174.0).abs() <= 0.1);
    assert!(
        (at(70, 140) - 87.0).abs() <= 0.1,
        "a 70-140 range reads drum & bass in half time"
    );
}

#[test]
fn modes_and_the_auto_decision() {
    let o = |mode, precision| GridOptions {
        mode,
        precision,
        ..Default::default()
    };
    assert_eq!(
        o(Mode::Auto, false).effective_mode(),
        Mode::Normal,
        "Auto needs high precision"
    );
    assert_eq!(o(Mode::Auto, true).effective_mode(), Mode::Auto);
    assert_eq!(o(Mode::Dynamic, false).effective_mode(), Mode::Dynamic);
    for drifts in [false, true] {
        assert!(!o(Mode::Normal, true).tracks(drifts), "Normal never tracks");
        assert!(
            o(Mode::Dynamic, false).tracks(drifts),
            "Dynamic always tracks"
        );
        assert_eq!(
            o(Mode::Auto, true).tracks(drifts),
            drifts,
            "Auto follows the drift"
        );
        assert!(
            !o(Mode::Auto, false).tracks(drifts),
            "Auto without high precision is Normal"
        );
    }
    assert!(
        !dynamic::drifts(10, 1) && !dynamic::drifts(10, 2),
        "needs a quarter of the windows"
    );
    assert!(dynamic::drifts(8, 2) && dynamic::drifts(10, 3));
    assert!(!dynamic::drifts(1, 1), "and at least two");
}

#[test]
fn high_precision_uses_finer_frames_and_refines_onsets() {
    assert!(
        GridOptions {
            precision: true,
            ..Default::default()
        }
        .flux_hz()
            > GridOptions::default().flux_hz()
    );
    // a flux peak whose right neighbour is larger than its left sits a little right of its frame
    let f = [0.0, 1.0, 4.0, 3.0, 0.0, 0.0, 9.0, 0.0];
    let plain = onsets(&f, false);
    let fine = onsets(&f, true);
    assert_eq!(plain.len(), fine.len());
    assert_eq!(plain[0].0, 2.0);
    assert!(
        fine[0].0 > 2.0 && fine[0].0 < 2.5,
        "{fine:?}: the larger right neighbour pulls the peak right"
    );
    assert_eq!(fine[1].0, 6.0, "a symmetric peak stays on its frame");

    let clicks = times(|_| 140.0, 0.1737, 30.0);
    let beats = beat_grid_with(&render(&clicks, 30.0), RATE, &opts(Mode::Normal, true));
    assert!(
        (beats[0].bpm_x100 as f64 / 100.0 - 140.0).abs() <= 0.05,
        "{}",
        beats[0].bpm_x100
    );
    let period = 60000.0 / 140.0;
    let d = (beats[0].time_ms as f64 - 173.7).rem_euclid(period);
    assert!(d.min(period - d) <= 6.0, "phase off by {d} ms");
}

#[test]
fn segments_merge_noise_and_find_the_step() {
    let fr = 400.0;
    let period = |bpm: f64| 60.0 * fr / bpm;
    // 64 beats at 120 with a little jitter: one segment, one regular grid
    let mut t = 100.0;
    let steady: Vec<f64> = (0..64)
        .map(|k| {
            t += period(120.0);
            t + if k % 2 == 0 { 0.8 } else { -0.8 }
        })
        .collect();
    let (out, bpm) = dynamic::segments(&steady, fr);
    assert!(bpm.iter().all(|&b| b == 12000), "{:?}", &bpm[..4]);
    assert!(
        out.windows(2)
            .all(|w| ((w[1] - w[0]) - period(120.0)).abs() < 0.05)
    );

    // 40 beats at 120 then 40 at 135
    let mut t = 100.0;
    let step: Vec<f64> = (0..80)
        .map(|k| {
            t += period(if k < 40 { 120.0 } else { 135.0 });
            t
        })
        .collect();
    let (out, bpm) = dynamic::segments(&step, fr);
    let edge = bpm.iter().position(|&b| b != bpm[0]).expect("two tempi");
    assert!(edge.abs_diff(40) <= 1, "the change is at beat {edge}");
    assert_eq!((bpm[0], bpm[79]), (12000, 13500));
    assert!(out.iter().zip(&step).all(|(a, b)| (a - b).abs() < 0.1));

    // one beat tracked 3 frames (7.5 ms) late is noise, not a tempo change
    let late: Vec<f64> = (0..64)
        .map(|k| 100.0 + k as f64 * period(120.0) + if k == 30 { 3.0 } else { 0.0 })
        .collect();
    let (_, bpm) = dynamic::segments(&late, fr);
    assert!(bpm.iter().all(|&b| b == bpm[0]), "{bpm:?}");
}

#[test]
fn kick_beats_louder_off_beat_hats() {
    let (period, mut s) = (60.0 / 128.0, vec![0f32; 30 * RATE as usize * 2]);
    for k in 0..60 {
        burst(&mut s, 0.173 + k as f64 * period, 1000.0, 0.5, 90);
        burst(&mut s, 0.173 + (k as f64 + 0.5) * period, 8000.0, 1.0, 90);
    }
    let beats = beat_grid(&s, RATE);
    let d = (beats[0].time_ms as f64 - 173.0).rem_euclid(period * 1000.0);
    assert!(
        d.min(period * 1000.0 - d) <= 10.0,
        "first beat {} ms is off the kick by {d} ms",
        beats[0].time_ms
    );
}

#[test]
fn downbeat_is_the_beat_with_the_most_low_end() {
    let (period, mut s) = (60.0 / 128.0, vec![0f32; 30 * RATE as usize * 2]);
    for k in 0..60 {
        let t = 0.173 + k as f64 * period;
        burst(&mut s, t, 1000.0, 0.5, 90);
        if k % 4 == 2 {
            burst(&mut s, t, 60.0, 0.9, 4000);
        }
    }
    for b in beat_grid(&s, RATE).iter().filter(|b| b.time_ms < 28000) {
        let k = ((b.time_ms as f64 / 1000.0 - 0.173) / period).round() as i64;
        assert_eq!(
            b.bar_pos == 1,
            k % 4 == 2,
            "beat {k} at {} ms has bar position {}",
            b.time_ms,
            b.bar_pos
        );
    }
}

#[test]
fn silence_has_no_grid() {
    assert!(beat_grid(&vec![0.0; RATE as usize * 20], RATE).is_empty());
}
