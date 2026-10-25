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
//! device rate by `Source::frame_at_step` (windowed sinc) and wrap at an active loop end like the plain path, so a loop
//! boundary inside a grain is seamless.
//!
//! Transients: a grain boundary that lands on a drum hit smears it (a hit half-faded in, or played twice a few ms apart).
//! So each block looks at the source for onsets (a jump in the energy of the first difference of the mono sum, per
//! `BIN` frames). A new onset in the next grain, with none in the previous grain's continuation to match, moves the
//! splice to just before it: the old grain plays on untouched, a short `XF` fade ends at the onset, the new grain starts
//! there at full level and exactly at its nominal position. An onset both grains have (the same hit, `|dt|` within
//! `TOL`, as the grains overlap in the source when slowing down, or the new one jumps ahead when speeding up) is
//! aligned instead: the correlation search runs on the hit alone, so the two copies coincide and the usual 50 % fade sums
//! them to one sharp hit. Blocks without onsets run the plain search and fade.
//!
//! ponytail: quality ceiling. Mono-sum search, one correlation per grain, fixed 46 ms grain, and a hit played twice
//! further apart than `TOL` (tempo outside ~0.62..1.6) still flams; only the first onset of a grain is placed, and
//! dense polyphony turns phasey. Upgrade: a phase-vocoder / elastique-class stretcher. All buffers are allocated in `new`.
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
/// Onset detector: frames per energy bin (`SEG` is a whole number of them), bins of history an onset is measured against,
/// the energy ratio that makes one (~8 dB) and the energy below which nothing is (about -50 dBFS of first difference).
const BIN: usize = 32;
const LOOKBACK: usize = 4;
const RISE: f32 = 6.0;
const FLOOR: f32 = 1e-5;
/// Splice at an onset: the fade is at most `XF` frames and ends at the onset, at least `MIN_XF` long.
const XF: usize = 128;
const MIN_XF: usize = 32;
/// Aligning a shared onset: the correlation window starts `PRE` frames before it and is `ALIGN` long.
const PRE: usize = 64;
const ALIGN: usize = 512;

/// One stretcher per voice; all working buffers are allocated in [`Stretch::new`].
pub struct Stretch {
    seg: Vec<f32>,    // stereo source region around the nominal position, SEG frames
    mono: Vec<f32>,   // SEG
    target: Vec<f32>, // mono natural continuation of the previous grain, HS
    cont: Vec<f32>,   // stereo natural continuation of the previous grain (its second half, unwindowed), HS frames
    en: Vec<f32>,     // first-difference energy of `mono` per BIN frames
    out: Vec<f32>,    // stereo output block, HS frames
    rise: Vec<f32>,   // HS Hann rising half (falling half = 1 - rise)
    at: usize,        // next unread frame of `out`; HS = exhausted
    primed: bool,     // a previous grain exists
    cont_onset: Option<usize>, // first onset in `cont`, in frames from its start
    prev_pos: f64,    // the nominal position and chosen offset of the previous grain
    prev_d: usize,
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
            cont: vec![0.0; 2 * HS],
            en: vec![0.0; SEG / BIN],
            out: vec![0.0; 2 * HS],
            rise,
            at: HS,
            primed: false,
            cont_onset: None,
            prev_pos: 0.0,
            prev_d: TOL,
        }
    }

    /// Forget the previous grain: the next one starts raw at the nominal position (seek, pause, key lock off).
    pub fn reset(&mut self) {
        self.at = HS;
        self.primed = false;
        self.cont_onset = None;
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

    /// Correlation of `target[a..a + len]` against `mono[d + a..]` over the decimated window, normalised by the candidate's energy.
    fn score(&self, d: usize, a: usize, len: usize) -> f32 {
        let (mut c, mut e) = (0.0f32, 1e-6f32);
        for (t, m) in self.target[a..a + len]
            .iter()
            .step_by(DECIM)
            .zip(self.mono[d + a..].iter().step_by(DECIM))
        {
            c += t * m;
            e += m * m;
        }
        c / e.sqrt()
    }

    /// Best offset in `lo..=2*TOL` for the window `a..a + len` of the continuation, searched outward from the centre; ties
    /// keep the centre (rate 1 = identity). `lo` past the range (a hit just played too far ahead to start after) gives `2 * TOL`.
    fn best(&self, a: usize, len: usize, lo: usize) -> usize {
        let (mut d, mut s) = (lo.min(2 * TOL), f32::NEG_INFINITY);
        for o in 0..=TOL {
            let both = [TOL - o, TOL + o];
            for &c in if o == 0 { &both[..1] } else { &both[..] } {
                if c < lo {
                    continue;
                }
                let sc = self.score(c, a, len);
                if sc > s {
                    (d, s) = (c, sc);
                }
            }
        }
        d
    }

    /// Fill `en` from `mono`: the mean square of the first difference per `BIN` frames (it follows hits, not bass).
    fn energy(&mut self) {
        let mut prev = 0.0;
        for (e, bin) in self.en.iter_mut().zip(self.mono.chunks_exact(BIN)) {
            let mut sum = 0.0;
            for &m in bin {
                sum += (m - prev) * (m - prev);
                prev = m;
            }
            *e = sum / BIN as f32;
        }
    }

    /// The first onset starting in `mono[from..to]` (`from` at least `LOOKBACK` bins in): the frame its bin starts at.
    fn onset(&self, from: usize, to: usize) -> Option<usize> {
        (from.div_ceil(BIN)..to / BIN)
            .find(|&b| {
                let before = self.en[b - LOOKBACK..b].iter().sum::<f32>() / LOOKBACK as f32;
                self.en[b] > RISE * before + FLOOR
            })
            .map(|b| b * BIN)
    }

    /// The last onset starting in `mono[..to]`: the frame its bin starts at.
    fn last_onset(&self, to: usize) -> Option<usize> {
        (LOOKBACK..to / BIN)
            .rev()
            .find(|&b| {
                let before = self.en[b - LOOKBACK..b].iter().sum::<f32>() / LOOKBACK as f32;
                self.en[b] > RISE * before + FLOOR
            })
            .map(|b| b * BIN)
    }

    /// Fill `out` with the next HS frames: the previous grain's continuation and the new grain, crossfaded.
    fn block(&mut self, src: &impl Source, pos: f64, k: f64, lp: Loop) {
        for j in 0..SEG {
            let q = advance(pos, (j as f64 - TOL as f64) * k, lp);
            let [l, r] = if q < 0.0 {
                [0.0; 2]
            } else {
                src.frame_at_step(q, k).unwrap_or([0.0; 2])
            };
            self.seg[2 * j] = l;
            self.seg[2 * j + 1] = r;
            self.mono[j] = 0.5 * (l + r);
        }
        self.energy();
        // The crossfade runs over a..b of the block: the whole block (Hann) unless an onset is spliced at.
        let (mut a, mut b, mut d) = (0, HS, TOL);
        if self.primed {
            // Frames at the head of the new grain the previous block has played already (the grains overlap in the source
            // when slowing down); an onset there is a repeat, not a new hit.
            let overlap = ((self.prev_pos - pos) / k + (HS + self.prev_d) as f64 - TOL as f64)
                .clamp(0.0, HS as f64) as usize;
            let new = self.onset(TOL + overlap, TOL + HS).map(|j| j - TOL);
            // A hit the previous block just played is in the source before TOL + overlap: a grain that starts before it
            // would fade it in a second time (an echo of the hit), so the search starts after it.
            let lo = self.last_onset(TOL + overlap).map_or(0, |j| j + 2 * BIN);
            match (new, self.cont_onset) {
                (Some(t), Some(c)) if t.abs_diff(c + overlap) <= TOL => {
                    let from = c.saturating_sub(PRE).min(HS - ALIGN);
                    d = self.best(from, ALIGN, lo);
                }
                (Some(t), _) => {
                    b = t.max(MIN_XF);
                    a = b.saturating_sub(XF);
                }
                (None, _) => d = self.best(0, HS, lo),
            }
        }
        for i in 0..HS {
            let w = if !self.primed || i >= b {
                1.0
            } else if i < a {
                0.0
            } else {
                self.rise[(i - a) * HS / (b - a)]
            };
            for c in 0..2 {
                self.out[2 * i + c] = (1.0 - w) * self.cont[2 * i + c] + w * self.seg[2 * (d + i) + c];
                self.cont[2 * i + c] = self.seg[2 * (d + HS + i) + c];
            }
            self.target[i] = self.mono[d + HS + i];
        }
        self.cont_onset = self.onset(d + HS, d + 2 * HS).map(|j| j - d - HS);
        self.prev_pos = pos;
        self.prev_d = d;
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

    /// Hann-windowed 3 kHz bursts of 24 frames (peak ~0.7) every `period` frames from `period` on, silence between: a hit that
    /// is band-limited, so a fractional read keeps its height.
    fn hits(secs: f64, period: usize) -> Pcm {
        let n = (44100.0 * secs) as usize;
        let mut v = vec![0.0f32; 2 * n];
        for start in (period..n - 24).step_by(period) {
            for j in 0..24 {
                let (t, tau) = (j as f64, std::f64::consts::TAU);
                let x = (0.8 * (0.5 - 0.5 * (tau * t / 24.0).cos()) * (tau * 3000.0 * t / 44100.0).sin()) as f32;
                v[2 * (start + j)] = x;
                v[2 * (start + j) + 1] = x;
            }
        }
        Pcm {
            rate: 44100,
            samples: v,
        }
    }

    /// The stretcher's first `n` output frames (left channel) at `tempo`, grains read 1:1.
    fn render(src: &Pcm, tempo: f64, n: usize) -> Vec<f32> {
        let (mut s, mut pos) = (Stretch::new(), 0.0);
        (0..n)
            .map(|_| {
                let x = s.next(src, pos, 1.0, None)[0];
                pos = advance(pos, tempo, None);
                x
            })
            .collect()
    }

    /// Runs of output above 0.1, joined across gaps under 60 frames: (first frame, last frame, loudest frame, peak).
    fn bursts(out: &[f32]) -> Vec<(usize, usize, usize, f32)> {
        let mut found: Vec<(usize, usize, usize, f32)> = vec![];
        for (i, x) in out.iter().enumerate().filter(|(_, x)| x.abs() > 0.1) {
            match found.last_mut() {
                Some(b) if i - b.1 < 60 => {
                    b.1 = i;
                    if x.abs() > b.3 {
                        (b.2, b.3) = (i, x.abs());
                    }
                }
                _ => found.push((i, i, i, x.abs())),
            }
        }
        found
    }

    #[test]
    fn hits_stay_single_and_sharp_at_tempos_far_from_one() {
        let (period, n) = (7001, 44100 * 4);
        let src = hits(6.0, period);
        for tempo in [0.8, 1.25] {
            let found = bursts(&render(&src, tempo, n));
            let mut interior = 0;
            for m in 1.. {
                let at = (m * period) as f64 / tempo;
                if at > (n - 900) as f64 {
                    break;
                }
                interior += 1;
                // the heard hit may lead or lag its nominal time by a grain's worth of search (TOL) and the tempo error
                let near: Vec<_> = found.iter().filter(|b| b.2.abs_diff(at as usize) <= 900).collect();
                assert_eq!(near.len(), 1, "tempo {tempo}, hit {m} at {at:.0}: {found:?}");
                let (first, last, _, peak) = *near[0];
                assert!(peak > 0.5, "tempo {tempo}, hit {m} smeared: peak {peak}");
                assert!(last - first < 60, "tempo {tempo}, hit {m} smeared over {} frames", last - first);
            }
            assert!(
                found.len() <= interior + 1,
                "tempo {tempo}: {} bursts for {interior} hits (an echo)",
                found.len()
            );
        }
    }

    #[test]
    fn onsets_are_found_in_the_first_difference_and_not_in_a_steady_tone() {
        let mut s = Stretch::new();
        s.mono.fill(0.0);
        for j in 0..24 {
            s.mono[1000 + j] = 0.5 * (j as f32 * 0.9).sin();
        }
        s.energy();
        assert_eq!(s.onset(TOL, SEG - HS), Some(992));
        assert_eq!(s.onset(TOL, 992), None, "nothing before it");
        let tone = sine(44100, 440.0, 0.1);
        for (j, m) in s.mono.iter_mut().enumerate() {
            *m = tone.samples[2 * j];
        }
        s.energy();
        assert_eq!(s.onset(TOL, SEG), None, "a steady tone has no onset");
    }
}
