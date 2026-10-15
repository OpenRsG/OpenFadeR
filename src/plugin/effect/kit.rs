//! Test helpers for effects: contexts, runs, measurements, and the bundled plugins loaded from `extension/plugin/`.

use super::{Effect, Slot, load};
use crate::audio::fx::{Ctx, Frame, Stage};
use std::f32::consts::TAU;
use std::path::PathBuf;
use std::sync::LazyLock;

pub const RATE: u32 = 48_000;

/// A context at 120 BPM, one beat: `period` 24 000 samples.
pub fn ctx(level: f32, g: f32, knob: f32) -> Ctx {
    Ctx {
        rate: RATE as f32,
        period: 24_000.0,
        beats: 1.0,
        level,
        g,
        knob,
    }
}

/// The `extension/plugin/` checkout of the plugins repository (start.sh fetches it); tests of its effects skip without it.
pub fn have_bundled() -> bool {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("extension/plugin")
        .is_dir()
}

/// Every effect of the bundled plugins (`extension/plugin/`), loaded once; the folder must load without errors.
pub fn bundled_all() -> &'static [Effect] {
    static ALL: LazyLock<Vec<Effect>> = LazyLock::new(|| {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension/plugin");
        let mut r = crate::plugin::discover(&[dir]);
        let fx = load(&r.plugins, &mut r.errors);
        assert!(r.errors.is_empty(), "{:#?}", r.errors);
        fx
    });
    &ALL
}

/// A new instance of the bundled effect `name` of `slot`.
pub fn bundled(slot: Slot, name: &str) -> Box<dyn Stage> {
    bundled_all()
        .iter()
        .find(|e| e.slot == slot && e.name == name)
        .unwrap_or_else(|| panic!("no bundled {slot:?} effect {name}"))
        .stage(RATE)
}

/// `s` run over `input` under `c`.
pub fn run(s: &mut dyn Stage, c: &Ctx, input: &[Frame]) -> Vec<Frame> {
    input.iter().map(|&x| s.tick(x, c)).collect()
}

/// An effect switched on for one frame (`g` 1 on frame 0, `x` the input there) then off (`g` 0, silent input): what is
/// left of it.
pub fn off_response(s: &mut dyn Stage, level: f32, knob: f32, x: Frame, n: usize) -> Vec<Frame> {
    (0..n)
        .map(|i| match i {
            0 => s.tick(x, &ctx(level, 1.0, knob)),
            _ => s.tick([0.0; 2], &ctx(level, 0.0, knob)),
        })
        .collect()
}

/// `n` frames of a sine at `hz`, amplitude `amp`, both channels.
pub fn sine(hz: f32, amp: f32, n: usize) -> Vec<Frame> {
    (0..n)
        .map(|i| {
            let v = amp * (TAU * hz * i as f32 / RATE as f32).sin();
            [v, v]
        })
        .collect()
}

/// `n` frames of silence with `x` at frame `at`.
pub fn impulse(n: usize, at: usize, x: Frame) -> Vec<Frame> {
    let mut v = vec![[0.0; 2]; n];
    v[at] = x;
    v
}

/// RMS of the left channel.
pub fn rms(x: &[Frame]) -> f32 {
    (x.iter().map(|f| f[0] * f[0]).sum::<f32>() / x.len() as f32).sqrt()
}

/// Index of the largest |left| frame of `x`.
pub fn peak_at(x: &[Frame]) -> usize {
    (0..x.len())
        .max_by(|&a, &b| x[a][0].abs().total_cmp(&x[b][0].abs()))
        .unwrap()
}

/// Gain of `s` for a steady sine at `hz`: RMS of the second half of the output over the input's.
pub fn gain(s: &mut dyn Stage, c: &Ctx, hz: f32) -> f32 {
    let x = sine(hz, 0.1, RATE as usize);
    let y = run(s, c, &x);
    let h = x.len() / 2;
    rms(&y[h..]) / rms(&x[h..])
}

/// Energy of the left channel of `x` (Hann windowed) at `hz`.
pub fn tone_power(x: &[Frame], hz: f32) -> f32 {
    let n = x.len() as f32;
    let (mut re, mut im) = (0.0f32, 0.0f32);
    for (i, f) in x.iter().enumerate() {
        let v = f[0] * (0.5 - 0.5 * (TAU * i as f32 / n).cos());
        let a = TAU * hz * i as f32 / RATE as f32;
        re += v * a.cos();
        im += v * a.sin();
    }
    re * re + im * im
}

/// The frequency in `lo..=hi` where [`tone_power`] of `x` is largest, found in 2 Hz steps.
pub fn peak_hz(x: &[Frame], lo: f32, hi: f32) -> f32 {
    let steps = ((hi - lo) / 2.0) as usize;
    (0..=steps)
        .map(|k| lo + 2.0 * k as f32)
        .max_by(|&a, &b| tone_power(x, a).total_cmp(&tone_power(x, b)))
        .unwrap()
}

/// 10 s of white noise at ±0.3 through a new instance under `c`: the output must stay finite, the last second must not
/// be louder than 1.2 times the second second (no run-away loop), and the peak must stay below `peak`.
pub fn assert_stable(slot: Slot, name: &str, c: &Ctx, peak: f32) {
    let mut s = 0x2545_f491u32;
    let x: Vec<Frame> = (0..10 * RATE as usize)
        .map(|_| {
            let mut v = [0.0; 2];
            for o in &mut v {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                *o = (s as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.3;
            }
            v
        })
        .collect();
    let y = run(&mut *bundled(slot, name), c, &x);
    assert!(
        y.iter().all(|f| f[0].is_finite() && f[1].is_finite()),
        "{name}: not finite"
    );
    let sec = RATE as usize;
    let (early, late) = (rms(&y[sec..2 * sec]), rms(&y[9 * sec..]));
    assert!(late <= 1.2 * early, "{name}: grows from {early} to {late}");
    let top = y
        .iter()
        .map(|f| f[0].abs().max(f[1].abs()))
        .fold(0.0, f32::max);
    assert!(top < peak, "{name}: peak {top}");
}
