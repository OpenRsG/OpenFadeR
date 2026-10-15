//! A drum loop's waveform for its slot tile: one column per pixel, the peak of the loop's frames in that column, in the DRUMS stem's blue.

use openfader::audio::groove::Drum;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Peak (0..1) of each of `w` equal parts of the `beats` beats of `d` that play.
pub fn peaks(d: &Drum, w: usize) -> Vec<f32> {
    let frames = ((d.beats as f64 * d.frames_per_beat()) as usize).min(d.pcm.samples.len() / 2);
    (0..w)
        .map(|x| {
            let (a, b) = (
                x * frames / w,
                ((x + 1) * frames / w).max(x * frames / w + 1).min(frames),
            );
            d.pcm.samples[2 * a..2 * b]
                .iter()
                .fold(0f32, |m, s| m.max(s.abs()))
                .min(1.0)
        })
        .collect()
}

/// `w`×`h` pixels, bars from the middle line.
pub fn image(d: &Drum, w: u32, h: u32) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let px = buf.make_mut_slice();
    for (x, p) in peaks(d, w as usize).into_iter().enumerate() {
        let half = ((p * h as f32 / 2.0).ceil() as u32).clamp(1, h / 2);
        for y in h / 2 - half.min(h / 2)..(h / 2 + half).min(h) {
            px[y as usize * w as usize + x] = Rgba8Pixel {
                r: 0x46,
                g: 0x70,
                b: 0xff,
                a: 0xff,
            };
        }
    }
    Image::from_rgba8(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::audio::Pcm;

    #[test]
    fn peaks_cover_the_playing_beats_only() {
        // 2 beats at 120 BPM at 1 kHz = 1000 frames of the 2000 given: loud first half, quiet second half, then silence past the loop
        let mut samples = vec![0.0f32; 2 * 2000];
        for (i, s) in samples.iter_mut().enumerate().take(2 * 1000) {
            *s = if i < 2 * 500 { 0.9 } else { 0.2 };
        }
        let d = Drum::new(
            Pcm {
                rate: 1000,
                samples,
            },
            120.0,
        )
        .unwrap();
        assert_eq!(d.beats, 4);
        let p = peaks(&d, 4);
        assert_eq!(p, vec![0.9, 0.2, 0.0, 0.0]);
    }
}
