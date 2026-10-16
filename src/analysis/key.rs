//! Musical key from decoded PCM, reported in Camelot notation ("8A", "8B", ... as DJ libraries show it).
//!
//! Pipeline (own design, textbook chroma/profile key finding): mono downmix, tent-filter decimation to
//! ~11 kHz, Hann STFT (8192, hop 4096), spectral peaks with parabolic frequency refinement, tuning estimate from
//! the circular mean of the peaks' offset from the semitone grid, per-frame normalised chroma summed over the
//! track, Pearson correlation with the 24 rotated key profiles.
//!
//! Profiles: Krumhansl & Kessler probe-tone ratings (Krumhansl 1990, "Cognitive Foundations of Musical Pitch").
//! Temperley's profiles (Kostka-Payne based, "Music and Probability", 2007) were tried and scored lower on a real
//! USB library (tests/key_usb.rs); the harmonic voting and the minor prior were tuned on that library too.

use super::Stft;

/// Krumhansl-Kessler probe-tone ratings, tonic first.
const KK_MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const KK_MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

const N: usize = 8192;
const HOP: usize = N / 2;
/// Target rate after decimation.
const LOW_RATE: u32 = 11025;
/// Peak frequency range that votes for a pitch class.
const F_MIN: f32 = 60.0;
const F_MAX: f32 = 2500.0;
/// A peak at `f` also votes, equally, for `f/2 .. f/HARMONICS`: it may be that harmonic of a lower note.
const HARMONICS: u32 = 6;
/// Added to the minor-key correlations. ponytail: tuned on one 24-track library (20 minor, 4 major by its own keys); it is a
/// genre prior (club music is mostly minor, [INFERENCE] not measured elsewhere), drop it or make it a setting for other material.
const MINOR_PRIOR: f32 = 0.1;
/// A frame whose strongest bin is below this (≈ −60 dBFS sine) is silence.
const SILENCE: f32 = 2.0;

/// Spectral peaks as `(frame, fractional MIDI note, magnitude)`; silent frames have none.
fn spectral_peaks(x: &[f32], rate: f32) -> Vec<(u32, f32, f32)> {
    let mut stft = Stft::new(N);
    let mut peaks = Vec::new();
    let bin_hz = rate / N as f32;
    let (lo, hi) = (
        (F_MIN / bin_hz) as usize,
        ((F_MAX / bin_hz) as usize).min(N / 2 - 2),
    );
    for (fi, frame) in x.windows(N).step_by(HOP).enumerate() {
        let mag = stft.magnitudes(frame);
        let top = mag[lo..=hi].iter().cloned().fold(0.0, f32::max);
        if top < SILENCE {
            continue;
        }
        let floor = top * 0.003;
        for i in lo.max(1)..=hi {
            let (a, b, c) = (mag[i - 1], mag[i], mag[i + 1]);
            if b > a && b >= c && b > floor {
                let (a, b, c) = (a.max(1e-9).ln(), b.ln(), c.max(1e-9).ln());
                let delta = 0.5 * (a - c) / (a - 2.0 * b + c);
                let hz = (i as f32 + delta) * bin_hz;
                peaks.push((fi as u32, 69.0 + 12.0 * (hz / 440.0).log2(), mag[i]));
            }
        }
    }
    peaks
}

/// Twelve-bin pitch-class profile (C = 0) summed over the track, tuning-corrected, every peak also voting for the
/// pitch classes of the fundamentals it could be a harmonic of. All zero for silence.
pub fn chroma(samples: &[f32], rate: u32) -> [f32; 12] {
    let d = (rate / LOW_RATE).max(1) as usize;
    let peaks = spectral_peaks(&super::mono_decimate(samples, d), rate as f32 / d as f32);
    // Per-frame normalised weights, so loud and quiet passages count alike.
    let mut sums = vec![0f32; peaks.last().map_or(0, |p| p.0 as usize + 1)];
    for p in &peaks {
        sums[p.0 as usize] += p.2;
    }
    let peaks: Vec<(f32, f32)> = peaks
        .iter()
        .map(|p| (p.1, p.2 / sums[p.0 as usize]))
        .collect();
    // Tuning: circular mean of each peak's offset from the semitone grid (period 1 semitone).
    let (s, c) = peaks.iter().fold((0f32, 0f32), |(s, c), &(m, w)| {
        let a = std::f32::consts::TAU * m;
        (s + w * a.sin(), c + w * a.cos())
    });
    let tuning = s.atan2(c) / std::f32::consts::TAU; // semitones, in (-0.5, 0.5]
    let mut out = [0f32; 12];
    for &(m, w) in &peaks {
        for h in 1..=HARMONICS {
            out[((m - 12.0 * (h as f32).log2() - tuning).round() as i32).rem_euclid(12)
                as usize] += w;
        }
    }
    out
}

/// Camelot name of a key: major tonic pitch class `t` is `(8 + 7t) mod 12`, minor shares its relative major's number.
fn camelot(tonic: usize, minor: bool) -> String {
    let major = if minor { (tonic + 3) % 12 } else { tonic };
    let n = (8 + 7 * major) % 12;
    format!(
        "{}{}",
        if n == 0 { 12 } else { n },
        if minor { 'A' } else { 'B' }
    )
}

fn pearson(a: &[f32; 12], b: impl Fn(usize) -> f32) -> f32 {
    let (ma, mb) = (
        a.iter().sum::<f32>() / 12.0,
        (0..12).map(&b).sum::<f32>() / 12.0,
    );
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b(i) - mb);
        (ab, aa, bb) = (ab + x * y, aa + x * x, bb + y * y);
    }
    ab / (aa * bb).sqrt().max(1e-12)
}

/// Best-correlating key of a chroma vector.
pub fn from_chroma(c: &[f32; 12]) -> Option<String> {
    if c.iter().sum::<f32>() <= 0.0 {
        return None;
    }
    let best = (0..24)
        .map(|k| {
            let (t, p) = (k % 12, if k < 12 { &KK_MAJOR } else { &KK_MINOR });
            (
                pearson(c, |i| p[(i + 12 - t) % 12]) + if k < 12 { 0.0 } else { MINOR_PRIOR },
                k,
            )
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    Some(camelot(best.1 % 12, best.1 >= 12))
}

/// Key of interleaved stereo f32 PCM; `None` for silence or audio shorter than one analysis window (~0.75 s).
pub fn detect(samples: &[f32], rate: u32) -> Option<String> {
    from_chroma(&chroma(samples, rate))
}

/// A key as `(Camelot number 1..=12, major)`: "A" = minor = `false`, "B" = major = `true`.
pub type Key = (u8, bool);

/// The key a text names, in any notation: Camelot ("10A"), Open Key ("1m" minor / "1d" major, C major = 1d, A minor = 1m,
/// +1 per fifth) or a musical name ("F#m", "Bb", "Am", "C#min", "Dmaj", "E♭"). Musical names map by the standard Camelot
/// wheel (Am = 8A, C = 8B; a fifth up = +1), computed from the tonic's pitch class. `None` for text that is no key.
pub fn parse(text: &str) -> Option<Key> {
    let k = text.trim();
    // "12A" / "1m": a number then one letter.
    if let Some(split) = k.len().checked_sub(1).filter(|&i| k.is_char_boundary(i)) {
        let (digits, last) = (&k[..split], &k[split..]);
        if let Ok(n @ 1..=12) = digits.parse::<u8>() {
            match last {
                "A" | "a" => return Some((n, false)),
                "B" | "b" => return Some((n, true)),
                // Open Key number n is Camelot (n + 6) mod 12 + 1.
                "m" | "M" | "d" | "D" => {
                    return Some(((n + 6) % 12 + 1, matches!(last, "d" | "D")));
                }
                _ => {}
            }
        }
    }
    let mut it = k.chars();
    let mut pc = match it.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    } as i32;
    let rest = it.as_str();
    let (acc, rest) = match rest.chars().next() {
        Some(c @ ('#' | '♯')) => (1, &rest[c.len_utf8()..]),
        Some(c @ '♭') => (-1, &rest[c.len_utf8()..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    pc += acc;
    let minor = match rest.to_lowercase().as_str() {
        "" | "maj" | "major" => false,
        "m" | "min" | "minor" => true,
        _ => return None,
    };
    let steps = (pc - if minor { 9 } else { 0 }).rem_euclid(12) * 7 % 12;
    Some((((steps + 7) % 12 + 1) as u8, !minor))
}

/// [`parse`] as the `collection.key_id` column: 0..=23, `(Camelot number − 1) × 2 + (B = 1)`.
pub fn key_id(text: &str) -> Option<u8> {
    parse(text).map(|(n, major)| (n - 1) * 2 + u8::from(major))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44100;

    /// Four 4 s chords (I IV V I, or i iv V i) of 4-harmonic notes, every frequency shifted by `cents`.
    fn song(tonic: i32, minor: bool, cents: f32) -> Vec<f32> {
        let third = if minor { 3 } else { 4 };
        let chords = [(0, third), (5, third), (7, 4), (0, third)];
        let mut out = Vec::new();
        for (root, third) in chords {
            let notes = [
                tonic + root - 12,
                tonic + root,
                tonic + root + third,
                tonic + root + 7,
            ]
            .map(|n| 48.0 + n as f32 + cents / 100.0);
            let hz: Vec<f32> = notes
                .iter()
                .map(|m| 440.0 * 2f32.powf((m - 69.0) / 12.0))
                .collect();
            for i in 0..RATE as usize * 4 {
                let t = i as f32 / RATE as f32;
                let v: f32 = hz
                    .iter()
                    .map(|f| {
                        (1..=4)
                            .map(|h| (std::f32::consts::TAU * f * h as f32 * t).sin() / h as f32)
                            .sum::<f32>()
                    })
                    .sum();
                out.extend([v * 0.1; 2]);
            }
        }
        out
    }

    #[test]
    fn camelot_wheel() {
        assert_eq!(
            [
                camelot(0, false),
                camelot(9, true),
                camelot(7, false),
                camelot(11, false),
                camelot(1, true)
            ],
            ["8B", "8A", "9B", "1B", "12A"]
        );
    }

    #[test]
    fn triad_progressions_and_tuning_offsets() {
        for (tonic, minor, want) in [
            (0, false, "8B"),
            (9, true, "8A"),
            (6, false, "2B"),
            (2, true, "7A"),
            (3, false, "5B"),
        ] {
            for cents in [-30.0, 0.0, 30.0] {
                assert_eq!(
                    detect(&song(tonic, minor, cents), RATE).as_deref(),
                    Some(want),
                    "tonic {tonic} minor {minor} {cents} cents"
                );
            }
        }
    }

    #[test]
    fn silence_and_too_short_have_no_key() {
        assert_eq!(detect(&vec![0.0; RATE as usize * 20], RATE), None);
        assert_eq!(detect(&[0.1; 2000], RATE), None);
    }

    #[test]
    fn key_ids_from_every_notation() {
        for (text, id) in [
            ("1A", Some(0)),
            ("1B", Some(1)),
            ("8A", Some(14)),
            ("12B", Some(23)),
            ("8a", Some(14)),
            (" 8B ", Some(15)),
            ("1m", Some(14)), // Open Key: A minor = 8A
            ("1d", Some(15)), // C major = 8B
            ("6m", Some(0)),
            ("6d", Some(1)),
            ("12d", Some(13)), // = 7B
            ("Am", Some(14)),
            ("C", Some(15)),
            ("C#", Some(5)),   // Db major = 3B
            ("Dbm", Some(22)), // = 12A
            ("C#m", Some(22)),
            ("Bb", Some(11)),
            ("A♭", Some(7)),
            ("Gb", Some(3)),
            ("F#min", Some(20)),
            ("Dmaj", Some(19)),
            ("", None),
            ("?", None),
            ("13A", None),
            ("0B", None),
            ("H", None),
            ("Xm", None),
        ] {
            assert_eq!(key_id(text), id, "{text:?}");
        }
    }
}
