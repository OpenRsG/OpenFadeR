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
fn rolling_flux_matches_the_direct_window_definition() {
    let fr = 400.0;
    let (smooth, look) = (4usize, 10usize);
    let e: [Vec<f32>; 3] = std::array::from_fn(|b| {
        (0..600)
            .map(|i| ((i * (b + 3)) % 17) as f32 * 0.01 + if i % 50 == 0 { 2.0 } else { 0.0 })
            .collect()
    });
    let lg = |v: f32| (1.0 + ENERGY_GAIN * v).ln();
    let direct: Vec<f32> = (0..600)
        .map(|i: usize| {
            e.iter()
                .map(|b| {
                    let s = |i: usize| {
                        lg(b[(i + 1).saturating_sub(smooth)..=i].iter().sum::<f32>()
                            / smooth as f32)
                    };
                    let m = (i.saturating_sub(look)..i).map(s).fold(0.0f32, f32::max);
                    (s(i) - m).max(0.0)
                })
                .sum()
        })
        .collect();
    let fast = flux(&e, fr);
    for (i, (a, b)) in direct.iter().zip(&fast).enumerate() {
        assert!((a - b).abs() < 1e-4, "hop {i}: {a} vs {b}");
    }
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
fn normal_mode_keeps_one_tempo_on_a_ramp() {
    let audio = render(&times(|t| 120.0 + 6.0 * t / 60.0, 0.173, 60.0), 60.0);
    for opt in [opts(Mode::Normal, false)] {
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
    for opt in [
        opts(Mode::Dynamic, false),
        opts(Mode::Auto, true),
        opts(Mode::Auto, false),
        GridOptions::default(),
    ] {
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

/// Runs of equal tempo as `(first beat, bpm_x100)`.
fn sections(beats: &[Beat]) -> Vec<(usize, u16)> {
    let mut s: Vec<(usize, u16)> = Vec::new();
    for (i, b) in beats.iter().enumerate() {
        if s.last().map(|x| x.1) != Some(b.bpm_x100) {
            s.push((i, b.bpm_x100));
        }
    }
    s
}

#[test]
fn a_tempo_change_at_bar_33_is_two_sections_within_a_millisecond() {
    // 120 BPM for 32 bars (64 s), then 128 BPM
    let clicks = times(|t| if t < 64.0 { 120.0 } else { 128.0 }, 0.173, 120.0);
    let audio = render(&clicks, 120.0);
    for opt in [
        GridOptions::default(),
        opts(Mode::Auto, false),
        opts(Mode::Auto, true),
    ] {
        let beats = beat_grid_with(&audio, RATE, &opt);
        assert_eq!(beats.len(), clicks.len(), "{opt:?}");
        let s = sections(&beats);
        assert_eq!(
            s.iter().map(|x| x.0).collect::<Vec<_>>(),
            [0, 128],
            "{opt:?}: two sections, the second from bar 33: {s:?}"
        );
        assert!(
            s[0].1.abs_diff(12000) <= 10 && s[1].1.abs_diff(12800) <= 10,
            "{opt:?}: {s:?}"
        );
        for (b, t) in beats.iter().zip(&clicks) {
            let d = (b.time_ms as f64 - t * 1000.0).abs();
            assert!(d <= 1.0, "{opt:?}: beat at {} ms is {d} ms off", b.time_ms);
        }
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

/// A house groove at `bpm` from `first` seconds: a kick (60 Hz) every `kick_beats` beats, hats (6 kHz) on every eighth and a clap
/// (1.5 kHz) on beats 2 and 4. `kick_beats` 1.5 is the dotted (3-against-4) bass pattern of much tech house.
fn groove(bpm: f64, kick_beats: f64, first: f64, secs: f64) -> Vec<f32> {
    let mut s = vec![0f32; (secs * RATE as f64) as usize * 2];
    let beat = 60.0 / bpm;
    let mut t = first;
    while t < secs - 0.2 {
        burst(&mut s, t, 60.0, 0.9, 6000);
        t += kick_beats * beat;
    }
    let mut k = 0;
    let mut t = first;
    while t < secs - 0.2 {
        burst(&mut s, t, 6000.0, 0.25, 300);
        if k % 4 == 2 {
            burst(&mut s, t, 1500.0, 0.5, 1500);
        }
        k += 1;
        t += beat / 2.0;
    }
    s
}

#[test]
fn dotted_kick_pattern_reads_the_beat_not_the_kick_period() {
    // Kicks every 1.5 beats of 128 BPM are 85.33 BPM on their own; the hats and claps say 128, and 85 is inside the default
    // range, so the fold of octaves alone cannot save it (issue 43, "Overflow").
    for first in [0.0, 0.173] {
        let audio = groove(128.0, 1.5, first, 40.0);
        for opt in [GridOptions::default(), opts(Mode::Normal, true)] {
            let beats = beat_grid_with(&audio, RATE, &opt);
            let got = beats.first().map_or(0.0, |b| b.bpm_x100 as f64 / 100.0);
            assert!(
                (got - 128.0).abs() <= 0.15,
                "{opt:?} first {first}: detected {got}"
            );
        }
    }
    // The plain four-on-the-floor groove still reads straight.
    let beats = beat_grid(&groove(124.0, 1.0, 0.173, 40.0), RATE);
    let got = beats.first().map_or(0.0, |b| b.bpm_x100 as f64 / 100.0);
    assert!((got - 124.0).abs() <= 0.15, "detected {got}");
}

#[test]
fn a_fold_with_thirds_and_an_empty_half_is_the_dotted_reading() {
    // 60 bins: the kick on 0, hats/claps on the thirds (20, 40), nothing on the half (30).
    let mut dotted_fold = vec![0.0; 60];
    dotted_fold[0] = 10.0;
    dotted_fold[20] = 2.0;
    dotted_fold[40] = 2.0;
    assert!(dotted(&dotted_fold));
    // The straight reading: the half carries the hats.
    let mut straight = vec![0.0; 60];
    straight[0] = 10.0;
    straight[30] = 4.0;
    assert!(!dotted(&straight));
    // A shuffle: thirds and a half both carry weight.
    straight[40] = 3.0;
    assert!(!dotted(&straight));
    assert!(!dotted(&[1.0; 6]), "too short to split");
}

#[test]
fn no_beats_in_the_silence_before_the_music_and_the_first_lands_on_the_kick() {
    // 1.3 s of silence, then the groove: the grid starts at the first kick, not on extrapolated beats in the silence,
    // and the first beat is within 3 ms of the kick's onset (issue 43).
    for first in [1.3, 2.05] {
        let audio = groove(126.0, 1.0, first, 40.0);
        for opt in [
            opts(Mode::Normal, false),
            opts(Mode::Normal, true),
            GridOptions::default(),
        ] {
            let beats = beat_grid_with(&audio, RATE, &opt);
            let t0 = beats.first().map_or(0, |b| b.time_ms) as f64;
            assert!(
                (t0 - first * 1000.0).abs() <= 3.0,
                "{opt:?} first {first}: first beat at {t0} ms"
            );
        }
    }
}

/// Onsets (400 Hz frames) every `step` frames for 60 s, `weight(k)` for the k-th.
fn pattern(step: f64, weight: impl Fn(usize) -> f64) -> Vec<(f64, f64)> {
    (0..(24_000.0 / step) as usize)
        .map(|k| (70.0 + k as f64 * step, weight(k)))
        .collect()
}

#[test]
fn octave_follows_the_onset_pattern_inside_the_range() {
    let fr = 400.0;
    // 300 frames = 80 BPM, 150 frames = 160 BPM, 600 frames = 40 BPM
    let wide = (60.0, 180.0);
    // a kick on every 80 BPM beat and faint hats between: the beat is 80, whichever reading came in
    let faint = pattern(150.0, |k| if k % 2 == 0 { 1.0 } else { 0.1 });
    assert_eq!(octave(&faint, 300.0, fr, wide), 300.0);
    assert_eq!(octave(&faint, 150.0, fr, wide), 300.0, "the double-time reading is dropped");
    // hats as strong as the kick: the pattern is a plain 160 BPM pulse, half time (80) would keep half the onsets
    let even = pattern(150.0, |_| 1.0);
    assert_eq!(octave(&even, 300.0, fr, wide), 150.0, "a half-time reading of an even pulse doubles");
    assert_eq!(octave(&even, 150.0, fr, wide), 150.0);
    // onsets only every second 160 BPM beat: 80 BPM
    let sparse = pattern(300.0, |_| 1.0);
    assert_eq!(octave(&sparse, 150.0, fr, wide), 300.0);
    // the prior stops the slide to ever slower readings: 40 BPM scores twice as high on onsets every 600 frames without it
    let slow = pattern(600.0, |_| 1.0);
    assert_eq!(octave(&slow, 300.0, fr, (30.0, 180.0)), 300.0);
    // the range is the user's: the 160 BPM reading of the even pulse is outside 70-140, so it stays at 80
    assert_eq!(octave(&even, 300.0, fr, (70.0, 140.0)), 300.0);
    // an octave-wide range around the edge: 174 and 87 BPM readings of plain beats keep their own pulse
    let plain = |bpm: f64| pattern(60.0 * fr / bpm, |_| 1.0);
    for bpm in [87.0, 174.0] {
        let p = 60.0 * fr / bpm;
        assert_eq!(octave(&plain(bpm), p, fr, (85.0, 175.0)), p, "{bpm}");
    }
}

#[test]
fn modes_and_the_auto_decision() {
    let o = |mode, precision| GridOptions {
        mode,
        precision,
        ..Default::default()
    };
    assert_eq!(GridOptions::default().mode, Mode::Auto);
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
        assert_eq!(
            o(Mode::Auto, false).tracks(drifts),
            drifts,
            "Auto needs no high precision"
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
fn kick_beats_a_louder_off_beat_bass_stab() {
    // Dance-track bass: a 55 Hz kick on the beat, a mid-heavy bass stab on every off-beat (the mid band rises there).
    let (period, mut s) = (60.0 / 128.0, vec![0f32; 30 * RATE as usize * 2]);
    for k in 0..60 {
        let t = 0.173 + k as f64 * period;
        burst(&mut s, t, 55.0, 0.9, 4000);
        burst(&mut s, t + period / 2.0, 400.0, 0.6, 3000);
        burst(&mut s, t + period / 2.0, 800.0, 0.4, 3000);
    }
    let beats = beat_grid(&s, RATE);
    let d = (beats[0].time_ms as f64 - 173.0).rem_euclid(period * 1000.0);
    assert!(
        d.min(period * 1000.0 - d) <= 10.0,
        "first beat {} ms is off the kick by {d} ms",
        beats[0].time_ms
    );
}

/// Asserts that, in `beats` of a 128 BPM track whose grid starts at 0.173 s, exactly the beats `bar1 + 4n` are bar position 1.
fn assert_bar_starts(beats: &[Beat], bar1: i64) {
    assert!(beats.len() > 100, "{} beats", beats.len());
    for b in beats.iter().filter(|b| b.time_ms < 60000) {
        let k = ((b.time_ms as f64 / 1000.0 - 0.173) / (60.0 / 128.0)).round() as i64;
        assert_eq!(
            b.bar_pos == 1,
            (k - bar1).rem_euclid(4) == 0,
            "beat {k} at {} ms has bar position {}",
            b.time_ms,
            b.bar_pos
        );
    }
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
    let beats = beat_grid(&s, RATE);
    for b in beats.iter().filter(|b| b.time_ms < 28000) {
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

/// 64 s of 128 BPM four-on-the-floor at 0.173 s: the same 60 Hz kick and 1 kHz click on every beat from beat `first`, so no
/// beat has more low end than another. From beat `section_at` the music changes every 8 bars: an 8 kHz hat on the off-beats
/// in the odd sections, none in the even ones.
fn floor_track(first: usize, section_at: usize) -> Vec<f32> {
    let (period, mut s) = (60.0 / 128.0, vec![0f32; 64 * RATE as usize * 2]);
    for k in first..136 {
        let t = 0.173 + k as f64 * period;
        burst(&mut s, t, 1000.0, 0.5, 90);
        burst(&mut s, t, 60.0, 0.9, 4000);
        if k >= section_at && (k - section_at) / 32 % 2 == 1 {
            burst(&mut s, t + period / 2.0, 8000.0, 0.5, 90);
        }
    }
    s
}

#[test]
fn equal_kicks_leave_the_bar_to_the_start_of_the_music() {
    // Grid beat 0 is silent: the music starts on beat 2, so that is bar 1.
    assert_bar_starts(&beat_grid(&floor_track(2, usize::MAX), RATE), 2);
}

#[test]
fn equal_kicks_leave_the_bar_to_the_section_changes() {
    // The music starts on beat 0, but every change of section (a hat layer comes and goes every 8 bars) lands on beat 2: a
    // two-beat pickup, so the changes outweigh the weak preference for the start.
    assert_bar_starts(&beat_grid(&floor_track(0, 2), RATE), 2);
}

#[test]
fn silence_has_no_grid() {
    assert!(beat_grid(&vec![0.0; RATE as usize * 20], RATE).is_empty());
}

fn reference_score(onsets: &[(f64, f64)], q: f64, bins: &mut Vec<f64>) -> f64 {
    fold(onsets, q, bins);
    let top = (0..bins.len()).map(|j| win3(bins, j)).fold(0.0, f64::max);
    top * top * q
}

#[test]
fn bounded_scores_preserve_exhaustive_decisions_and_rounding() {
    let mut state = 17u64;
    for len in [0, 3, 511, 512, 2048] {
        let on: Vec<_> = (0..len)
            .map(|i| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let jitter = (state >> 32) as f64 / u32::MAX as f64;
                let w = 2.0f64.powi((state % 90) as i32 - 45);
                (i as f64 * 137.13 + jitter, w)
            })
            .collect();
        let bound = ScoreBound::new(&on);
        for q in [0.75, 1.0, 2.0, 2.00001, 137.0, 200.5, 499.999] {
            let exact = reference_score(&on, q, &mut Vec::new());
            assert_eq!(score(&on, q, &mut Vec::new()).to_bits(), exact.to_bits());
            for best in [0.0, exact.next_down(), exact, exact.next_up(), exact * 4.0] {
                match bound.score(&on, q, &mut Vec::new(), best) {
                    Some(got) => assert_eq!(got.to_bits(), exact.to_bits(), "len={len}, q={q}"),
                    None => assert!(exact <= best, "rejected winner: len={len}, q={q}"),
                }
            }
        }
    }
}

fn flux_fixture(secs: usize, fr: f64, bpm: f64, ambiguous: bool) -> Vec<f32> {
    let mut f = vec![0.0f32; (secs as f64 * fr) as usize];
    let mut t = 0.173 * fr;
    let p = 60.0 * fr / bpm;
    let mut k = 0;
    while t + p < f.len() as f64 {
        let at = t.round() as usize;
        f[at] = if ambiguous && k % 2 != 0 { 0.7 } else { 1.0 };
        f[at - 1] = 0.13;
        f[at + 1] = 0.21;
        if ambiguous {
            let off = (t + p * 0.5).round() as usize;
            f[off] = 0.69;
            f[off + 1] = 0.17;
        }
        t += p;
        k += 1;
    }
    f
}

#[test]
fn exact_tempo_search_matches_reference_at_both_precisions_and_octaves() {
    for precision in [false, true] {
        let opt = opts(Mode::Normal, precision);
        let fr = RATE as f64 / (RATE / opt.flux_hz()) as f64;
        for (bpm, ambiguous) in [(120.0, false), (140.25, true), (174.0, true)] {
            let f = flux_fixture(24, fr, bpm, ambiguous);
            let on = onsets(&f, precision);
            for range in [(85, 175), (70, 140), (110, 220)] {
                let opt = GridOptions {
                    bpm_range: range,
                    ..opt
                };
                let reference = tempo_search(&f, fr, &opt, &|| false, |q, bins, _| {
                    Some(reference_score(&on, q, bins))
                });
                assert_eq!(
                    tempo(&f, &on, fr, &opt, &|| false),
                    reference,
                    "{bpm}, {opt:?}"
                );
            }
        }
    }
    let f = flux_fixture(330, 400.0, 120.0, false);
    let on = onsets(&f, false);
    assert!(on.len() >= 512);
    let opt = GridOptions::default();
    let reference = tempo_search(&f, 400.0, &opt, &|| false, |q, bins, _| {
        Some(reference_score(&on, q, bins))
    });
    assert_eq!(tempo(&f, &on, 400.0, &opt, &|| false), reference);
    let bound = ScoreBound::new(&on);
    let incumbent = reference_score(&on, 200.0, &mut Vec::new());
    assert!(
        bound
            .score(&on, 190.0, &mut Vec::new(), incumbent)
            .is_none()
    );
}

#[test]
fn tempo_search_cancels_inside_autocorrelation_and_fine_search() {
    use std::cell::Cell;
    let f = flux_fixture(120, 400.0, 120.0, false);
    let opt = GridOptions::default();
    let polls = Cell::new(0);
    let evaluated = Cell::new(0);
    let cancelled = || {
        polls.set(polls.get() + 1);
        polls.get() >= 3
    };
    let result = tempo_search(&f, 400.0, &opt, &cancelled, |_, _, _| {
        evaluated.set(evaluated.get() + 1);
        Some(1.0)
    });
    assert_eq!(result, None);
    assert_eq!(evaluated.get(), 0);
    assert!(polls.get() >= 3);

    let on = onsets(&f, false);
    let cancelled = || evaluated.get() >= 3;
    let result = tempo_search(&f, 400.0, &opt, &cancelled, |q, bins, _| {
        evaluated.set(evaluated.get() + 1);
        Some(reference_score(&on, q, bins))
    });
    assert_eq!(result, None);
    assert_eq!(evaluated.get(), 3);
}

#[test]
#[ignore = "scaling benchmark; run optimized with --ignored --nocapture"]
fn benchmark_exact_fine_search_scaling() {
    use std::{hint::black_box, time::Instant};
    for fr in [400.0, 1000.0] {
        for secs in [30, 120, 480, 1800] {
            let n = (secs as f64 * fr) as usize;
            let f = flux_fixture(secs, fr, 120.0, true);
            let on = onsets(&f, true);
            let original = on;
            for no_prune in [false, true] {
                let mut on = original.clone();
                let mut thresholds = Vec::new();
                if no_prune {
                    // A dominant transient keeps every candidate concentrated. Incumbents just
                    // below exact scores force the bound check to do work without rejecting.
                    on[0].1 = on.len() as f64 * 4.0;
                    let (mut r, mut bins) = (-0.006, Vec::new());
                    while r <= 0.006 {
                        let q = 60.0 * fr / (120.0 * (1.0 + r));
                        thresholds.push(reference_score(&on, q, &mut bins) * 0.99);
                        r += 1.5 / n as f64;
                    }
                }
                let bound = ScoreBound::new(&on);
                let run = |bounded: bool| {
                    let (mut best, mut bins, mut r) = ((0.0f64, 0.0f64), Vec::new(), -0.006);
                    let (mut candidates, mut pruned) = (0, 0);
                    while r <= 0.006 {
                        let q = 60.0 * fr / (120.0 * (1.0 + r));
                        let incumbent = if no_prune {
                            thresholds[candidates]
                        } else {
                            best.0
                        };
                        let s = if bounded {
                            bound.score(black_box(&on), q, &mut bins, incumbent)
                        } else {
                            Some(reference_score(black_box(&on), q, &mut bins))
                        };
                        if let Some(s) = s {
                            if s > best.0 {
                                best = (s, q);
                            }
                        } else {
                            pruned += 1;
                        }
                        candidates += 1;
                        r += 1.5 / n as f64;
                    }
                    (best, candidates, pruned)
                };
                let start = Instant::now();
                let reference = run(false);
                let reference_time = start.elapsed();
                let start = Instant::now();
                let optimized = run(true);
                let optimized_time = start.elapsed();
                assert_eq!(reference.0, optimized.0);
                assert_eq!(reference.1, optimized.1);
                if no_prune {
                    assert_eq!(optimized.2, 0);
                }
                eprintln!(
                    "fine-search fr={fr} secs={secs} no-prune={no_prune} onsets={} candidates={} pruned={} reference={reference_time:?} optimized={optimized_time:?}",
                    on.len(),
                    optimized.1,
                    optimized.2
                );
            }
        }
    }
}

/// Annotated clips under `OPENFADER_DOWNBEAT_AUDIO` (wav/mp3, any depth) with `<stem>.beats` files (`seconds beat-number` per
/// line) in `OPENFADER_DOWNBEAT_ANN`: `[genre, [accent, novelty, start] scores, true bar phase]` of every clip whose detected
/// beats match the annotated ones (within 70 ms, over 90% of them) and whose bar has 4 beats.
fn annotated_clips() -> Vec<(String, [[f64; 4]; 3], usize)> {
    let (Ok(audio), Ok(ann)) = (
        std::env::var("OPENFADER_DOWNBEAT_AUDIO"),
        std::env::var("OPENFADER_DOWNBEAT_ANN"),
    ) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    let mut dirs = vec![std::path::PathBuf::from(audio)];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "wav" || x == "mp3") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut out = Vec::new();
    for p in files {
        let genre = p
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let Ok(text) = std::fs::read_to_string(
            std::path::Path::new(&ann)
                .join(p.file_stem().unwrap())
                .with_extension("beats"),
        ) else {
            continue;
        };
        let truth: Vec<(f64, i64)> = text
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
            })
            .collect();
        if truth.iter().map(|t| t.1).max() != Some(4) {
            continue;
        }
        let pcm = crate::audio::decode(&p).unwrap();
        let found = beat_grid(&pcm.samples, pcm.rate);
        let (mut votes, mut matched) = ([0usize; 4], 0);
        for (k, b) in found.iter().enumerate() {
            let t = b.time_ms as f64 / 1000.0;
            let near = truth
                .iter()
                .min_by(|x, y| (x.0 - t).abs().total_cmp(&(y.0 - t).abs()))
                .unwrap();
            if (near.0 - t).abs() <= 0.07 {
                matched += 1;
                votes[(k as i64 - (near.1 - 1)).rem_euclid(4) as usize] += 1;
            }
        }
        if found.is_empty() || (matched as f64) < 0.9 * found.len() as f64 {
            continue;
        }
        let (e, fr) = hops(&pcm.samples, pcm.rate, GridOptions::default().flux_hz());
        let amp = e.map(|b| b.iter().map(|v| v.sqrt()).collect::<Vec<f32>>());
        let times: Vec<f64> = found
            .iter()
            .map(|b| b.time_ms as f64 / 1000.0 * fr)
            .collect();
        let phase = (0..4).max_by_key(|&c| votes[c]).unwrap();
        out.push((genre, phase_scores(&amp, &times, fr), phase));
    }
    out
}

/// Bar phase accuracy on an annotated set (see [`annotated_clips`]) for the accent alone (the detector before the novelty
/// and start evidence), for each evidence alone, and for the weights in use.
#[test]
#[ignore]
fn measure_bar_phase_on_annotated_set() {
    let clips = annotated_clips();
    let pick = |s: [f64; 4]| (0..4).max_by(|&a, &b| s[a].total_cmp(&s[b])).unwrap();
    let acc = |f: &dyn Fn(&[[f64; 4]; 3]) -> [f64; 4], of: &dyn Fn(usize) -> bool| {
        let set: Vec<_> = clips
            .iter()
            .enumerate()
            .filter(|(i, _)| of(*i))
            .map(|(_, c)| c)
            .collect();
        let ok = set.iter().filter(|c| pick(f(&c.1)) == c.2).count();
        format!(
            "{ok}/{} = {:.1}%",
            set.len(),
            100.0 * ok as f64 / set.len().max(1) as f64
        )
    };
    let all = |_: usize| true;
    eprintln!("clips {}", clips.len());
    eprintln!("accent only (before): {}", acc(&|s| s[0], &all));
    eprintln!("novelty only: {}", acc(&|s| s[1], &all));
    eprintln!("start only: {}", acc(&|s| s[2], &all));
    for w in [0.0, 0.25, 0.5, 1.0, 2.0] {
        let f = |s: &[[f64; 4]; 3]| std::array::from_fn(|c| s[0][c] + w * s[1][c]);
        eprintln!(
            "accent + {w} novelty: all {} | even {} | odd {}",
            acc(&f, &all),
            acc(&f, &|i| i % 2 == 0),
            acc(&f, &|i| i % 2 == 1)
        );
    }
    let f = |s: &[[f64; 4]; 3]| {
        std::array::from_fn(|c| s[0][c] + NOVELTY_W * s[1][c] + START_W * s[2][c])
    };
    eprintln!("weights in use: {}", acc(&f, &all));
    // Paired: clips the new evidence fixes vs breaks (an exact sign test on these decides whether the gain is chance).
    let (fixed, broken) = clips.iter().fold((0, 0), |(fx, br), c| {
        match (pick(c.1[0]) == c.2, pick(f(&c.1)) == c.2) {
            (false, true) => (fx + 1, br),
            (true, false) => (fx, br + 1),
            _ => (fx, br),
        }
    });
    eprintln!("paired vs accent only: fixed {fixed}, broken {broken}");
    // Held out: the weights chosen on the other folds, scored on each fold in turn (k-fold by clip index).
    let score = |wn: f64, ws: f64, c: &(String, [[f64; 4]; 3], usize)| {
        pick(std::array::from_fn(|p| {
            c.1[0][p] + wn * c.1[1][p] + ws * c.1[2][p]
        })) == c.2
    };
    for k in [2usize, 5] {
        let mut held = 0;
        for fold in 0..k {
            let mut best = (0usize, 0.0, 0.0);
            for wn in [0.0, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0] {
                for ws in [0.0, 0.05, 0.1, 0.15, 0.25, 0.5] {
                    let ok = (clips.iter().enumerate())
                        .filter(|(i, c)| i % k != fold && score(wn, ws, c))
                        .count();
                    if ok > best.0 {
                        best = (ok, wn, ws);
                    }
                }
            }
            held += (clips.iter().enumerate())
                .filter(|(i, c)| i % k == fold && score(best.1, best.2, c))
                .count();
            eprintln!(
                "{k}-fold {fold}: chosen novelty {} start {}",
                best.1, best.2
            );
        }
        eprintln!(
            "{k}-fold held out: {held}/{} = {:.1}%",
            clips.len(),
            100.0 * held as f64 / clips.len().max(1) as f64
        );
    }
    let mut genres: Vec<_> = clips.iter().map(|c| c.0.clone()).collect();
    genres.dedup();
    genres.sort();
    genres.dedup();
    for g in genres {
        let of = |i: usize| clips[i].0 == g;
        let f0 = |s: &[[f64; 4]; 3]| s[0];
        eprintln!("{g}: before {} | now {}", acc(&f0, &of), acc(&f, &of));
    }
}
