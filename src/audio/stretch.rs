//! Key lock and key shift: real-time WSOLA (waveform-similarity overlap-add) on the source PCM.
//!
//! Output is a stream of grains of `2*HS` frames read at a constant resampling step (1:1, or `pitch`
//! times it for a key shift: pitch changes, the grain length in output frames does not), Hann-windowed, overlapped
//! 50 %. A grain is read at the *nominal* source position (the player's clock: `rate` times faster
//! or slower than the output, so tempo changes) but its start slides by up to ±`TOL` frames to the
//! offset whose first half best matches (normalised cross-correlation on the mono sum, every
//! `DECIM`-th frame) the natural continuation of the previous grain; that keeps the overlap in phase.
//! The player's `advance` stays the only position truth: the clock reports the nominal position and the
//! audio lags/leads it by the chosen offset, at most `TOL` frames (~9 ms). Reads are resampled to the
//! device rate by `Pcm::frame_at` and wrap at an active loop end like the plain path, so a loop
//! boundary inside a grain is seamless.
//!
//! ponytail: quality ceiling. Mono-sum search, one correlation per grain, fixed 46 ms grain: transients
//! smear (pre-echo) beyond ~±15 % tempo and dense polyphony turns phasey. Upgrade: transient-adaptive
//! grain size, or a phase-vocoder / elastique-class stretcher. All buffers are allocated in `new`.
//! Tempo/pitch ratios beyond 2x (e.g. key lock at +100 % tempo with a -12 st shift) skip source material:
//! the nominal hop then exceeds the grain's span.

use super::{Loop, Source, advance};

/// Synthesis hop in frames; grains are `2 * HS` long.
const HS: usize = 1024;
/// Search half-width around the nominal grain start, in frames.
const TOL: usize = 384;
/// Correlation looks at every DECIM-th frame.
const DECIM: usize = 2;
/// Source frames read per grain: the grain plus the search range on both sides.
const SEG: usize = 2 * HS + 2 * TOL;

/// One stretcher per voice; all working buffers are allocated in [`Stretch::new`].
pub struct Stretch {
    seg: Vec<f32>,    // stereo source region around the nominal position, SEG frames
    mono: Vec<f32>,   // SEG
    target: Vec<f32>, // mono natural continuation of the previous grain, HS
    tail: Vec<f32>,   // stereo windowed second half of the previous grain, HS frames
    out: Vec<f32>,    // stereo output block, HS frames
    rise: Vec<f32>,   // HS Hann rising half (falling half = 1 - rise)
    at: usize,        // next unread frame of `out`; HS = exhausted
    primed: bool,     // a previous grain exists
}

impl Stretch {
    pub fn new() -> Stretch {
        let rise = (0..HS)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / HS as f32).cos())
            .collect();
        Stretch {
            seg: vec![0.0; 2 * SEG],
            mono: vec![0.0; SEG],
            target: vec![0.0; HS],
            tail: vec![0.0; 2 * HS],
            out: vec![0.0; 2 * HS],
            rise,
            at: HS,
            primed: false,
        }
    }

    /// Forget the previous grain: the next one starts raw at the nominal position (seek, pause, key lock off).
    pub fn reset(&mut self) {
        self.at = HS;
        self.primed = false;
    }

    /// Next output frame. `pos` is the clock's source-frame position of *this* output frame, `k` source
    /// frames per output frame the grains are read at (`src_rate / dev_rate` for pitch 1; times the pitch
    /// factor for a key shift; negative plays the grains backwards), `lp` the loop in source frames.
    pub fn next(&mut self, src: &impl Source, pos: f64, k: f64, lp: Loop) -> [f32; 2] {
        if self.at == HS {
            self.block(src, pos, k, lp);
            self.at = 0;
        }
        let i = self.at;
        self.at += 1;
        [self.out[2 * i], self.out[2 * i + 1]]
    }

    /// Correlation of `target` against `mono[d..]` over the decimated window, normalised by the candidate's energy.
    fn score(&self, d: usize) -> f32 {
        let (mut c, mut e) = (0.0f32, 1e-6f32);
        for (t, m) in self
            .target
            .iter()
            .step_by(DECIM)
            .zip(self.mono[d..].iter().step_by(DECIM))
        {
            c += t * m;
            e += m * m;
        }
        c / e.sqrt()
    }

    /// Best offset in 0..=2*TOL, searched outward from the centre; ties keep the centre (rate 1 = identity).
    fn best(&self) -> usize {
        let (mut d, mut s) = (TOL, self.score(TOL));
        for o in 1..=TOL {
            for c in [TOL - o, TOL + o] {
                let sc = self.score(c);
                if sc > s {
                    (d, s) = (c, sc);
                }
            }
        }
        d
    }

    /// Fill `out` with the next HS frames: previous grain's tail + the new grain's rising half.
    fn block(&mut self, src: &impl Source, pos: f64, k: f64, lp: Loop) {
        for j in 0..SEG {
            let q = advance(pos, (j as f64 - TOL as f64) * k, lp);
            let [l, r] = if q < 0.0 {
                [0.0; 2]
            } else {
                src.frame_at(q).unwrap_or([0.0; 2])
            };
            self.seg[2 * j] = l;
            self.seg[2 * j + 1] = r;
            self.mono[j] = 0.5 * (l + r);
        }
        let d = if self.primed { self.best() } else { TOL };
        for i in 0..HS {
            let w = self.rise[i];
            for c in 0..2 {
                let s = self.seg[2 * (d + i) + c];
                self.out[2 * i + c] = if self.primed {
                    self.tail[2 * i + c] + w * s
                } else {
                    s
                };
                self.tail[2 * i + c] = (1.0 - w) * self.seg[2 * (d + HS + i) + c];
            }
            self.target[i] = self.mono[d + HS + i];
        }
        self.primed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::{super::Pcm, *};

    fn sine(rate: u32, hz: f64, secs: f64) -> Pcm {
        let n = (rate as f64 * secs) as usize;
        Pcm {
            rate,
            samples: (0..n)
                .flat_map(|i| {
                    let v = (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin() as f32
                        * 0.5;
                    [v, v]
                })
                .collect(),
        }
    }

    #[test]
    fn rate_one_is_identity() {
        let src = sine(44100, 440.0, 1.0);
        let mut s = Stretch::new();
        for i in 0..20000 {
            let [l, _] = s.next(&src, i as f64, 1.0, None);
            assert!(
                (l - src.samples[2 * i]).abs() < 1e-4,
                "frame {i}: {l} vs {}",
                src.samples[2 * i]
            );
        }
    }

    #[test]
    fn stretching_keeps_pitch_and_only_changes_source_consumption() {
        let src = sine(44100, 440.0, 8.0);
        for rate in [0.92, 1.1, 1.5] {
            let mut s = Stretch::new();
            let (mut pos, mut out) = (0.0, vec![]);
            for _ in 0..44100 * 2 {
                out.push(s.next(&src, pos, 1.0, None)[0]);
                pos = advance(pos, rate, None);
            }
            let ups = out[11025..]
                .windows(2)
                .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
                .count() as f64;
            let hz = ups / ((out.len() - 11025) as f64 / 44100.0);
            assert!((hz / 440.0 - 1.0).abs() < 0.01, "rate {rate}: {hz:.1} Hz");
        }
    }
}
