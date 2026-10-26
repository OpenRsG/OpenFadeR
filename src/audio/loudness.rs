//! Integrated loudness estimate of a decoded track for AUTO GAIN (Preferences > Controller > Mixer).
//!
//! ITU-R BS.1770 loudness: K-weighted mean squares over 400 ms blocks with 75 % overlap, blocks under
//! [`ABSOLUTE_GATE_DB`] dropped, then blocks more than [`RELATIVE_GATE_DB`] below the mean of the rest dropped,
//! so silence and quiet intros do not drag a track's loudness down. The gain brings that loudness to
//! [`TARGET_LUFS`], clamped to +-[`MAX_GAIN_DB`]; a boost stops where the loudest sample would reach full scale (minus
//! [`PEAK_MARGIN_DB`]), so a quiet but peaky track is not clipped.

use super::Frames;

/// Loudness every track is steered to (streaming-service level).
const TARGET_LUFS: f32 = -14.0;
/// Largest AUTO GAIN correction in either direction, dB.
pub const MAX_GAIN_DB: f32 = 12.0;
const BLOCK_S: f32 = 0.4;
const ABSOLUTE_GATE_DB: f32 = -70.0;
const RELATIVE_GATE_DB: f32 = -10.0;
/// Headroom a boost leaves under full scale, dB.
const PEAK_MARGIN_DB: f32 = 0.5;

/// Mean-square power as loudness: dB with the BS.1770 -0.691 offset (power floored at 1e-12).
fn db(power: f64) -> f32 {
    10.0 * (power.max(1e-12) as f32).log10() - 0.691
}

/// A biquad in transposed direct form II (`a0` = 1).
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The BS.1770 K-weighting for `rate` Hz: the pre-filter (high shelf, +4 dB above ~1.7 kHz) then the RLB high-pass. The
/// standard lists 48 kHz coefficients; these are the analog prototypes' parameters (as in the standard's derivation)
/// through the bilinear transform at the actual rate.
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let (f0, gain, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let vh = 10f64.powf(gain / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let rlb = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, rlb]
}

/// Gain in dB that brings `audio` (at `rate` Hz) to [`TARGET_LUFS`]; 0 for an empty or silent track.
pub fn auto_gain_db<S: Frames + ?Sized>(audio: &S, rate: u32) -> f32 {
    let n = audio.frame_count();
    // K-weighted squares of both channels summed per 100 ms step; a 400 ms block is four steps, a new one every step
    // (75 % overlap). A track shorter than a block is one block.
    let step = ((rate as f32 * BLOCK_S / 4.0) as usize).max(1);
    let mut filters = [k_weighting(rate as f64), k_weighting(rate as f64)];
    let mut steps: Vec<f64> = Vec::with_capacity(n / step + 1);
    let mut sum = 0.0;
    for (i, frame) in audio.frames_in(0..n).enumerate() {
        for (ch, f) in filters.iter_mut().enumerate() {
            let x = f[0].run(frame[ch] as f64);
            let y = f[1].run(x);
            sum += y * y;
        }
        if (i + 1) % step == 0 {
            steps.push(std::mem::take(&mut sum));
        }
    }
    let mut blocks: Vec<f64> = steps
        .windows(4)
        .map(|w| w.iter().sum::<f64>() / (4 * step) as f64)
        .collect();
    if blocks.is_empty() && n > 0 {
        blocks.push((steps.iter().sum::<f64>() + sum) / n as f64);
    }
    blocks.retain(|&p| db(p) > ABSOLUTE_GATE_DB);
    if blocks.is_empty() {
        return 0.0;
    }
    let mean = |b: &[f64]| b.iter().sum::<f64>() / b.len() as f64;
    let floor = db(mean(&blocks)) + RELATIVE_GATE_DB;
    blocks.retain(|&p| db(p) > floor);
    let gain = (TARGET_LUFS - db(mean(&blocks))).clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
    if gain <= 0.0 {
        return gain;
    }
    // a boost never takes the loudest sample (non-zero here: a block was above the gate) past full scale
    let peak = audio
        .frames_in(0..n)
        .flatten()
        .fold(0.0f32, |m, s| m.max(s.abs()));
    gain.min((-20.0 * peak.log10() - PEAK_MARGIN_DB).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 8000;

    fn tone(seconds: f32, amp: f32) -> Vec<f32> {
        (0..(RATE as f32 * seconds) as usize)
            .flat_map(|i| {
                let s = amp * (i as f32 * 0.3).sin();
                [s, s]
            })
            .collect()
    }

    fn gain(samples: Vec<f32>) -> f32 {
        auto_gain_db(&samples, RATE)
    }

    /// `seconds` of a sine in the left channel only (the standard's test signals are mono-in-one-channel).
    fn sine(rate: u32, hz: f32, dbfs: f32, seconds: f32) -> Vec<f32> {
        let amp = 10f32.powf(dbfs / 20.0);
        (0..(rate as f32 * seconds) as usize)
            .flat_map(|i| {
                let t = i as f64 / rate as f64;
                [amp * (2.0 * std::f64::consts::PI * hz as f64 * t).sin() as f32, 0.0]
            })
            .collect()
    }

    /// The loudness `auto_gain_db` steered to: the target minus its (unclamped) gain.
    fn lufs(samples: Vec<f32>, rate: u32) -> f32 {
        TARGET_LUFS - auto_gain_db(&samples, rate)
    }

    #[test]
    fn standard_test_signals_read_their_lufs() {
        // full-scale 1 kHz: -3.01 LUFS (the gain, +-12 dB, still reaches -3 from -14)
        let full = lufs(sine(48_000, 1000.0, 0.0, 5.0), 48_000);
        assert!((full - -3.01).abs() < 0.1, "{full}");
        let ref997 = lufs(sine(48_000, 997.0, -20.0, 5.0), 48_000);
        assert!((ref997 - -23.0).abs() < 0.2, "{ref997}");
    }

    #[test]
    fn loudness_does_not_depend_on_the_sample_rate() {
        let a = lufs(sine(48_000, 1000.0, -20.0, 5.0), 48_000);
        let b = lufs(sine(44_100, 1000.0, -20.0, 5.0), 44_100);
        assert!((a - b).abs() < 0.1, "{a} vs {b}");
        // the shelf lifts 4 kHz by about 3.3 dB over 1 kHz (not the full +4 dB, which is the shelf's top), at any rate
        let hi44 = lufs(sine(44_100, 4000.0, -20.0, 5.0), 44_100);
        let hi48 = lufs(sine(48_000, 4000.0, -20.0, 5.0), 48_000);
        assert!((hi44 - b - 3.3).abs() < 0.3, "{hi44} vs {b}");
        assert!((hi44 - hi48).abs() < 0.1, "{hi44} vs {hi48}");
    }

    #[test]
    fn loud_tracks_are_turned_down_and_quiet_ones_up() {
        let loud = gain(tone(3.0, 0.5));
        let quiet = gain(tone(3.0, 0.02));
        assert!(loud < -1.0 && loud > -MAX_GAIN_DB, "{loud}");
        assert_eq!(quiet, MAX_GAIN_DB); // far below target: clamped
    }

    #[test]
    fn a_track_at_the_target_needs_no_gain() {
        // a stereo sine of amplitude a measures 20 log10(a) - 0.691 (both channels summed): a = 0.2 is about -14.7
        let g = gain(tone(3.0, 0.2));
        assert!(g.abs() < 1.5, "{g}");
        // doubling the amplitude lowers the gain by 6 dB (inside the clamp)
        let d = gain(tone(3.0, 0.4)) - g;
        assert!((d + 6.02).abs() < 0.05, "{d}");
    }

    #[test]
    fn silence_and_nothing_give_no_gain() {
        assert_eq!(gain(vec![]), 0.0);
        assert_eq!(gain(vec![0.0; 2 * RATE as usize]), 0.0);
    }

    #[test]
    fn silent_stretches_do_not_change_the_loudness() {
        // The 75 % overlapped blocks that straddle a body edge hold 25, 50 and 75 % of it and pass the relative gate
        // (as in the standard), pulling the mean down by 3 / (blocks of the body + 3) in power: a long body makes that
        // small.
        let body = tone(20.0, 0.3);
        let plain = gain(body.clone());
        let mut padded = vec![0.0; 2 * RATE as usize * 2];
        padded.extend(&body);
        padded.extend(vec![0.0; 2 * RATE as usize * 2]);
        assert!((gain(padded) - plain).abs() < 0.15);
    }

    #[test]
    fn a_quiet_intro_is_gated_out() {
        let mut samples = tone(2.0, 0.01); // 30 dB under the body
        samples.extend(tone(20.0, 0.3)); // long, so the blocks straddling the edge weigh little (see above)
        let gated = gain(samples);
        let body = gain(tone(20.0, 0.3));
        assert!((gated - body).abs() < 0.1, "{gated} vs {body}");
    }

    /// A quiet but peaky track is boosted only as far as its loudest sample allows (it used to go past full scale).
    #[test]
    fn a_boost_stops_short_of_full_scale() {
        // 3 s of a 0.05 tone with a 3-sample click of 0.8 every 100 ms: peak 0.8, about -22 LUFS, so +12 dB is asked
        let mut s = tone(3.0, 0.05);
        for k in 0..30 {
            for j in 0..3 {
                s[2 * (800 * k + j)] = 0.8;
                s[2 * (800 * k + j) + 1] = 0.8;
            }
        }
        let out = 0.8 * 10f32.powf(gain(s) / 20.0);
        assert!(out <= 1.0 && out > 0.9, "the peak ends at {out}");
    }
}
