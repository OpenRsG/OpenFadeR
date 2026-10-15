use super::*;
use crate::audio::{dsp, fx::FALLBACK_BPM};

const FS: u32 = 48_000;
/// Frames per `begin`, like an audio callback.
const BUF: usize = 256;

fn rig(kind: Kind, knob: f32) -> (Cfx, Controls) {
    let c = Controls::default();
    c.set_kind(kind);
    c.set_knob(knob);
    (Cfx::new(FS), c)
}

/// Left output for `n` frames of the input `f(i)` on both channels.
fn run(cfx: &mut Cfx, c: &Controls, n: usize, f: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(n);
    for start in (0..n).step_by(BUF) {
        cfx.begin(c);
        for i in start..(start + BUF).min(n) {
            out.push(cfx.process(f(i), f(i)).0);
        }
    }
    out
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn sine(hz: f64) -> impl Fn(usize) -> f32 {
    move |i| (0.5 * (std::f64::consts::TAU * hz * i as f64 / FS as f64).sin()) as f32
}

/// Steady-state gain in dB for a sine at `hz`.
fn gain_db(kind: Kind, knob: f32, hz: f64) -> f64 {
    let (mut cfx, c) = rig(kind, knob);
    let out = run(&mut cfx, &c, FS as usize, sine(hz));
    let x: Vec<f32> = (0..FS as usize).map(sine(hz)).collect();
    20.0 * (rms(&out[FS as usize / 2..]) / rms(&x[FS as usize / 2..])).log10()
}

/// Deterministic wideband test signal.
fn busy(i: usize) -> f32 {
    ((i * 7919 % 2001) as f32 / 1000.0 - 1.0) * 0.8
}

#[test]
fn centre_is_a_bit_exact_bypass_for_every_kind() {
    for kind in Kind::ALL {
        for knob in [0.0, 0.02, -0.02] {
            let (mut cfx, c) = rig(kind, knob);
            for chunk in 0..40 {
                let to_filter = cfx.begin(&c);
                assert_eq!(to_filter, if kind == Kind::Filter { knob } else { 0.0 });
                for i in chunk * BUF..(chunk + 1) * BUF {
                    assert_eq!(
                        cfx.process(busy(i), -busy(i)),
                        (busy(i), -busy(i)),
                        "{kind:?} at {knob}"
                    );
                }
            }
        }
    }
}

#[test]
fn filter_kind_is_the_channels_plain_filter() {
    for knob in [-1.0, -0.6, 0.02, 0.4, 1.0] {
        let (mut cfx, c) = rig(Kind::Filter, knob);
        let (mut plain, mut via) = (dsp::Fx::new(), dsp::Fx::new());
        for chunk in 0..40 {
            plain.set(FS as f64, [0.0; 3], knob, BUF);
            via.set(FS as f64, [0.0; 3], cfx.begin(&c), BUF);
            for i in chunk * BUF..(chunk + 1) * BUF {
                let want = plain.process(busy(i), -busy(i));
                let (l, r) = via.process(busy(i), -busy(i));
                assert_eq!(cfx.process(l, r), want, "knob {knob}");
            }
        }
    }
}

#[test]
fn dub_echo_repeats_at_the_beat_period() {
    let (mut cfx, c) = rig(Kind::DubEcho, 1.0);
    c.set_bpm(120.0);
    let (hit, beat) = (12_000, FS as usize / 2); // 120 BPM = 0.5 s
    let out = run(&mut cfx, &c, hit + 3 * beat, |i| {
        if i == hit { 1.0 } else { 0.0 }
    });
    assert_eq!(out[hit], 1.0);
    assert!(
        out[hit + 1..hit + beat].iter().all(|v| v.abs() < 1e-6),
        "silent between hit and first repeat"
    );
    assert!(
        (out[hit + beat] - 1.0).abs() < 1e-3,
        "first repeat is `amount` loud: {}",
        out[hit + beat]
    );
    let second = out[hit + beat + 1..]
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap();
    assert_eq!(
        second.0 + hit + beat + 1,
        hit + 2 * beat,
        "second repeat one beat later"
    );
    assert!(
        (second.1 - 0.65 * 0.85).abs() < 0.01,
        "after the feedback low-pass: {}",
        second.1
    );
    // A faster deck: 150 BPM = 0.4 s
    let (mut cfx, c) = rig(Kind::DubEcho, 1.0);
    c.set_bpm(150.0);
    let out = run(&mut cfx, &c, hit + FS as usize, |i| {
        if i == hit { 1.0 } else { 0.0 }
    });
    assert!((out[hit + 19_200] - 1.0).abs() < 1e-3);
}

#[test]
fn crush_quantises_to_the_expected_levels() {
    let (mut cfx, c) = rig(Kind::Crush, 1.0); // 4 bits: steps of 1/8, held 24 frames
    let out = run(&mut cfx, &c, 30_000, |i| 0.9 * sine(220.0)(i) * 2.0);
    let out = &out[12_000..]; // the knob smoothing has settled
    assert!(
        out.iter().all(|v| (v * 8.0).fract() == 0.0),
        "multiples of 1/8"
    );
    let mut levels: Vec<i32> = out.iter().map(|v| (v * 8.0) as i32).collect();
    levels.sort_unstable();
    levels.dedup();
    assert!(levels.len() > 8 && levels.len() <= 17, "{levels:?}");
    let changes = out.windows(2).filter(|w| w[0] != w[1]).count();
    assert!(
        changes <= out.len() / 24 + 1,
        "held for 24 frames: {changes} changes"
    );
}

#[test]
fn noise_level_follows_the_knob() {
    for side in [-1.0f32, 1.0] {
        let mut last = 0.0;
        for x in [0.25f32, 0.5, 0.75, 1.0] {
            let (mut cfx, c) = rig(Kind::Noise, side * x);
            let out = run(&mut cfx, &c, FS as usize * 2, |_| 0.0);
            let level = rms(&out[FS as usize..]);
            let want = 0.2 * ((x - DEAD) / (1.0 - DEAD)) as f64;
            assert!(
                (level / want - 1.0).abs() < 0.15,
                "side {side} knob {x}: rms {level:.4}, want {want:.4}"
            );
            assert!(level > last);
            last = level;
        }
    }
}

#[test]
fn noise_sweeps_the_filter_with_the_knob() {
    let band = |knob: f32| {
        let (mut cfx, c) = rig(Kind::Noise, knob);
        let out = run(&mut cfx, &c, FS as usize * 2, |_| 0.0);
        // share of the energy in the first difference (high frequencies) against the level
        let d: Vec<f32> = out[FS as usize..].windows(2).map(|w| w[1] - w[0]).collect();
        rms(&d) / rms(&out[FS as usize..])
    };
    assert!(band(-1.0) < band(-0.3), "left darkens as it turns");
    assert!(band(1.0) > band(0.3), "right brightens as it turns");
}

#[test]
fn space_send_rises_with_the_knob() {
    let wet = |knob: f32| {
        let (mut cfx, c) = rig(Kind::Space, knob);
        let out = run(&mut cfx, &c, FS as usize * 2, busy);
        let w: Vec<f32> = out
            .iter()
            .enumerate()
            .skip(FS as usize)
            .map(|(i, y)| y - busy(i))
            .collect();
        rms(&w)
    };
    let (a, b, c) = (wet(0.2), wet(0.5), wet(1.0));
    assert!(a > 1e-3 && a < b && b < c, "{a} {b} {c}");
}

#[test]
fn sweep_is_a_band_that_moves_with_the_knob() {
    assert!(gain_db(Kind::Sweep, 1.0, 100.0) < -20.0);
    assert!(gain_db(Kind::Sweep, 1.0, 8000.0) > -3.0);
    assert!(gain_db(Kind::Sweep, -1.0, 5000.0) < -20.0);
    assert!(gain_db(Kind::Sweep, -1.0, 58.0) > -3.0);
}

#[test]
fn tails_decay_to_silence_then_the_channel_is_bit_exact_again() {
    for kind in [Kind::Space, Kind::DubEcho] {
        let (mut cfx, c) = rig(kind, 0.8);
        c.set_bpm(120.0);
        run(&mut cfx, &c, FS as usize * 2, sine(330.0));
        c.set_knob(0.0);
        let secs = 20;
        let out = run(&mut cfx, &c, FS as usize * secs, |_| 0.0);
        let sec = |s: usize| rms(&out[s * FS as usize..(s + 1) * FS as usize]);
        assert!(
            sec(0) > 1e-3,
            "{kind:?} rings after the knob is back: {}",
            sec(0)
        );
        assert!(sec(4) < sec(0), "{kind:?} decays");
        assert!(
            out[(secs - 1) * FS as usize..].iter().all(|&v| v == 0.0),
            "{kind:?} silent and bypassed at the end"
        );
        cfx.begin(&c);
        assert_eq!(cfx.process(0.3, -0.2), (0.3, -0.2));
    }
}

#[test]
fn switching_kind_drops_the_old_tail() {
    let (mut cfx, c) = rig(Kind::DubEcho, 1.0);
    run(&mut cfx, &c, FS as usize, sine(330.0));
    c.set_knob(0.0);
    c.set_kind(Kind::Crush);
    let out = run(&mut cfx, &c, BUF * 4, |_| 0.0);
    assert!(out.iter().all(|&v| v == 0.0));
}

#[test]
fn controls_clamp_and_default_bad_values() {
    let c = Controls::default();
    c.set_knob(f32::NAN);
    assert_eq!(c.knob(), 0.0);
    c.set_knob(7.0);
    assert_eq!(c.knob(), 1.0);
    c.set_bpm(f32::NAN);
    assert_eq!(c.bpm.get(), FALLBACK_BPM);
    c.set_bpm(1.0);
    assert_eq!(c.bpm.get(), 20.0);
    assert_eq!(Kind::from_index(99), Kind::Crush);
}

/// A COLOR plugin effect that outputs `[knob, g]` while engaged (else its input), so the context it was given can be read back.
struct Probe(std::sync::Arc<AtomicU32>);

impl fx::Stage for Probe {
    fn tick(&mut self, x: Frame, c: &fx::Ctx) -> Frame {
        if c.g == 0.0 { x } else { [c.knob, c.g] }
    }
    fn reset(&mut self) {}
    fn engage(&mut self) {
        self.0.fetch_add(1, Relaxed);
    }
}

#[test]
fn a_color_plugin_follows_the_built_ins_and_gets_the_signed_knob() {
    let engaged = std::sync::Arc::new(AtomicU32::new(0));
    let mut cfx = Cfx::with(FS, vec![Box::new(Probe(engaged.clone()))]);
    let c = Controls::default();
    c.set_index(Kind::ALL.len()); // the first plugin effect
    let last = |cfx: &mut Cfx, buffers: usize| {
        let mut y = (0.0, 0.0);
        for _ in 0..buffers {
            cfx.begin(&c);
            for _ in 0..BUF {
                y = cfx.process(0.25, 0.25);
            }
        }
        y
    };
    for (knob, want) in [(-1.0f32, -1.0f32), (0.5, (0.5 - DEAD) / (1.0 - DEAD))] {
        c.set_knob(knob);
        let (k, g) = last(&mut cfx, 40);
        assert!((k - want).abs() < 1e-4 && g == 1.0, "knob {knob}: {k}, {g}");
    }
    assert!(engaged.load(Relaxed) >= 1);
    // Back at the centre the effect rings out its hold, then the channel is bit-exact again.
    c.set_knob(0.0);
    assert_eq!(last(&mut cfx, 200), (0.25, 0.25));
    // Another entry of the selector drops the plugin: FILTER hands the knob to the channel filter.
    c.set_index(0);
    c.set_knob(0.7);
    assert_eq!(cfx.begin(&c), 0.7);
    assert_eq!(cfx.process(0.25, 0.25), (0.25, 0.25));
}
