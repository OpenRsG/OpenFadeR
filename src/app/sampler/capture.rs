//! What a capture is made of and where it goes: the span of a deck's loop, the WAV file next to every capture (CONTROLLER > Sampler >
//! capture location, at the ADVANCED > Export Loop As Wav sampling frequency) and the slice's dominant frequency
//! band for the SYNC WITH FREQUENCY colour (VIEW > Color > Sampler). The colour mapping and the band weights are ours.

use super::prefs;
use openfader::{
    audio::{self, Pcm},
    paths,
};
use std::path::PathBuf;

/// The ENV override of the capture folder.
const DIR_ENV: &str = "OPENFADER_SAMPLER_DIR";

/// `(from_ms, secs)` of the loop `(start_ms, end_ms)`, at most `max_secs` long.
pub(super) fn loop_region((start, end): (u32, u32), max_secs: f64) -> (f64, f64) {
    (
        start as f64,
        (end.saturating_sub(start) as f64 / 1000.0).min(max_secs),
    )
}

/// Writes `pcm` as `<folder>/<prefix>-YYYYmmdd-HHMMSS.wav` (16-bit stereo at the loop-export frequency) and returns the path.
pub(super) fn save(pcm: &Pcm, prefix: &str) -> Result<PathBuf, String> {
    let (dir, hz) = prefs::with(|p| {
        (
            paths::capture_dir(&p.controller.capture_dir, DIR_ENV, "Sampler"),
            p.advanced.loop_hz(),
        )
    });
    let dir = dir.ok_or(tr!("set a capture location, {} or HOME", DIR_ENV))?;
    let path = paths::at(paths::timestamped(&dir, prefix, "wav"), &dir)?;
    paths::at(audio::write_wav(&path, pcm, hz), &path)?;
    Ok(path)
}

/// The slice's dominant band: 0 low, 1 mid, 2 high. Three bands from two one-pole filters (250 Hz, 3 kHz) on the mono mix; the energies are
/// weighted 1 : 4 : 12 because music carries far more energy in the bass, so a plain maximum would call every slice low.
pub(super) fn band(pcm: &Pcm) -> i32 {
    if pcm.rate == 0 {
        return 0;
    }
    let k = |fc: f32| 1.0 - (-std::f32::consts::TAU * fc / pcm.rate as f32).exp();
    let (k1, k2) = (k(250.0), k(3000.0));
    let (mut l1, mut l2, mut e) = (0f32, 0f32, [0f64; 3]);
    for f in pcm.samples.chunks_exact(2) {
        let m = (f[0] + f[1]) * 0.5;
        l1 += k1 * (m - l1);
        l2 += k2 * (m - l2);
        for (e, x) in e.iter_mut().zip([l1, l2 - l1, m - l2]) {
            *e += (x * x) as f64;
        }
    }
    let w = [1.0, 4.0, 12.0];
    (0..3)
        .max_by(|&a, &b| (e[a] * w[a]).total_cmp(&(e[b] * w[b])))
        .unwrap_or(0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32) -> Pcm {
        Pcm {
            rate: 44_100,
            samples: (0..44_100)
                .flat_map(|i| {
                    let s = (i as f32 * hz * std::f32::consts::TAU / 44_100.0).sin();
                    [s, s]
                })
                .collect(),
        }
    }

    #[test]
    fn a_loop_is_captured_from_its_start_for_its_length_and_never_longer_than_the_cap() {
        assert_eq!(loop_region((1000, 3500), 30.0), (1000.0, 2.5));
        assert_eq!(loop_region((0, 90_000), 30.0), (0.0, 30.0));
        assert_eq!(
            loop_region((5000, 4000), 30.0),
            (5000.0, 0.0),
            "a reversed range is empty, not a panic"
        );
    }

    #[test]
    fn the_frequency_colour_follows_the_loudest_weighted_band() {
        assert_eq!(
            [100.0, 1000.0, 10_000.0].map(|hz| band(&sine(hz))),
            [0, 1, 2]
        );
        assert_eq!(band(&Pcm::default()), 0);
    }
}
