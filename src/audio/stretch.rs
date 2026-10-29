//! Key lock and key shift: real-time WSOLA (waveform-similarity overlap-add) on the source PCM.
//!
//! Output is a stream of grains of `2*HS` frames read at a constant resampling step (1:1, or `pitch`
//! times it for a key shift: pitch changes, the grain length in output frames does not), Hann-windowed, overlapped
//! 50 %. A grain is read at the *nominal* source position (the player's clock: `rate` times faster
//! or slower than the output, so tempo changes) but its start slides by up to ~17.4 ms to the
//! offset whose first half best matches (normalised stereo cross-correlation, every
//! `DECIM`-th frame) the natural continuation of the previous grain; that keeps the overlap in phase.
//! The player's `advance` stays the only position truth: the clock reports the nominal position and the
//! grain starts may lead/lag it by the chosen offset (at most ~17.4 ms at unity pitch). Within each grain,
//! pitch-rate reads and the nominal tempo advance can differ. Reads are resampled to the
//! device rate by `Source::frame_at_step` (windowed sinc) and wrap at an active loop end like the plain path, so a loop
//! boundary inside a grain is seamless.
//!
//! Transients: a grain boundary that lands on a drum hit smears it (a hit half-faded in, or played twice a few ms apart).
//! So each block looks at the source for onsets (a jump in the energy of the first difference of the mono sum, per
//! `BIN` frames). A new onset in the next grain, with none in the previous grain's continuation to match, moves the
//! splice to just before it: the old grain plays on untouched, a short `XF` fade ends at the onset, the new grain starts
//! there at full level and exactly at its nominal position. An onset both grains have (the same hit, `|dt|` within
//! the search tolerance, as the grains overlap in the source when slowing down, or the new one jumps ahead when speeding up) is
//! aligned instead: the correlation search runs on the hit alone, so the two copies coincide and the usual 50 % fade sums
//! them to one sharp hit. Blocks without onsets run the plain search and fade.
//!
//! shortcut: one lag for both channels, a fixed 2048-frame grain, and only the first onset placed per grain
//! still make dense polyphony phasey; upgrade to a phase-vocoder / elastique-class stretcher when that is required.
//! The bounded search covers a whole period down to ~29 Hz at pitch 1. All buffers are allocated in `new`.
//! Tempo/pitch ratios beyond 2x (e.g. key lock at +100 % tempo with a -12 st shift) skip source material:
//! the nominal hop then exceeds the grain's span.

use super::{Loop, Source, advance};

/// Synthesis hop in frames; grains are `2 * HS` long.
const HS: usize = 1024;
/// Correlation looks at every DECIM-th frame.
const DECIM: usize = 2;
/// Onset detector: frames per energy bin, bins of history an onset is measured against,
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
    seg: Vec<f32>,             // stereo source region around the nominal position
    mono: Vec<f32>,            // mono source region, for onset detection only
    cont: Vec<f32>, // stereo natural continuation of the previous grain (its second half, unwindowed), HS frames
    en: Vec<f32>,   // first-difference energy of `mono` per BIN frames
    out: Vec<f32>,  // stereo output block, HS frames
    rise: Vec<f32>, // HS Hann rising half (falling half = 1 - rise)
    at: usize,      // next unread frame of `out`; HS = exhausted
    primed: bool,   // a previous grain exists
    cont_onset: Option<usize>, // first onset in `cont`, in frames from its start
    prev_pos: f64,    // the nominal position and chosen offset of the previous grain
    prev_d: usize,
    tol: usize, // search half-width in output frames, scaled to the actual device rate
}

impl Stretch {
    pub fn new(rate: u32) -> Stretch {
        // A full bass period must fit in the lag range, including at 96 kHz; otherwise the
        // best available splice keeps walking out of phase and the bass follows the tempo.
        let tol = (768 * rate as usize).div_ceil(44100 * BIN).max(LOOKBACK) * BIN;
        let seg = 2 * HS + 2 * tol;
        let rise = (0..HS)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / HS as f32).cos())
            .collect();
        Stretch {
            seg: vec![0.0; 2 * seg],
            mono: vec![0.0; seg],
            cont: vec![0.0; 2 * HS],
            en: vec![0.0; seg / BIN],
            out: vec![0.0; 2 * HS],
            rise,
            at: HS,
            primed: false,
            cont_onset: None,
            prev_pos: 0.0,
            prev_d: tol,
            tol,
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

    /// Stereo correlation against the previous continuation, normalised by candidate energy.
    fn score(&self, d: usize, a: usize, len: usize) -> f32 {
        let (mut c, mut e) = (0.0f32, 1e-6f32);
        for (t, m) in self.cont[2 * a..2 * (a + len)]
            .chunks_exact(2)
            .step_by(DECIM)
            .zip(self.seg[2 * (d + a)..].chunks_exact(2).step_by(DECIM))
        {
            c += t[0] * m[0] + t[1] * m[1];
            e += m[0] * m[0] + m[1] * m[1];
        }
        c / e.sqrt()
    }

    /// Search outward from the nominal start, then refine the best coarse lag to one frame.
    /// Stereo scoring cannot cancel an antiphase signal; the coarse pass bounds its cost.
    fn best(&self, a: usize, len: usize, lo: usize) -> usize {
        let end = 2 * self.tol;
        let stride = (4 * self.tol).div_ceil(768);
        let (mut d, mut s) = (lo.min(end), f32::NEG_INFINITY);
        for o in (0..=self.tol).step_by(stride) {
            let both = [self.tol - o, self.tol + o];
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
        let coarse = d;
        for c in coarse.saturating_sub(stride - 1).max(lo)..=(coarse + stride - 1).min(end) {
            let sc = self.score(c, a, len);
            if sc > s {
                (d, s) = (c, sc);
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
        for j in 0..self.mono.len() {
            let q = advance(pos, (j as f64 - self.tol as f64) * k, lp);
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
        let (mut a, mut b, mut d) = (0, HS, self.tol);
        if self.primed {
            // Frames at the head of the new grain the previous block has played already (the grains overlap in the source
            // when slowing down); an onset there is a repeat, not a new hit.
            let overlap = ((self.prev_pos - pos) / k + (HS + self.prev_d) as f64 - self.tol as f64)
                .clamp(0.0, HS as f64) as usize;
            let new = self
                .onset(self.tol + overlap, self.tol + HS)
                .map(|j| j - self.tol);
            // A hit the previous block just played is in the source before tol + overlap: a grain that starts before it
            // would fade it in a second time (an echo of the hit), so the search starts after it.
            let lo = self
                .last_onset(self.tol + overlap)
                .map_or(0, |j| j + 2 * BIN);
            match (new, self.cont_onset) {
                (Some(t), Some(c)) if t.abs_diff(c + overlap) <= self.tol => {
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
        let mut s = Stretch::new(44100);
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
            let mut s = Stretch::new(44100);
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
        let (mut s, mut pos) = (Stretch::new(src.rate), 0.0);
        (0..n)
            .map(|_| {
                let x = s.next(src, pos, 1.0, None)[0];
                pos = advance(pos, tempo, None);
                x
            })
            .collect()
    }

    #[test]
    fn small_tempo_changes_keep_local_pitch_stable_in_stereo() {
        let mut failures = Vec::new();
        for rate in [44100, 48000, 96000] {
            for hz in [48.0, 440.0] {
                for (left, right) in [(1.0, 1.0), (1.0, -1.0), (0.0, 1.0)] {
                    let mut src = sine(rate, hz, 3.0);
                    for frame in src.samples.chunks_exact_mut(2) {
                        frame[0] *= left;
                        frame[1] *= right;
                    }
                    for tempo in [0.97, 1.0, 1.03, 1.06] {
                        let (mut s, mut pos) = (Stretch::new(rate), 0.0);
                        let channel = usize::from(left == 0.0);
                        let out: Vec<_> = (0..rate)
                            .map(|_| {
                                let x = s.next(&src, pos, 1.0, None)[channel];
                                pos = advance(pos, tempo, None);
                                x
                            })
                            .collect();
                        let crossings: Vec<_> = out[rate as usize / 5..]
                            .windows(2)
                            .enumerate()
                            .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
                            .map(|(i, w)| i as f64 - w[0] as f64 / (w[1] - w[0]) as f64)
                            .collect();
                        assert!(crossings.len() > 20, "missing output at {hz} Hz");
                        let cents: Vec<_> = crossings
                            .windows(5)
                            .map(|w| 1200.0 * (4.0 * rate as f64 / ((w[4] - w[0]) * hz)).log2())
                            .collect();
                        let worst = cents.iter().map(|c| c.abs()).fold(0.0f64, f64::max);
                        let mean = cents.iter().sum::<f64>() / cents.len() as f64;
                        eprintln!(
                            "{rate} Hz, {hz} Hz stereo {left}/{right} tempo {tempo}: mean {mean:.2}, worst {worst:.2} cents"
                        );
                        if worst > 12.0 {
                            failures.push(format!("{rate} Hz, {hz} Hz stereo {left}/{right} tempo {tempo}: {worst:.2} cents"));
                        }
                        assert!((pos - rate as f64 * tempo).abs() < 1e-6, "tempo changed");
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "unstable local pitch:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    #[ignore = "offline measurement: requires OPENFADER_STRETCH_INPUT/OUTPUT f32le stereo paths"]
    fn render_issue35_probe() {
        use std::io::{BufWriter, Write};
        let input = std::env::var("OPENFADER_STRETCH_INPUT").expect("input f32le path");
        let output = std::env::var("OPENFADER_STRETCH_OUTPUT").expect("output f32le path");
        let rate: u32 = std::env::var("OPENFADER_STRETCH_RATE")
            .unwrap_or_else(|_| "44100".into())
            .parse()
            .expect("source sample rate");
        let device_rate: u32 = std::env::var("OPENFADER_STRETCH_DEVICE_RATE")
            .unwrap_or_else(|_| rate.to_string())
            .parse()
            .expect("output sample rate");
        let tempo: f64 = std::env::var("OPENFADER_STRETCH_TEMPO")
            .unwrap_or_else(|_| "1.03".into())
            .parse()
            .expect("tempo ratio");
        assert!(rate > 0 && device_rate > 0 && tempo.is_finite() && tempo > 0.0);
        let bytes = std::fs::read(input).expect("read input");
        assert!(
            bytes.len() % 8 == 0 && !bytes.is_empty(),
            "interleaved stereo f32le"
        );
        let src = Pcm {
            rate,
            samples: bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect(),
        };
        assert!(src.samples.iter().all(|v| v.is_finite()));
        let frames = src.samples.len() / 2;
        let k = rate as f64 / device_rate as f64;
        let n = (frames as f64 / (tempo * k)).ceil() as usize;
        let (mut s, mut pos) = (Stretch::new(device_rate), 0.0);
        let mut writer = BufWriter::new(std::fs::File::create(output).expect("create output"));
        let started = std::time::Instant::now();
        for _ in 0..n {
            for x in s.next(&src, pos, k, None) {
                writer.write_all(&x.to_le_bytes()).expect("write output");
            }
            pos = advance(pos, tempo * k, None);
        }
        writer.flush().expect("flush output");
        eprintln!(
            "Stretch probe: {rate} -> {device_rate} Hz, tempo {tempo}, {frames} -> {n} frames, {:.3}s render",
            started.elapsed().as_secs_f64()
        );
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
        let mut s = Stretch::new(44100);
        s.mono.fill(0.0);
        for j in 0..24 {
            s.mono[1000 + j] = 0.5 * (j as f32 * 0.9).sin();
        }
        s.energy();
        assert_eq!(s.onset(s.tol, s.mono.len() - HS), Some(992));
        assert_eq!(s.onset(s.tol, 992), None, "nothing before it");
        let tone = sine(44100, 440.0, 0.1);
        for (j, m) in s.mono.iter_mut().enumerate() {
            *m = tone.samples[2 * j];
        }
        s.energy();
        assert_eq!(
            s.onset(s.tol, s.mono.len()),
            None,
            "a steady tone has no onset"
        );
    }
}
