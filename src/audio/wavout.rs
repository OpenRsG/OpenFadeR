//! A [`Pcm`] slice as a canonical 16-bit stereo PCM WAV (Microsoft RIFF WAVE, 44-byte header) at a chosen sampling
//! rate: the sampler's capture files and Export Loop As Wav. The slice is resampled linearly ([`Pcm::frame_at`]) when
//! its rate differs from `hz`; samples are clamped to ±1 and scaled by 32767, like the recorder ([`super::rec`]),
//! which shares [`header`] and [`to_i16`] (and so does [`crate::edit`]'s arrangement render).

use super::Pcm;
use std::{
    io::{self, Write},
    path::Path,
};

/// Bytes of the header before the samples.
pub(crate) const HEADER: usize = 44;
/// Bytes of one 16-bit stereo frame.
pub(crate) const FRAME_BYTES: usize = 4;

/// The header of a 16-bit stereo file at `hz` whose sample data is `data` bytes (saturated to the u32 size fields).
pub(crate) fn header(hz: u32, data: u64) -> Vec<u8> {
    let data = data.min((u32::MAX as usize - HEADER + 8) as u64) as u32;
    let fields: [&[u8]; 12] = [
        b"RIFF",
        &(data + HEADER as u32 - 8).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
        &1u16.to_le_bytes(), // PCM
        &2u16.to_le_bytes(), // channels
        &hz.to_le_bytes(),
        &(hz * FRAME_BYTES as u32).to_le_bytes(), // byte rate
        &(FRAME_BYTES as u16).to_le_bytes(),      // block align
        &16u16.to_le_bytes(),                     // bits per sample
        b"data",
        &data.to_le_bytes(),
    ];
    fields.concat()
}

/// One sample as 16-bit PCM: clamped to ±1, scaled by 32767 (hard clip).
pub(crate) fn to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Little-endian u32 at byte `i`.
#[cfg(test)]
pub(super) fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

/// The whole file for `pcm` at `hz` (`frames * hz / rate` frames; an empty or rate-less slice gives an empty data chunk).
pub fn wav_bytes(pcm: &Pcm, hz: u32) -> Vec<u8> {
    let hz = hz.max(1);
    let frames = if pcm.rate == 0 {
        0
    } else {
        (pcm.samples.len() as u64 / 2 * hz as u64 / pcm.rate as u64) as usize
    };
    let mut b = header(hz, (frames * FRAME_BYTES) as u64);
    b.reserve(frames * FRAME_BYTES);
    let step = pcm.rate as f64 / hz as f64;
    for i in 0..frames {
        for x in pcm.frame_at(i as f64 * step).unwrap_or([0.0; 2]) {
            b.extend(to_i16(x).to_le_bytes());
        }
    }
    b
}

/// Writes `pcm` at `hz` to `path`, which must not exist yet.
pub fn write_wav(path: &Path, pcm: &Pcm, hz: u32) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&wav_bytes(pcm, hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(rate: u32, frames: usize) -> Pcm {
        Pcm {
            rate,
            samples: (0..frames)
                .flat_map(|i| [i as f32 / frames as f32, -0.5])
                .collect(),
        }
    }

    #[test]
    fn header_describes_16_bit_stereo_at_the_requested_rate() {
        let b = wav_bytes(&ramp(48_000, 480), 48_000);
        assert_eq!(
            (&b[0..4], &b[8..16], &b[36..40]),
            (&b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..])
        );
        assert_eq!(
            (
                u32_at(&b, 16),
                b[20],
                b[22],
                u32_at(&b, 24),
                u32_at(&b, 28),
                b[32],
                b[34]
            ),
            (16, 1, 2, 48_000, 192_000, 4, 16)
        );
        assert_eq!(
            (b.len(), u32_at(&b, 4) as usize, u32_at(&b, 40) as usize),
            (HEADER + 480 * 4, b.len() - 8, 480 * 4)
        );
    }

    #[test]
    fn resampling_scales_the_length_and_keeps_the_shape() {
        let b = wav_bytes(&ramp(48_000, 4800), 44_100);
        assert_eq!(u32_at(&b, 40) as usize / 4, 4410, "0.1 s stays 0.1 s");
        let l = |i: usize| i16::from_le_bytes([b[HEADER + 4 * i], b[HEADER + 4 * i + 1]]);
        assert!(
            l(0) == 0 && l(4409) > 29_000 && (1..4410).all(|i| l(i) >= l(i - 1)),
            "the ramp still rises"
        );
        assert_eq!(
            i16::from_le_bytes([b[HEADER + 2], b[HEADER + 3]]),
            -16_384,
            "-0.5 on the right channel"
        );
    }

    #[test]
    fn an_empty_slice_is_a_valid_empty_file_and_samples_clip() {
        assert_eq!(wav_bytes(&Pcm::default(), 44_100).len(), HEADER);
        let loud = Pcm {
            rate: 44_100,
            samples: vec![3.0, -3.0],
        };
        let b = wav_bytes(&loud, 44_100);
        assert_eq!(
            (
                i16::from_le_bytes([b[44], b[45]]),
                i16::from_le_bytes([b[46], b[47]])
            ),
            (32767, -32767)
        );
    }

    #[test]
    fn write_refuses_to_overwrite() {
        let p = std::env::temp_dir().join(format!("openfader-wavout-{}.wav", std::process::id()));
        write_wav(&p, &ramp(44_100, 10), 44_100).unwrap();
        assert_eq!(std::fs::read(&p).unwrap().len(), HEADER + 40);
        assert!(write_wav(&p, &ramp(44_100, 10), 44_100).is_err());
        std::fs::remove_file(&p).unwrap();
    }
}
