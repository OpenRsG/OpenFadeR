//! The bundled plugins' effects (`extension/plugin/`, JSON only), through the host: the tests of the native crates they
//! replaced, ported unchanged, so each keeps the behaviour it was tested for.

use super::Slot::{Beat, Color, Fader, Lever, Mic, Send};
use super::kit::*;
use crate::audio::fx::{Ctx, Frame};

#[test]
fn every_bundled_effect_is_listed_in_its_slot_in_plugin_order() {
    if !have_bundled() {
        return;
    }
    let names = |slot| -> Vec<&str> {
        bundled_all()
            .iter()
            .filter(|e| e.slot == slot)
            .map(|e| e.name.as_str())
            .collect()
    };
    let has = |slot, want: &[&str]| {
        let got = names(slot);
        assert!(want.iter().all(|w| got.contains(w)), "{slot:?}: {got:?}");
    };
    has(
        Beat,
        &[
            "PING PONG",
            "SPIRAL",
            "TRIPLET ROLL",
            "TRIPLET LPF",
            "TRIPLET HPF",
            "HELIX",
            "MOBIUS",
        ],
    );
    has(Mic, &["ECHO", "PITCH", "MEGAPHONE", "REVERB"]);
    has(
        Send,
        &[
            "SHORT DELAY",
            "REVERB",
            "SHIMMER",
            "TAPE ECHO",
            "PING PONG",
            "ECHO-VERB",
        ],
    );
    has(
        Lever,
        &["ECHO", "REVERB", "JUGGLE", "REVERSE", "SOLO", "STRETCH"],
    );
    has(Color, &["CROSS-PASS"]);
    has(Fader, &["SOFT"]);
}

// ---- beat-fx ----

#[test]
fn ping_pong_bounces_an_impulse_to_the_other_side_one_period_later() {
    if !have_bundled() {
        return;
    }
    let y = run(
        &mut *bundled(Beat, "PING PONG"),
        &ctx(1.0, 1.0, 0.5),
        &impulse(80_000, 0, [1.0, 0.0]),
    );
    // The input is in the left channel: the repeats alternate right, left, right, one period apart.
    for (k, side) in [(1, 1), (2, 0), (3, 1)] {
        let at = 24_000 * k;
        assert!(
            (y[at][side] - 0.5f32.powi(k as i32 - 1) * 0.5).abs() < 1e-6,
            "repeat {k}: {:?}",
            y[at]
        );
        assert_eq!(y[at][1 - side], 0.0);
    }
}

#[test]
fn spiral_repeats_at_the_period_then_smears_and_rings_after_off() {
    if !have_bundled() {
        return;
    }
    let y = off_response(&mut *bundled(Beat, "SPIRAL"), 1.0, 0.5, [1.0, 1.0], 100_000);
    let first = peak_at(&y[10_000..40_000]) + 10_000;
    assert!(first.abs_diff(24_000) < 20, "first repeat at {first}");
    // The second repeat went through the diffusers: lower and wider than the first.
    let second = &y[48_000 - 200..48_000 + 200];
    assert!(second.iter().map(|f| f[0].abs()).fold(0.0, f32::max) < 0.5 * y[first][0].abs());
    assert!(
        rms(&y[72_000..96_000]) > 1e-4,
        "the wash keeps going after g dropped"
    );
}

#[test]
fn spiral_feedback_cannot_run_away() {
    if !have_bundled() {
        return;
    }
    assert_stable(Beat, "SPIRAL", &ctx(1.0, 1.0, 0.5), 3.0);
}

#[test]
fn triplet_roll_repeats_the_captured_slice_every_two_thirds_of_the_period() {
    if !have_bundled() {
        return;
    }
    let c = ctx(1.0, 1.0, 0.5);
    let ramp: Vec<Frame> = (0..50_000).map(|i| [i as f32 * 1e-5, 0.0]).collect();
    let y = run(&mut *bundled(Beat, "TRIPLET ROLL"), &c, &ramp);
    assert_eq!(
        y[100][0], ramp[100][0],
        "the slice is heard live while it records"
    );
    for k in [1usize, 2] {
        let at = 16_000 * k + 123;
        assert!(
            (y[at][0] - ramp[123][0]).abs() < 1e-4,
            "repeat {k}: {} vs {}",
            y[at][0],
            ramp[123][0]
        );
    }
}

#[test]
fn triplet_filters_sweep_with_the_triplet_period_and_start_open() {
    if !have_bundled() {
        return;
    }
    let loudest = |y: &[Frame], at: usize| {
        y[at..at + 500]
            .iter()
            .map(|f| f[0].abs())
            .fold(0.0, f32::max)
    };
    // The cycle is 16 000 frames: fully open at 0 and 16 000, fully closed at 8 000 (LPF 150 Hz, HPF 8 kHz).
    let y = run(
        &mut *bundled(Beat, "TRIPLET LPF"),
        &ctx(1.0, 1.0, 0.5),
        &sine(5000.0, 0.5, 17_000),
    );
    assert!(
        loudest(&y, 7800) < 0.05,
        "closed low-pass: {}",
        loudest(&y, 7800)
    );
    assert!(loudest(&y, 16_000) > 0.4, "open again one triplet later");
    let y = run(
        &mut *bundled(Beat, "TRIPLET HPF"),
        &ctx(1.0, 1.0, 0.5),
        &sine(100.0, 0.5, 17_000),
    );
    assert!(
        loudest(&y, 7800) < 0.05,
        "closed high-pass: {}",
        loudest(&y, 7800)
    );
    assert!(loudest(&y, 16_000) > 0.4, "open again one triplet later");
}

#[test]
fn helix_loops_the_capture_and_each_pass_climbs_a_semitone() {
    if !have_bundled() {
        return;
    }
    let mut c = ctx(1.0, 1.0, 0.5);
    c.period = 8000.0;
    let y = run(
        &mut *bundled(Beat, "HELIX"),
        &c,
        &sine(1000.0, 0.5, 8000 + 8000 + 7000 + 100),
    );
    let pass = |from: usize, to: usize| peak_hz(&y[from..to], 900.0, 1300.0);
    // Capture [0, 8000) is heard live; pass 0 plays at the original speed, pass 1 a semitone faster.
    assert!(
        (pass(8100, 15_900) - 1000.0).abs() < 8.0,
        "pass 0: {}",
        pass(8100, 15_900)
    );
    let want = 1000.0 * 2f32.powf(1.0 / 12.0);
    assert!(
        (pass(16_100, 22_800) - want).abs() < 12.0,
        "pass 1: {} want {want}",
        pass(16_100, 22_800)
    );
}

#[test]
fn mobius_sweeps_endlessly_once_per_period_and_leaves_level_zero_alone() {
    if !have_bundled() {
        return;
    }
    let mut c = ctx(1.0, 1.0, 0.5);
    c.period = 12_000.0;
    let x = sine(1000.0, 0.3, 5 * 12_000);
    let y = run(&mut *bundled(Beat, "MOBIUS"), &c, &x);
    let env = |at: usize| rms(&y[at..at + 300]);
    // A tone is met by one filter per period, so the level repeats with the period and swings within it.
    for k in 2..4 {
        for off in [0, 3000, 6000, 9000] {
            let (a, b) = (env(12_000 * k + off), env(12_000 * (k + 1) + off));
            assert!(
                (a - b).abs() < 0.05 * a.max(b),
                "period {k}+{off}: {a} vs {b}"
            );
        }
    }
    let levels: Vec<f32> = (0..24).map(|i| env(24_000 + 500 * i)).collect();
    let (lo, hi) = levels
        .iter()
        .fold((f32::MAX, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    assert!(hi > 1.5 * lo, "swings from {lo} to {hi}");
    assert_eq!(
        run(&mut *bundled(Beat, "MOBIUS"), &ctx(0.0, 1.0, 0.5), &x)[..50],
        x[..50]
    );
}

// ---- mic-fx ----

#[test]
fn mic_echo_repeats_after_the_knob_time_and_rings_after_off() {
    if !have_bundled() {
        return;
    }
    let y = off_response(&mut *bundled(Mic, "ECHO"), 1.0, 0.5, [1.0, 0.0], 60_000);
    // knob 0.5 = 0.45 s = 21 600 frames; the second repeat is lower (feedback and low-pass).
    assert!(y[21_599..21_602].iter().any(|f| f[0] > 0.9), "first repeat");
    assert!(
        y[43_199..43_202].iter().any(|f| f[0] > 0.01 && f[0] < 0.45),
        "second repeat"
    );
    assert_eq!(y[1000], [0.0; 2], "nothing before the delay");
    let dry = run(
        &mut *bundled(Mic, "ECHO"),
        &ctx(0.0, 1.0, 0.5),
        &sine(300.0, 0.5, 40_000),
    );
    assert_eq!(dry, sine(300.0, 0.5, 40_000), "LEVEL 0: only the dry mic");
}

#[test]
fn mic_pitch_moves_a_tone_by_the_knob_and_leaves_the_centre_alone() {
    if !have_bundled() {
        return;
    }
    let tone = sine(1000.0, 0.5, 24_000);
    for (knob, want) in [(1.0, 2000.0), (0.0, 500.0), (0.75, 1414.2)] {
        let y = run(&mut *bundled(Mic, "PITCH"), &ctx(1.0, 1.0, knob), &tone);
        let found = peak_hz(&y[12_000..], 0.6 * want, 1.4 * want);
        assert!(
            (found - want).abs() < 0.03 * want,
            "knob {knob}: {found} Hz, wanted {want}"
        );
    }
    assert_eq!(
        run(&mut *bundled(Mic, "PITCH"), &ctx(1.0, 1.0, 0.5), &tone),
        tone
    );
}

#[test]
fn megaphone_keeps_the_middle_and_drops_100_hz_and_8_khz() {
    if !have_bundled() {
        return;
    }
    let c = ctx(1.0, 1.0, 0.0);
    let mid = gain(&mut *bundled(Mic, "MEGAPHONE"), &c, 1500.0);
    for hz in [100.0, 8000.0] {
        let g = gain(&mut *bundled(Mic, "MEGAPHONE"), &c, hz);
        assert!(g < 0.1 * mid, "{hz} Hz: {g} against the middle's {mid}");
    }
    assert!(mid > 0.5, "middle {mid}");
    // The drive clips: a loud tone does not grow with the input.
    let loud = run(
        &mut *bundled(Mic, "MEGAPHONE"),
        &ctx(1.0, 1.0, 1.0),
        &sine(1500.0, 1.0, 24_000),
    );
    assert!(loud.iter().all(|f| f[0].abs() < 0.5));
}

#[test]
fn mic_reverb_size_lengthens_the_tail_and_level_zero_is_dry() {
    if !have_bundled() {
        return;
    }
    let tail = |knob| {
        let y = off_response(&mut *bundled(Mic, "REVERB"), 1.0, knob, [1.0, 1.0], 96_000);
        rms(&y[72_000..96_000])
    };
    assert!(
        tail(1.0) > 4.0 * tail(0.0),
        "{} against {}",
        tail(1.0),
        tail(0.0)
    );
    let y = off_response(&mut *bundled(Mic, "REVERB"), 0.0, 1.0, [0.3, 0.2], 5000);
    assert!(y[1..].iter().all(|f| *f == [0.0; 2]) && y[0] == [0.3, 0.2]);
}

// ---- send-fx ----

/// What every send effect must do: no dry signal, a tail after OFF, silence at return level 0, finite output.
fn send_rules(name: &str) {
    let y = off_response(
        &mut *bundled(Send, name),
        1.0,
        0.5,
        [1.0, 1.0],
        3 * RATE as usize,
    );
    assert!(
        y[..100].iter().all(|f| *f == [0.0; 2]),
        "{name}: dry signal in the return"
    );
    assert!(
        rms(&y[100..]) > 3e-4,
        "{name}: no tail after OFF ({})",
        rms(&y[100..])
    );
    assert!(
        y.iter().all(|f| f[0].is_finite() && f[1].is_finite()),
        "{name}: not finite"
    );
    let y = off_response(
        &mut *bundled(Send, name),
        0.0,
        0.5,
        [1.0, 1.0],
        3 * RATE as usize,
    );
    assert!(
        y.iter().all(|f| *f == [0.0; 2]),
        "{name}: sound at return level 0"
    );
}

#[test]
fn every_send_effect_returns_only_wet_and_keeps_its_tail() {
    if !have_bundled() {
        return;
    }
    for n in [
        "SHORT DELAY",
        "REVERB",
        "SHIMMER",
        "TAPE ECHO",
        "PING PONG",
        "ECHO-VERB",
    ] {
        send_rules(n);
    }
}

#[test]
fn short_delay_time_follows_the_knob_from_40_to_150_ms() {
    if !have_bundled() {
        return;
    }
    for (knob, at) in [(0.0, 1920), (1.0, 7200)] {
        let y = off_response(
            &mut *bundled(Send, "SHORT DELAY"),
            1.0,
            knob,
            [1.0, 0.0],
            12_000,
        );
        assert!(y[at][0] > 0.9, "knob {knob}: {:?}", &y[at - 1..=at + 1]);
        assert!(y[..at - 1].iter().all(|f| *f == [0.0; 2]));
    }
}

#[test]
fn send_ping_pong_return_alternates_sides_each_period() {
    if !have_bundled() {
        return;
    }
    let y = off_response(
        &mut *bundled(Send, "PING PONG"),
        1.0,
        0.5,
        [1.0, 0.0],
        100_000,
    );
    // The impulse is on the left: mono 0.5 goes in on the right, then left, right (feedback 0.5).
    for (k, side) in [(1, 1), (2, 0), (3, 1)] {
        let f = y[24_000 * k];
        assert!(
            (f[side] - 0.5 * 0.5f32.powi(k as i32 - 1)).abs() < 1e-6 && f[1 - side] == 0.0,
            "repeat {k}: {f:?}"
        );
    }
}

#[test]
fn tape_echo_repeat_wobbles_around_one_beat_and_darkens() {
    if !have_bundled() {
        return;
    }
    let y = off_response(
        &mut *bundled(Send, "TAPE ECHO"),
        1.0,
        0.8,
        [1.0, 1.0],
        100_000,
    );
    let first = 20_000 + peak_at(&y[20_000..30_000]);
    // 1.5 ms wow + 0.1 ms flutter at most.
    assert!(first.abs_diff(24_000) <= 80, "first repeat at {first}");
    let second = 40_000 + peak_at(&y[40_000..56_000]);
    assert!(
        y[second][0].abs() < 0.6 * y[first][0].abs(),
        "the second repeat is darker and lower"
    );
}

#[test]
fn shimmer_puts_the_tail_an_octave_above_a_plain_reverb() {
    if !have_bundled() {
        return;
    }
    // A 300 Hz burst, then off; compare the share of 600 Hz in the tail.
    let tail = |name| {
        let mut s = bundled(Send, name);
        let burst = sine(300.0, 0.3, 14_400);
        let y: Vec<Frame> = (0..3 * RATE as usize)
            .map(|i| match burst.get(i) {
                Some(&x) => s.tick(x, &ctx(1.0, 1.0, 1.0)),
                None => s.tick([0.0; 2], &ctx(1.0, 0.0, 1.0)),
            })
            .collect();
        let t = &y[24_000..96_000];
        tone_power(t, 600.0) / tone_power(t, 300.0)
    };
    let (p, s) = (tail("REVERB"), tail("SHIMMER"));
    assert!(
        s > 1.3 * p,
        "600/300 Hz power in the tail: shimmer {s}, reverb {p}"
    );
}

#[test]
fn echo_verb_echoes_on_the_beat_and_the_reverb_follows_it() {
    if !have_bundled() {
        return;
    }
    let y = off_response(
        &mut *bundled(Send, "ECHO-VERB"),
        1.0,
        0.5,
        [1.0, 1.0],
        100_000,
    );
    assert!(y[24_000][0] > 0.7, "the echo, dry of reverb at that moment");
    // The echo's reverb is already ringing before the second repeat.
    assert!(rms(&y[26_000..30_000]) > 1e-3);
}

#[test]
fn the_send_loops_cannot_run_away() {
    if !have_bundled() {
        return;
    }
    let c = ctx(1.0, 1.0, 1.0);
    assert_stable(Send, "SHIMMER", &c, 3.0);
    assert_stable(Send, "ECHO-VERB", &c, 3.0);
}

// ---- lever-fx ----

/// The rest rule: after being worked, an effect at g 0 and knob 0 hands the input back unchanged.
fn rests(name: &str) {
    let x = sine(300.0, 0.5, 2 * RATE as usize);
    let mut s = bundled(Lever, name);
    s.engage();
    run(&mut *s, &ctx(1.0, 1.0, 0.8), &x);
    let y = run(&mut *s, &ctx(1.0, 0.0, 0.0), &x);
    assert!(y == x, "{name}: not bit-exact at rest");
    assert_eq!(run(&mut *bundled(Lever, name), &ctx(1.0, 0.0, 0.0), &x), x);
}

#[test]
fn every_lever_effect_is_bit_exact_at_rest() {
    if !have_bundled() {
        return;
    }
    for n in ["ECHO", "REVERB", "JUGGLE", "REVERSE", "SOLO", "STRETCH"] {
        rests(n);
    }
}

#[test]
fn lever_echo_and_reverb_add_a_tail_that_grows_with_the_lever() {
    if !have_bundled() {
        return;
    }
    let tail = |name, lever| {
        rms(&run(
            &mut *bundled(Lever, name),
            &ctx(1.0, 1.0, lever),
            &impulse(60_000, 0, [1.0, 1.0]),
        )[20_000..30_000])
    };
    assert!(tail("ECHO", 1.0) > 2.0 * tail("ECHO", 0.3) && tail("ECHO", 0.3) > 0.0);
    assert!(tail("REVERB", 1.0) > 2.0 * tail("REVERB", 0.3) && tail("REVERB", 0.3) > 0.0);
}

#[test]
fn solo_narrows_to_one_kilohertz() {
    if !have_bundled() {
        return;
    }
    let c = ctx(1.0, 1.0, 1.0);
    let mid = gain(&mut *bundled(Lever, "SOLO"), &c, 1000.0);
    assert!(mid > 1.0, "1 kHz: {mid}");
    for hz in [100.0, 8000.0] {
        let g = gain(&mut *bundled(Lever, "SOLO"), &c, hz);
        assert!(g < 0.05 * mid, "{hz} Hz: {g}");
    }
    assert!(
        gain(&mut *bundled(Lever, "SOLO"), &ctx(1.0, 1.0, 0.0), 100.0) > 0.05,
        "a wide band at the lever's start"
    );
}

/// A ramp, so every output frame says which input frame it came from (left channel = index / 1e5).
fn counter(n: usize) -> Vec<Frame> {
    (0..n).map(|i| [i as f32 * 1e-5, 0.0]).collect()
}

#[test]
fn juggle_replays_the_captured_beat_in_scrambled_slices() {
    if !have_bundled() {
        return;
    }
    let mut c = ctx(1.0, 1.0, 0.1);
    c.period = 1600.0;
    let x = counter(1600 + 1600 + 100);
    let y = run(&mut *bundled(Lever, "JUGGLE"), &c, &x);
    assert_eq!(y[..1600], x[..1600], "the capture is heard live");
    // Four slices of 400: the order is 0, 3, 2, 1.
    for (slice, from) in [(0, 0), (1, 3), (2, 2), (3, 1)] {
        let (at, src) = (1600 + 400 * slice + 200, 400 * from + 200);
        assert!(
            (y[at][0] - x[src][0]).abs() < 1e-5,
            "slice {slice}: {} vs {}",
            y[at][0],
            x[src][0]
        );
    }
}

#[test]
fn reverse_plays_the_previous_beat_backwards() {
    if !have_bundled() {
        return;
    }
    let mut c = ctx(1.0, 1.0, 0.5);
    c.period = 2000.0;
    let x = counter(4000);
    let y = run(&mut *bundled(Lever, "REVERSE"), &c, &x);
    assert_eq!(y[..2000], x[..2000], "the first beat is heard forwards");
    for i in [300, 1000, 1700] {
        assert!(
            (y[2000 + i][0] - x[1999 - i][0]).abs() < 1e-5,
            "frame {i}: {} vs {}",
            y[2000 + i][0],
            x[1999 - i][0]
        );
    }
    let third = run(&mut *bundled(Lever, "REVERSE"), &c, &counter(6000));
    assert!(
        (third[4000 + 500][0] - 3499.0 * 1e-5).abs() < 1e-5,
        "the second reversal plays beat two"
    );
}

#[test]
fn stretch_slows_the_audio_by_the_lever() {
    if !have_bundled() {
        return;
    }
    let y = run(
        &mut *bundled(Lever, "STRETCH"),
        &ctx(1.0, 1.0, 1.0),
        &sine(1000.0, 0.5, 40_000),
    );
    let hz = peak_hz(&y[10_000..], 150.0, 400.0);
    assert!(
        (hz - 250.0).abs() < 6.0,
        "{hz} Hz, expected a quarter of 1 kHz"
    );
}

// ---- cross-pass ----

#[test]
fn closing_the_low_pass_keeps_60_hz_and_kills_2_khz() {
    if !have_bundled() {
        return;
    }
    let c = ctx(1.0, 1.0, -1.0);
    let low = gain(&mut *bundled(Color, "CROSS-PASS"), &c, 60.0);
    // Fully closed, the sweep's own low-pass phase-shifts the highs against the lows: about +1 dB near 60 Hz.
    assert!((low - 1.0).abs() < 0.2, "60 Hz: {low}");
    assert!(
        gain(&mut *bundled(Color, "CROSS-PASS"), &c, 2000.0) < 0.05,
        "2 kHz"
    );
}

#[test]
fn opening_the_high_pass_keeps_the_lows_and_the_top_but_not_the_middle() {
    if !have_bundled() {
        return;
    }
    let c = ctx(1.0, 1.0, 1.0);
    let g = |hz| gain(&mut *bundled(Color, "CROSS-PASS"), &c, hz);
    assert!((g(60.0) - 1.0).abs() < 0.15, "60 Hz");
    assert!(g(1000.0) < 0.05, "1 kHz");
    assert!((g(15_000.0) - 1.0).abs() < 0.15, "15 kHz");
}

#[test]
fn the_cross_pass_centre_is_an_exact_passthrough() {
    if !have_bundled() {
        return;
    }
    let x = sine(2000.0, 0.5, 5000);
    assert_eq!(
        run(&mut *bundled(Color, "CROSS-PASS"), &ctx(1.0, 1.0, 0.0), &x),
        x
    );
}

// ---- soft-mix ----

#[test]
fn soft_full_fader_is_unity_and_zero_is_silent() {
    if !have_bundled() {
        return;
    }
    let x = sine(5000.0, 0.5, 5000);
    assert_eq!(
        run(&mut *bundled(Fader, "SOFT"), &ctx(1.0, 1.0, 1.0), &x),
        x
    );
    assert!(
        run(&mut *bundled(Fader, "SOFT"), &ctx(1.0, 1.0, 0.0), &x)
            .iter()
            .all(|f| *f == [0.0; 2])
    );
    let off = Ctx {
        g: 0.0,
        ..ctx(1.0, 1.0, 0.0)
    };
    assert_eq!(
        run(&mut *bundled(Fader, "SOFT"), &off, &x),
        x,
        "deselected: the host's curve takes over"
    );
}

#[test]
fn soft_mid_fader_cuts_the_highs_by_up_to_6_db_and_leaves_the_lows() {
    if !have_bundled() {
        return;
    }
    let c = ctx(1.0, 1.0, 0.5);
    let low = gain(&mut *bundled(Fader, "SOFT"), &c, 100.0) / 0.5;
    let high = gain(&mut *bundled(Fader, "SOFT"), &c, 10_000.0) / 0.5;
    assert!((low - 1.0).abs() < 0.05, "100 Hz: {low}");
    assert!((0.48..0.6).contains(&high), "10 kHz: {high}");
    let near_top = gain(&mut *bundled(Fader, "SOFT"), &ctx(1.0, 1.0, 0.9), 10_000.0) / 0.9;
    assert!(near_top > 0.85, "gone towards the top: {near_top}");
}

// ---- tutorial-fx ----

#[test]
fn the_example_beat_effects_change_the_signal_and_are_exact_while_off() {
    if !have_bundled() {
        return;
    }
    let x = sine(440.0, 0.5, 20_000);
    for name in ["JET SWEEP", "DEEP PHASER", "TRANCE GATE", "LO-FI"] {
        let on = run(&mut *bundled(Beat, name), &ctx(1.0, 1.0, 0.5), &x);
        assert!(
            on.iter().all(|f| f[0].is_finite() && f[1].is_finite()),
            "{name}"
        );
        assert!(
            on.iter().zip(&x).any(|(a, b)| (a[0] - b[0]).abs() > 0.05),
            "{name} changes a sine"
        );
        let off = run(&mut *bundled(Beat, name), &ctx(1.0, 0.0, 0.5), &x);
        assert_eq!(off, x, "{name}: off is the input, bit for bit");
    }
}

#[test]
fn the_trance_gate_chops_at_twice_the_beat_rate() {
    if !have_bundled() {
        return;
    }
    // 120 BPM, 1 beat: a cycle is 12 000 frames, open for the first 6000 (3 ms declick at the edges).
    let y = run(
        &mut *bundled(Beat, "TRANCE GATE"),
        &ctx(1.0, 1.0, 0.5),
        &vec![[0.5, 0.5]; 24_000],
    );
    assert!(
        y[5000][0] > 0.49 && y[9000][0].abs() < 0.01 && y[17_000][0] > 0.49,
        "{:?}",
        [y[5000], y[9000], y[17_000]]
    );
}

#[test]
fn the_dub_siren_returns_a_swept_echo_and_no_dry_signal() {
    if !have_bundled() {
        return;
    }
    let y = off_response(
        &mut *bundled(Send, "DUB SIREN"),
        1.0,
        0.5,
        [1.0, 1.0],
        3 * RATE as usize,
    );
    assert!(y[..100].iter().all(|f| *f == [0.0; 2]));
    assert!(rms(&y[12_000..60_000]) > 1e-3, "the echoes");
    assert!(y.iter().all(|f| f[0].is_finite()), "finite");
    assert!(
        rms(&y[2 * RATE as usize..]) < rms(&y[12_000..60_000]),
        "and they die away"
    );
}
