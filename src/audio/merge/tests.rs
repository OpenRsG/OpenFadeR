use super::*;

const FS: u32 = 48_000;
const BUF: usize = 256;

type Buf = Vec<(Frame, Frame)>;

/// `n` frames of input `f(i)` (both channels), `BUF` per callback, from frame `from`; the master path out.
fn run(u: &mut Unit, c: &Controls, from: usize, n: usize, f: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(n);
    for start in (from..from + n).step_by(BUF) {
        let mut b: Buf = (start..(start + BUF).min(from + n)).map(|i| ([f(i); 2], [0.5; 2])).collect();
        u.apply(c, &mut b);
        assert!(b.iter().all(|f| f.1 == [0.5; 2]), "the cue path is never touched");
        out.extend(b.iter().map(|f| f.0[0]));
    }
    out
}

fn rig(preset: usize, knob: f32, volume: f32) -> (Unit, Controls) {
    let c = Controls::default();
    c.set_preset(preset);
    c.set_knob(knob);
    c.set_volume(volume);
    c.set_on(true);
    (Unit::new(FS), c)
}

fn sine(hz: f64, amp: f32) -> impl Fn(usize) -> f32 {
    move |i| amp * (std::f64::consts::TAU * hz * i as f64 / FS as f64).sin() as f32
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

#[test]
fn presets_are_distinct_and_every_part_rises_with_the_knob() {
    for (i, a) in PRESETS.iter().enumerate() {
        assert!(PRESETS[i + 1..].iter().all(|b| [a.hpf, a.noise, a.echo] != [b.hpf, b.noise, b.echo]), "{}", a.name);
        assert_eq!(a.amounts(0.0), [0.0; 3], "{}: silent at k = 0", a.name);
        let mut last = [0.0; 3];
        for k in (0..=20).map(|k| k as f32 / 20.0) {
            let now = a.amounts(k);
            assert!(now.iter().zip(last).all(|(n, l)| *n >= l && *n <= 1.0), "{} at {k}", a.name);
            last = now;
        }
        assert_eq!(a.amounts(1.0), [a.hpf, a.noise, a.echo]);
        assert_eq!(a.amounts(f32::NAN), [0.0; 3]);
        assert_eq!(a.amounts(7.0), a.amounts(1.0));
    }
}

#[test]
fn controls_clamp_and_abort_clears_on() {
    let c = Controls::default();
    c.set_preset(99);
    assert_eq!(c.preset().name, PRESETS[3].name);
    c.set_knob(f32::NAN);
    assert_eq!(c.knob(), 0.0);
    c.set_knob(2.0);
    assert_eq!(c.knob(), 1.0);
    c.set_volume(f32::INFINITY);
    assert_eq!(c.volume(), DEFAULT_VOLUME);
    c.set_on(true);
    c.abort();
    assert!(!c.on.load(Relaxed));
}

#[test]
fn off_is_a_bit_exact_untouched_pass_through() {
    let (mut u, c) = rig(3, 1.0, 1.0);
    c.set_on(false);
    assert!(u.idle());
    let f = |i: usize| (i as f32 * 0.37).sin();
    assert_eq!(run(&mut u, &c, 0, 4 * BUF, f), (0..4 * BUF).map(f).collect::<Vec<_>>());
    let c = Controls::default(); // knob at 0 but ON held: nothing is audible either
    c.set_on(true);
    let mut u = Unit::new(FS);
    let out = run(&mut u, &c, 0, 8 * BUF, f);
    assert!(out.iter().zip((0..).map(f)).all(|(y, x)| (y - x).abs() < 1e-6));
}

#[test]
fn noise_rises_with_the_knob_and_follows_the_sample_volume() {
    let level = |preset, knob, vol| {
        let (mut u, c) = rig(preset, knob, vol);
        rms(&run(&mut u, &c, 0, FS as usize, |_| 0.0)[FS as usize / 2..])
    };
    let r: Vec<f64> = [0.25, 0.5, 0.75, 1.0].iter().map(|&k| level(0, k, 0.5)).collect();
    assert!(r.windows(2).all(|w| w[1] > w[0] * 1.2) && r[0] > 0.005, "{r:?}");
    let (half, full) = (level(0, 1.0, 0.5), level(0, 1.0, 1.0));
    assert!((full / half - 2.0).abs() < 0.1, "volume 1 is twice volume 0.5: {half} {full}");
    assert!(level(0, 1.0, 0.0) < 1e-6, "volume 0 is silent");
    assert!(level(1, 1.0, 1.0) < 1e-6, "the ECHO preset has no noise");
}

#[test]
fn high_pass_rises_with_the_knob() {
    let gain = |knob| {
        let (mut u, c) = rig(0, knob, 0.0); // RISE without its noise
        let out = run(&mut u, &c, 0, FS as usize, sine(100.0, 0.5));
        rms(&out[FS as usize / 2..]) / rms(&(0..FS as usize / 2).map(sine(100.0, 0.5)).collect::<Vec<_>>())
    };
    let g: Vec<f64> = [0.1, 0.5, 0.8, 1.0].iter().map(|&k| gain(k)).collect();
    assert!(g[0] > 0.9 && g[3] < 0.05 && g.windows(2).all(|w| w[1] < w[0]), "{g:?}");
}

#[test]
fn echo_feed_grows_with_the_knob_at_the_beat_period() {
    let repeat = |knob| {
        let (mut u, c) = rig(1, knob, 0.0); // ECHO preset
        c.set_bpm(120.0); // 0.5 s
        let (hit, beat, burst) = (12_000, FS as usize / 2, 960);
        let out = run(&mut u, &c, 0, hit + beat + 2 * burst, |i| if (hit..hit + burst).contains(&i) { sine(3000.0, 0.5)(i) } else { 0.0 });
        assert!(rms(&out[hit + beat - 200..hit + beat]) < 1e-3, "silent before the first repeat");
        rms(&out[hit + beat..hit + beat + burst])
    };
    let r: Vec<f64> = [0.25, 0.5, 1.0].iter().map(|&k| repeat(k)).collect();
    assert!(r[0] > 0.005 && r[1] > r[0] * 1.3 && r[2] > r[1] * 1.3, "{r:?}");
}

#[test]
fn abort_drops_everything_at_once_release_lets_the_echo_ring_out() {
    let tone = sine(3000.0, 0.5);
    let tail = |end: &dyn Fn(&Controls)| {
        let (mut u, c) = rig(3, 1.0, 1.0);
        c.set_bpm(120.0);
        run(&mut u, &c, 0, FS as usize / 2, &tone);
        end(&c);
        let (from, quiet) = (FS as usize / 2, |_: usize| 0.0);
        let out = run(&mut u, &c, from, 4 * BUF, quiet); // the fade is 240 frames: past it
        let late = run(&mut u, &c, from + 4 * BUF, FS as usize / 4, quiet);
        (u, out, late)
    };
    let (u, _, late) = tail(&|c| c.abort());
    assert!(late.iter().all(|&y| y == 0.0), "abort: bit-exact silence (the input) after the fade");
    assert!(u.idle(), "abort: the unit is idle");

    let (u, _, late) = tail(&|c| c.set_on(false));
    assert!(rms(&late) > 0.01 && !u.idle(), "release: the echo repeats still ring: {}", rms(&late));
}

#[test]
fn abort_fades_in_a_few_ms_and_on_starts_afresh() {
    let (mut u, c) = rig(3, 1.0, 1.0);
    let f = sine(200.0, 0.5);
    run(&mut u, &c, 0, FS as usize / 2, &f);
    c.abort();
    let out = run(&mut u, &c, FS as usize / 2, 6 * BUF, &f);
    let want: Vec<f32> = (FS as usize / 2..FS as usize / 2 + 6 * BUF).map(&f).collect();
    assert_eq!(out[2 * BUF..], want[2 * BUF..], "dry after the 5 ms fade");
    let err: Vec<f32> = out.iter().zip(&want).map(|(y, x)| y - x).collect();
    let steps: Vec<f64> = err[..4 * 60].chunks(60).map(rms).collect();
    assert!(steps.windows(2).all(|w| w[1] < w[0]) && steps[0] > 0.05, "the wet share ramps down over the 5 ms: {steps:?}");

    c.set_on(true);
    let out = run(&mut u, &c, 0, FS as usize / 2, &f);
    let dry: Vec<f32> = (0..FS as usize / 2).map(&f).collect();
    let err: Vec<f32> = out.iter().zip(&dry).map(|(y, x)| y - x).collect();
    assert!(rms(&err[FS as usize / 4..]) > 0.1, "the next ON builds up again");
    assert!(out.iter().all(|y| y.is_finite()));
}
