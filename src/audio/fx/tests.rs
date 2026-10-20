//! Behaviour tests of the Beat FX unit at 48 kHz and 120 BPM: a beat is 24000 samples, 1/4 beat 6000.

use super::*;
use std::f32::consts::TAU;

const RATE: u32 = 48_000;
const QUARTER: usize = 6000;

fn on(effect: Effect, beats: f32, level: f32) -> Settings {
    Settings {
        effect: effect.index(),
        beats,
        level,
        knob: 0.5,
        on: true,
        bpm: 120.0,
    }
}

/// `u.set(s)`, then `n` frames of `input(i)`; an idle unit passes frames through like [`Unit::apply`].
fn run(u: &mut Unit, s: Settings, n: usize, input: impl Fn(usize) -> Frame) -> Vec<Frame> {
    u.prepare(s.effect);
    u.set(s);
    (0..n)
        .map(|i| {
            if u.idle() {
                input(i)
            } else {
                u.process(input(i))
            }
        })
        .collect()
}

fn impulse(at: usize) -> impl Fn(usize) -> Frame {
    move |i| if i == at { [1.0, -1.0] } else { [0.0; 2] }
}

fn sine(hz: f32) -> impl Fn(usize) -> Frame {
    move |i| [(i as f32 * hz * TAU / RATE as f32).sin(); 2]
}

/// A unit switched ON a moment ago with silence, so the engage ramp is done and the tempo settled.
fn warm(effect: Effect, beats: f32, level: f32) -> (Unit, Settings) {
    let mut u = Unit::new(RATE);
    let s = on(effect, beats, level);
    run(&mut u, s, 1000, |_| [0.0; 2]);
    (u, s)
}

fn peak(out: &[Frame], from: usize, len: usize) -> f32 {
    out[from..from + len]
        .iter()
        .map(|y| y[0].abs())
        .fold(0.0, f32::max)
}

#[test]
fn echo_and_delay_repeat_at_the_beat_period_with_decay() {
    for (e, fb) in [(Effect::Delay, 0.4f32), (Effect::Echo, 0.55)] {
        let (mut u, s) = warm(e, 0.25, 1.0);
        let out = run(&mut u, s, QUARTER * 6, impulse(0));
        assert_eq!(out[0], [1.0, -1.0], "{e:?}: dry passes");
        assert!(
            (out[QUARTER][0] - 1.0).abs() < 1e-4,
            "{e:?}: first repeat is the input, one period later"
        );
        let mut last = f32::MAX;
        for n in 1..=5 {
            let p = peak(&out, QUARTER * n, 50);
            assert!(p < last && p > 1e-3, "{e:?} repeat {n} decays: {p}");
            if e == Effect::Delay {
                assert!(
                    (p - fb.powi(n as i32 - 1)).abs() < 1e-4,
                    "delay repeat {n} = fb^{}: {p}",
                    n - 1
                );
            }
            assert!(
                peak(&out, QUARTER * n + QUARTER / 2 - 100, 200) < 1e-6,
                "{e:?}: silent between repeats"
            );
            last = p;
        }
    }
}

#[test]
fn echo_period_follows_bpm_and_beat_fraction() {
    let mut u = Unit::new(RATE);
    // 1/2 beat at 150 BPM = 0.2 s = 9600 samples
    let s = Settings {
        bpm: 150.0,
        ..on(Effect::Delay, 0.5, 1.0)
    };
    run(&mut u, s, 1000, |_| [0.0; 2]);
    let out = run(&mut u, s, 9700, impulse(0));
    assert!((out[9600][0] - 1.0).abs() < 1e-4);
}

#[test]
fn off_is_bit_exact_for_every_effect() {
    let sig = |i: usize| {
        [
            ((i * 7919) % 2001) as f32 / 1000.0 - 1.0,
            ((i * 104729) % 1999) as f32 / 999.0 - 1.0,
        ]
    };
    for e in Effect::ALL {
        let mut u = Unit::new(RATE);
        assert!(u.idle(), "{e:?}");
        u.prepare(e.index());
        u.set(Settings {
            on: false,
            ..on(e, 1.0, 1.0)
        });
        // Even forced through the effect (idle skips it), a unit that never engaged changes nothing.
        assert!((0..3000).all(|i| u.process(sig(i)) == sig(i)), "{e:?}");
        assert!(u.idle(), "{e:?}");
    }
}

#[test]
fn trails_decay_to_silence_then_the_unit_is_idle_and_exact() {
    for e in [Effect::Echo, Effect::Delay, Effect::Reverb] {
        let (mut u, s) = warm(e, 0.25, 1.0);
        run(
            &mut u,
            s,
            2000,
            |i| if i < 500 { [0.3, 0.2] } else { [0.0; 2] },
        );
        let off = Settings { on: false, ..s };
        // the tail is still ringing half a second to a second after OFF…
        let after = run(&mut u, off, RATE as usize, |_| [0.0; 2]);
        assert!(
            after[RATE as usize / 2..].iter().any(|y| y[0].abs() > 1e-4),
            "{e:?} has no trail"
        );
        assert!(!u.idle(), "{e:?}");
        // …and gone within a minute, after which the signal passes untouched.
        let tail = run(&mut u, off, RATE as usize * 60, |_| [0.0; 2]);
        assert!(
            tail[tail.len() - 1000..].iter().all(|y| *y == [0.0; 2]),
            "{e:?}"
        );
        assert!(u.idle(), "{e:?}");
        let sig = |i: usize| [i as f32 * 1e-4, 0.25];
        assert!(
            run(&mut u, off, 100, sig)
                .iter()
                .enumerate()
                .all(|(i, y)| *y == sig(i)),
            "{e:?}"
        );
    }
}

#[test]
fn effects_without_trails_stop_within_the_ramp() {
    for e in [
        Effect::Flanger,
        Effect::Phaser,
        Effect::Roll,
        Effect::Lpf,
        Effect::Hpf,
        Effect::Trans,
    ] {
        let (mut u, s) = warm(e, 0.25, 1.0);
        run(&mut u, s, 5000, sine(440.0));
        let out = run(&mut u, Settings { on: false, ..s }, 600, |i| {
            sine(440.0)(i + 5000)
        });
        assert!(
            out[300..]
                .iter()
                .enumerate()
                .all(|(i, y)| *y == sine(440.0)(5300 + i)),
            "{e:?}"
        );
    }
}

/// `e` at 1/4 beat on a steady sine, 6 periods.
fn modulated(e: Effect, hz: f32) -> Vec<Frame> {
    let (mut u, s) = warm(e, 0.25, 1.0);
    run(&mut u, s, QUARTER * 6, sine(hz))
}

#[test]
fn flanger_and_phaser_lfo_period_is_the_beat_fraction() {
    // 112 Hz and 1008 Hz fit whole times into half a period (3000 samples), so only the LFO can make the output differ.
    for (e, hz) in [(Effect::Flanger, 112.0), (Effect::Phaser, 1008.0)] {
        let out = modulated(e, hz);
        let diff = |a: usize, b: usize, len: usize| {
            (0..len)
                .map(|i| (out[a + i][0] - out[b + i][0]).abs())
                .fold(0.0, f32::max)
        };
        assert!(
            diff(QUARTER, 2 * QUARTER, QUARTER) < 2e-3,
            "{e:?} repeats every period: {}",
            diff(QUARTER, 2 * QUARTER, QUARTER)
        );
        assert!(
            diff(QUARTER, QUARTER + QUARTER / 2, QUARTER / 2) > 0.05,
            "{e:?} differs half a period later"
        );
    }
}

#[test]
fn lfo_completes_one_cycle_per_period() {
    let mut l = Lfo::default();
    let phases: Vec<f32> = (0..24_000 * 16).map(|_| l.tick(24_000.0)).collect();
    assert_eq!(
        phases.windows(2).filter(|w| w[1] < w[0]).count(),
        15,
        "wraps between 16 cycles"
    );
    assert!(phases[24_000 * 16 - 1] > 0.99);
}

#[test]
fn roll_repeats_the_slice_captured_at_on() {
    let mut u = Unit::new(RATE);
    let input = |i: usize| [i as f32 * 1e-4, -(i as f32) * 1e-4];
    let out = run(&mut u, on(Effect::Roll, 0.25, 1.0), QUARTER * 4, input);
    for i in 0..QUARTER {
        assert_eq!(out[i], input(i), "recording passes through");
        for r in 1..4 {
            assert!(
                (out[r * QUARTER + i][0] - input(i)[0]).abs() < 1e-6,
                "repeat {r} at {i}"
            );
        }
    }
}

#[test]
fn trans_gate_opens_once_per_period() {
    let (mut u, s) = warm(Effect::Trans, 0.25, 1.0);
    let out = run(&mut u, s, QUARTER * 8, |_| [1.0; 2]);
    let opens: Vec<usize> = (1..out.len())
        .filter(|&i| out[i - 1][0] < 0.5 && out[i][0] >= 0.5)
        .collect();
    assert_eq!(opens.len(), 8, "{opens:?}");
    assert!(
        opens.windows(2).all(|w| w[1] - w[0] == QUARTER),
        "{opens:?}"
    );
    assert!(out.iter().any(|y| y[0] < 0.01), "it closes too");
}

#[test]
fn lpf_and_hpf_sweep_the_spectrum_over_the_period() {
    let rms = |o: &[Frame]| (o.iter().map(|y| y[0] * y[0]).sum::<f32>() / o.len() as f32).sqrt();
    let mid = QUARTER / 2;
    // 8 kHz sine: the LPF is open at the period start, shut at mid-period.
    let out = run(
        &mut Unit::new(RATE),
        on(Effect::Lpf, 0.25, 1.0),
        QUARTER,
        sine(8000.0),
    );
    let (start, centre) = (rms(&out[300..600]), rms(&out[mid - 150..mid + 150]));
    assert!(start > 0.5 && centre < 0.1, "LPF {start} {centre}");
    // 100 Hz sine: the HPF lets it through at the start, blocks it mid-period.
    let out = run(
        &mut Unit::new(RATE),
        on(Effect::Hpf, 0.25, 1.0),
        QUARTER,
        sine(100.0),
    );
    let (start, centre) = (rms(&out[300..780]), rms(&out[mid - 240..mid + 240]));
    assert!(start > 0.3 && centre < 0.05, "HPF {start} {centre}");
}

#[test]
fn tempo_change_glides_without_a_jump() {
    let (mut u, s) = warm(Effect::Delay, 0.25, 1.0);
    run(&mut u, s, QUARTER * 2, sine(150.0));
    let out = run(&mut u, Settings { bpm: 140.0, ..s }, 5000, |i| {
        sine(150.0)(i + QUARTER * 2)
    });
    assert!(
        out.windows(2).all(|w| (w[1][0] - w[0][0]).abs() < 0.5),
        "no step in the output"
    );
}

#[test]
fn apply_edits_only_the_assigned_master_path() {
    let c = Controls::default();
    c.set_effect(Effect::Trans);
    c.set_level(1.0);
    c.set_beat(2); // 1/4
    c.set_on(true);
    // The gate is open for the first 3000 samples and shut for the next 3000.
    for (assign, hit) in [(Assign::A, 0usize), (Assign::B, 1)] {
        c.set_assign(assign);
        let mut u = Unit::new(RATE);
        u.prepare(c.settings().effect);
        let mut buf = vec![vec![([1.0; 2], [0.7; 2]); QUARTER]; 2];
        u.apply(&c, &mut buf, QUARTER);
        assert!(
            buf[hit].iter().any(|f| f.0[0] < 0.05),
            "{assign:?} is gated"
        );
        assert!(
            buf[1 - hit].iter().all(|f| f.0 == [1.0; 2]),
            "{assign:?}: the other channel is untouched"
        );
        assert!(
            buf.iter().flatten().all(|f| f.1 == [0.7; 2]),
            "{assign:?}: the cue path is untouched"
        );
    }
    // Master: the sum of both channels goes through the gate.
    c.set_assign(Assign::Master);
    let mut u = Unit::new(RATE);
    u.prepare(c.settings().effect);
    let mut buf = vec![vec![([0.5; 2], [0.7; 2]); QUARTER]; 2];
    u.apply(&c, &mut buf, QUARTER);
    let sum = |i: usize| buf[0][i].0[0] + buf[1][i].0[0];
    assert!((sum(1000) - 1.0).abs() < 1e-4, "open: {}", sum(1000));
    assert!(
        sum(QUARTER * 3 / 4) < 0.05,
        "shut: {}",
        sum(QUARTER * 3 / 4)
    );
}

#[test]
fn a_unit_aimed_at_a_stem_part_is_skipped_by_apply_and_left_idle() {
    let c = Controls::default();
    c.set_effect(Effect::Trans);
    c.set_level(1.0);
    c.set_beat(2); // 1/4
    c.set_on(true);
    c.set_assign(Assign::A);
    let mut u = Unit::new(RATE);
    u.prepare(c.settings().effect);
    let mut buf = vec![vec![([1.0; 2], [0.7; 2]); QUARTER]; 2];
    u.apply(&c, &mut buf, QUARTER); // whole channel: gated, the unit is busy
    assert!(!u.idle());
    c.set_part(1 << 3);
    let mut buf = vec![vec![([1.0; 2], [0.7; 2]); QUARTER]; 2];
    u.apply(&c, &mut buf, QUARTER);
    assert!(
        buf.iter().flatten().all(|f| f.0 == [1.0; 2]),
        "a part unit does not touch the channel"
    );
    assert!(u.idle(), "and its whole-channel state is dropped");
    assert_eq!(c.stem_target(0).map(|t| t.0), Some(8));
    assert!(c.stem_target(1).is_none(), "only its own channel");
    c.set_part(0x1F);
    assert_eq!(
        c.stem_target(0).map(|t| t.0),
        Some(0xF),
        "mask is limited to the four stems"
    );
    c.set_part(0);
    assert!(c.stem_target(0).is_none(), "no part = the whole channel");
    c.set_part(8);
    c.set_assign(Assign::Master);
    assert!(c.stem_target(0).is_none(), "MASTER ignores the part");
    let mut buf = vec![vec![([0.5; 2], [0.7; 2]); QUARTER]; 2];
    u.apply(&c, &mut buf, QUARTER);
    assert!(
        buf[0].iter().any(|f| f.0[0] < 0.4),
        "master with a part still gates the sum"
    );
}

#[test]
fn idle_unit_leaves_the_buffers_alone() {
    let c = Controls::default();
    let mut u = Unit::new(RATE);
    let make = || {
        vec![
            vec![([0.25; 2], [0.1; 2]); 64],
            vec![([-0.5; 2], [0.2; 2]); 64],
        ]
    };
    let mut buf = make();
    u.apply(&c, &mut buf, 64);
    assert_eq!(buf, make());
}

#[test]
fn bad_knob_values_are_safe() {
    let c = Controls::default();
    c.set_bpm(f32::NAN);
    c.set_level(f32::INFINITY);
    c.set_beat(99);
    let s = c.settings();
    assert_eq!((s.bpm, s.level, s.beats), (FALLBACK_BPM, 0.0, 32.0));
    assert_eq!(fold(1e9, 100.0), 1e9 / 2f32.powi(24));
    assert_eq!(fold(f32::NAN, 100.0), 1.0);
}
