//! Audio fingerprint: finds a track again after its file moved, was renamed or was re-tagged (`collection.fingerprint`).
//!
//! The decoded samples of a fixed window (30 s from 10 s in, the whole track when it is shorter than 40 s) are mixed to mono,
//! rounded to 16 bit and hashed with FNV-1a 64 together with the sample rate; the result is 16 hex digits. Decoding the same
//! file twice with one decoder always gives the same fingerprint. Anything that changes the audio changes it: another encode,
//! a trim or a different start, a gain change, a sample-rate change. Tag edits do not.
//! ponytail: two different decoders (or builds) may differ by float noise; the 16-bit rounding hides it except in the rare
//! sample that sits on a rounding edge, and then the hashes differ and the track is not found (never a wrong match). Use a
//! spectral fingerprint if that matters.
//!
//! [`Window`] takes the decoder's f32 frames one by one (keeping only the first 40 s as mono 16-bit values, the window is
//! decided at the end when the length is known), so the decoder fingerprints a track without a full-length buffer;
//! [`of`] feeds it a [`Pcm`].

use super::cache::fnv1a;
use crate::audio::Pcm;

/// Seconds skipped at the start (intros are often silence or shared) and hashed after that.
const SKIP_S: usize = 10;
const WINDOW_S: usize = 30;

/// Mono 16-bit values of the first `SKIP_S + WINDOW_S` seconds of a stream of stereo f32 frames at `rate` Hz, and how many
/// frames came by in all.
pub(crate) struct Window {
    rate: u32,
    keep: usize,
    mono: Vec<i16>,
    frames: usize,
}

impl Window {
    pub fn new(rate: u32) -> Self {
        Window {
            rate,
            keep: (SKIP_S + WINDOW_S) * rate as usize,
            mono: Vec::new(),
            frames: 0,
        }
    }

    pub fn push(&mut self, [l, r]: [f32; 2]) {
        if self.mono.len() < self.keep {
            let m = (l + r) * 0.5;
            self.mono
                .push((m.clamp(-1.0, 1.0) * 32767.0).round() as i16);
        }
        self.frames += 1;
    }

    /// The fingerprint of the stream; empty for no audio.
    pub fn finish(self) -> String {
        if self.rate == 0 || self.frames == 0 {
            return String::new();
        }
        let window = if self.frames > self.keep {
            &self.mono[SKIP_S * self.rate as usize..]
        } else {
            &self.mono[..]
        };
        let bytes = window.iter().flat_map(|v| v.to_le_bytes());
        format!(
            "{:016x}",
            fnv1a(self.rate.to_le_bytes().into_iter().chain(bytes))
        )
    }
}

/// The fingerprint of `pcm` (interleaved stereo); empty for no audio.
pub fn of(pcm: &Pcm) -> String {
    let mut w = Window::new(pcm.rate);
    pcm.samples
        .chunks_exact(2)
        .for_each(|f| w.push([f[0], f[1]]));
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of stereo tones: `hz` on the left, a fifth above on the right.
    fn tone(hz: f32, secs: usize) -> Pcm {
        let rate = 8000;
        Pcm {
            rate,
            samples: (0..rate as usize * secs)
                .flat_map(|i| {
                    let t = i as f32 / rate as f32;
                    [
                        (t * hz * 6.2831855).sin() * 0.5,
                        (t * hz * 1.5 * 6.2831855).sin() * 0.5,
                    ]
                })
                .collect(),
        }
    }

    #[test]
    fn same_audio_same_fingerprint_other_audio_another() {
        let (a, b) = (tone(440.0, 45), tone(441.0, 45));
        assert_eq!(of(&a), of(&tone(440.0, 45))); // a second decode
        assert_ne!(of(&a), of(&b));
        assert_eq!(of(&a).len(), 16);
        // Only the window counts: a change before it does not matter, one inside does.
        let mut head = a.clone();
        head.samples[10] += 0.3;
        assert_eq!(of(&a), of(&head));
        let mut inside = a.clone();
        inside.samples[2 * 8000 * 20] += 0.3;
        assert_ne!(of(&a), of(&inside));
        assert_ne!(of(&tone(440.0, 5)), of(&tone(440.0, 6))); // short tracks hash whole
        assert_eq!(of(&Pcm::default()), "");
    }
}
