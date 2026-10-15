//! Integrated loudness estimate of a decoded track for AUTO GAIN (Preferences > Controller > Mixer).
//!
//! BS.1770-style gating on plain RMS: mean squares over non-overlapping 400 ms blocks, blocks under
//! [`ABSOLUTE_GATE_DB`] dropped, then blocks more than [`RELATIVE_GATE_DB`] below the mean of the rest dropped,
//! so silence and quiet intros do not drag a track's loudness down. The gain brings that loudness to
//! [`TARGET_LUFS`], clamped to +-[`MAX_GAIN_DB`]. ponytail: no K-weighting filter, so this is LUFS-ish (within a
//! couple of dB on music); add the two biquads if exact LUFS ever matters.

use super::Pcm;

/// Loudness every track is steered to (streaming-service level).
const TARGET_LUFS: f32 = -14.0;
/// Largest AUTO GAIN correction in either direction, dB.
pub const MAX_GAIN_DB: f32 = 12.0;
const BLOCK_S: f32 = 0.4;
const ABSOLUTE_GATE_DB: f32 = -70.0;
const RELATIVE_GATE_DB: f32 = -10.0;

/// Mean-square power as loudness: dB with the BS.1770 -0.691 offset (power floored at 1e-12).
fn db(power: f64) -> f32 {
    10.0 * (power.max(1e-12) as f32).log10() - 0.691
}

/// Gain in dB that brings the track to [`TARGET_LUFS`]; 0 for an empty or silent track.
pub fn auto_gain_db(pcm: &Pcm) -> f32 {
    let block = ((pcm.rate as f32 * BLOCK_S) as usize).max(1) * 2;
    // Summed mean square of both channels per block (a short track is one block).
    let power = |b: &[f32]| {
        b.iter().map(|&s| s as f64 * s as f64).sum::<f64>() / (b.len() / 2).max(1) as f64
    };
    let mut blocks: Vec<f64> = pcm.samples.chunks_exact(block).map(power).collect();
    if blocks.is_empty() && !pcm.samples.is_empty() {
        blocks.push(power(&pcm.samples));
    }
    blocks.retain(|&p| db(p) > ABSOLUTE_GATE_DB);
    if blocks.is_empty() {
        return 0.0;
    }
    let mean = |b: &[f64]| b.iter().sum::<f64>() / b.len() as f64;
    let floor = db(mean(&blocks)) + RELATIVE_GATE_DB;
    blocks.retain(|&p| db(p) > floor);
    (TARGET_LUFS - db(mean(&blocks))).clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
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

    fn pcm(samples: Vec<f32>) -> Pcm {
        Pcm {
            rate: RATE,
            samples,
        }
    }

    #[test]
    fn loud_tracks_are_turned_down_and_quiet_ones_up() {
        let loud = auto_gain_db(&pcm(tone(3.0, 0.5)));
        let quiet = auto_gain_db(&pcm(tone(3.0, 0.02)));
        assert!(loud < -1.0 && loud > -MAX_GAIN_DB, "{loud}");
        assert_eq!(quiet, MAX_GAIN_DB); // far below target: clamped
    }

    #[test]
    fn a_track_at_the_target_needs_no_gain() {
        // a stereo sine of amplitude a measures 20 log10(a) - 0.691 (both channels summed): a = 0.2 is about -14.7
        let g = auto_gain_db(&pcm(tone(3.0, 0.2)));
        assert!(g.abs() < 1.5, "{g}");
        // doubling the amplitude lowers the gain by 6 dB (inside the clamp)
        let d = auto_gain_db(&pcm(tone(3.0, 0.4))) - g;
        assert!((d + 6.02).abs() < 0.05, "{d}");
    }

    #[test]
    fn silence_and_nothing_give_no_gain() {
        assert_eq!(auto_gain_db(&pcm(vec![])), 0.0);
        assert_eq!(auto_gain_db(&pcm(vec![0.0; 2 * RATE as usize])), 0.0);
    }

    #[test]
    fn silent_stretches_do_not_change_the_loudness() {
        let body = tone(4.0, 0.3); // whole 400 ms blocks
        let plain = auto_gain_db(&pcm(body.clone()));
        let mut padded = vec![0.0; 2 * RATE as usize * 2];
        padded.extend(&body);
        padded.extend(vec![0.0; 2 * RATE as usize * 2]);
        assert!((auto_gain_db(&pcm(padded)) - plain).abs() < 0.1);
    }

    #[test]
    fn a_quiet_intro_is_gated_out() {
        let mut samples = tone(2.0, 0.01); // 30 dB under the body
        samples.extend(tone(4.0, 0.3));
        let gated = auto_gain_db(&pcm(samples));
        let body = auto_gain_db(&pcm(tone(4.0, 0.3)));
        assert!((gated - body).abs() < 0.1, "{gated} vs {body}");
    }
}
