//! The master picture: each deck's frame weighted by the crossfader (video follows the audio crossfader)
//! and by its channel fader (the picture's brightness follows it), then summed.

pub const DECKS: usize = crate::audio::CHANNELS;

/// How the crossfader changes the picture. There are two basic transitions here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Transition {
    /// Dissolve: each deck's share follows its crossfader gain.
    #[default]
    Fade,
    /// The deck with the larger crossfader gain shows alone (switches at the middle).
    Cut,
}

/// Weight (0..=1, summing to at most 1) of each deck's picture. `sides` is each deck's crossfader gain (what the audio
/// mixer uses; 0 for decks not in use), `levels` its channel fader (1 = full brightness).
pub fn weights(sides: [f32; DECKS], levels: [f32; DECKS], t: Transition) -> [f32; DECKS] {
    let sides = sides.map(|s| if s.is_finite() { s.max(0.0) } else { 0.0 });
    let mut w = [0.0; DECKS];
    match t {
        Transition::Fade => {
            // A constant-power crossfader gives sides summing above 1; the picture dissolves, it never brightens.
            let sum = sides.iter().sum::<f32>().max(1.0);
            for i in 0..DECKS {
                w[i] = sides[i] / sum;
            }
        }
        Transition::Cut => {
            let top = sides.iter().copied().fold(0.0, f32::max);
            if let Some(i) = sides.iter().position(|&s| s > 0.0 && s >= top) {
                w[i] = 1.0;
            }
        }
    }
    for i in 0..DECKS {
        w[i] *= levels[i].clamp(0.0, 1.0);
    }
    w
}

/// `out` = the sum of the layers (RGB24 bytes of the same length) scaled by their weights, in 8.8 fixed point.
pub fn blend(out: &mut [u8], layers: &[(&[u8], f32)]) {
    out.fill(0);
    for &(src, w) in layers {
        let w = (w.clamp(0.0, 1.0) * 256.0).round() as u32;
        if w == 0 {
            continue;
        }
        for (o, &s) in out.iter_mut().zip(src) {
            *o = o.saturating_add(((u32::from(s) * w + 128) >> 8) as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; DECKS], b: [f32; DECKS]) {
        assert!(
            a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5),
            "{a:?} != {b:?}"
        );
    }

    const FULL: [f32; DECKS] = [1.0; DECKS];

    #[test]
    fn fade_dissolves_with_the_crossfader() {
        // smooth curve: gains [1-x, x] sum to 1
        close(
            weights([0.75, 0.25, 0.0, 0.0], FULL, Transition::Fade),
            [0.75, 0.25, 0.0, 0.0],
        );
        close(
            weights([1.0, 0.0, 0.0, 0.0], FULL, Transition::Fade),
            [1.0, 0.0, 0.0, 0.0],
        );
        // constant power: -3 dB each in the middle is still a 50/50 dissolve
        let h = std::f32::consts::FRAC_1_SQRT_2;
        close(
            weights([h, h, 0.0, 0.0], FULL, Transition::Fade),
            [0.5, 0.5, 0.0, 0.0],
        );
        // a THRU channel (gain 1) beside a side at 1 shares half each
        close(
            weights([1.0, 1.0, 0.0, 0.0], FULL, Transition::Fade),
            [0.5, 0.5, 0.0, 0.0],
        );
        // never above 1 in total, nothing from nothing
        close(weights([0.0; DECKS], FULL, Transition::Fade), [0.0; DECKS]);
        close(
            weights([f32::NAN, 0.5, 0.0, 0.0], FULL, Transition::Fade),
            [0.0, 0.5, 0.0, 0.0],
        );
    }

    #[test]
    fn cut_switches_at_the_middle() {
        let cut = |a: f32| weights([1.0 - a, a, 0.0, 0.0], FULL, Transition::Cut);
        close(cut(0.0), [1.0, 0.0, 0.0, 0.0]);
        close(cut(0.49), [1.0, 0.0, 0.0, 0.0]);
        close(cut(0.51), [0.0, 1.0, 0.0, 0.0]);
        close(cut(1.0), [0.0, 1.0, 0.0, 0.0]);
        // exactly in the middle the first deck keeps it; with nothing open nothing shows
        close(cut(0.5), [1.0, 0.0, 0.0, 0.0]);
        close(weights([0.0; DECKS], FULL, Transition::Cut), [0.0; DECKS]);
    }

    #[test]
    fn channel_fader_dims_its_deck() {
        close(
            weights([0.5, 0.5, 0.0, 0.0], [0.5, 1.0, 1.0, 1.0], Transition::Fade),
            [0.25, 0.5, 0.0, 0.0],
        );
        close(
            weights([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 1.0, 1.0], Transition::Cut),
            [0.0; DECKS],
        );
    }

    #[test]
    fn blend_sums_the_weighted_layers() {
        let (a, b) = ([200u8, 100, 0, 255], [0u8, 100, 200, 255]);
        let mut out = [9u8; 4];
        blend(&mut out, &[(&a, 0.5), (&b, 0.5)]);
        assert_eq!(out, [100, 100, 100, 255]);
        blend(&mut out, &[(&a, 1.0)]);
        assert_eq!(out, a);
        blend(&mut out, &[(&a, 0.0), (&b, 0.0)]);
        assert_eq!(out, [0; 4]);
    }
}
