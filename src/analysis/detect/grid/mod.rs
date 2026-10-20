//! Beat grid from decoded PCM (stereo f32, see [`super`]). The design is ours.
//!
//! Tempo: onset flux at 400 Hz (1000 Hz with [`GridOptions::precision`]; band-split, [`flux`]);
//! autocorrelation proposes candidates and a fold of the onsets modulo each fine-stepped period scores
//! them ([`tempo`]). Searched around the BPM range of [`GridOptions`] (default 85-175: searched 70-180) and
//! folded into that range ([`fold_bpm`]).
//!
//! Phase: the beat is where the mid band (200 Hz - 2.5 kHz) amplitude rises (the kick), not where
//! the onsets are strongest ([`kick_offset`]). A weighted line fit through the onsets near that grid
//! refines tempo and phase ([`fit`]). Bar positions: beat 1 is the one of four with the most
//! low-band energy ([`downbeat`]).
//!
//! Tempo changes (Track Analysis Mode; [`dynamic`]): tempo is also estimated per 8 s window. Normal never
//! tracks: one steady tempo. Dynamic always tracks the beats and merges them into segments of one tempo each.
//! Auto (needs high precision) tracks when a constant grid does not fit a quarter of the periodic windows.
//!
//! Measured on the 24-track test USB (tests/detect_usb.rs): tempo equals the stored library grid's in 24/24
//! and every grid is constant, like the stored ones. The first beat lands within 3 ms of a stored beat
//! in 24/24 (a strongest-onset phase: 18/24 within 30 ms, 4 of the other 6 half a beat off). Our bar
//! position agrees with the stored one on 21/24 (strongest onsets: 11/24); the windows never flag
//! drift (worst window 0.59 of its own best score, flagged below 0.5). ponytail: 24 tracks of one
//! genre set the mid-band choice and the thresholds; other genres may need other bands. Drift
//! tracking is verified on synthetic click tracks only (no real variable-tempo audio available).

mod dynamic;
#[cfg(test)]
mod tests;

use super::{FLUX_HZ, PRECISE_FLUX_HZ, hops};
use crate::analysis::Beat;

/// Track Analysis Mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// One steady tempo and a fixed grid.
    #[default]
    Normal,
    /// Tempo-varying grid: segments of one tempo each.
    Dynamic,
    /// Dynamic when the tempo drifts, else Normal; exists only with high precision.
    Auto,
}

/// What the grid detector does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridOptions {
    pub mode: Mode,
    /// High precision BeatGrid analysis: 1 ms instead of 2.5 ms flux frames and sub-frame onset positions.
    pub precision: bool,
    /// BPM range the tempo is folded into (Normal mode's "BPM Range"; applies to every mode).
    pub bpm_range: (u16, u16),
}

impl Default for GridOptions {
    fn default() -> Self {
        GridOptions {
            mode: Mode::Normal,
            precision: false,
            bpm_range: (85, 175),
        }
    }
}

impl GridOptions {
    /// Auto exists only with high precision; a stored Auto without it reads as Normal.
    pub fn effective_mode(&self) -> Mode {
        if self.mode == Mode::Auto && !self.precision {
            Mode::Normal
        } else {
            self.mode
        }
    }

    /// Whether the beats are tracked into tempo segments, given whether the windows show drift ([`dynamic::drifts`]).
    fn tracks(&self, drifts: bool) -> bool {
        match self.effective_mode() {
            Mode::Normal => false,
            Mode::Dynamic => true,
            Mode::Auto => drifts,
        }
    }

    /// The search band of [`tempo`]: the range widened so that octave candidates exist for the fold.
    fn search(&self) -> (f64, f64) {
        (
            self.bpm_range.0 as f64 * 0.82,
            self.bpm_range.1 as f64 * 1.03,
        )
    }

    pub(super) fn flux_hz(&self) -> u32 {
        if self.precision {
            PRECISE_FLUX_HZ
        } else {
            FLUX_HZ
        }
    }
}

const ENERGY_GAIN: f32 = 1e4;
/// SuperFlux smoothing and look-back windows and the onset-fit tolerance (seconds; 4 hops, 10 hops and 2 hops at 400 Hz).
const SMOOTH_S: f64 = 0.01;
const LOOK_S: f64 = 0.025;
const FIT_S: f64 = 0.005;
/// Rise window of the mid-band amplitude that marks a kick, and the low-band window `[from, to]`
/// around a beat compared across a bar (seconds).
const KICK_S: f64 = 0.06;
const DOWNBEAT_S: [f64; 2] = [-0.02, 0.1];
/// Tempo window (seconds) and the weight of the beat-interval penalty against the onset flux (flux in
/// units of its rms, penalty = TIGHT x relative deviation squared) when a drifting tempo is tracked.
const WINDOW_S: f64 = 8.0;
const TIGHT: f64 = 500.0;

/// Onset flux per hop: sum over bands of how far the log energy (mean of the last [`SMOOTH_S`] seconds)
/// rises above its maximum over the [`LOOK_S`] seconds before (as in SuperFlux, Böck & Widmer
/// 2013). Plain positive change of log energy reads any periodic ripple as onsets: measured, an
/// A-minor pad under a 126 BPM click read 157.5 BPM, because the 2.5 ms hops alias the ripple of a
/// sustained tone's energy (at twice its frequency) and the chord's difference tones modulate the
/// mid band at 40-110 Hz. A ripple never exceeds the maximum of its own last period; a click does.
fn flux(e: &[Vec<f32>; 3], fr: f64) -> Vec<f32> {
    let (smooth, look) = (
        ((SMOOTH_S * fr).round() as usize).max(1),
        ((LOOK_S * fr).round() as usize).max(1),
    );
    let lg = |v: f32| (1.0 + ENERGY_GAIN * v).ln();
    let s: Vec<Vec<f32>> = e
        .iter()
        .map(|b| {
            (0..b.len())
                .map(|i| {
                    lg(b[(i + 1).saturating_sub(smooth)..=i].iter().sum::<f32>() / smooth as f32)
                })
                .collect()
        })
        .collect();
    (0..e[0].len())
        .map(|i| {
            s.iter()
                .map(|b| {
                    (b[i]
                        - b[i.saturating_sub(look)..i]
                            .iter()
                            .fold(0.0, |m: f32, &v| m.max(v)))
                    .max(0.0)
                })
                .sum()
        })
        .collect()
}

// ponytail: a plain octave fold; a track whose true tempo is outside the range reads at the octave inside it. Upgrade:
// pick the octave by onset-pattern salience instead of folding.
/// `b` doubled or halved into `range` (BPM).
fn fold_bpm(mut b: f64, range: (f64, f64)) -> f64 {
    while b < range.0 {
        b *= 2.0;
    }
    while b > range.1 {
        b /= 2.0;
    }
    b
}

/// Onsets folded modulo `p` frames into 1-frame `bins`.
fn fold(onsets: &[(f64, f64)], p: f64, bins: &mut Vec<f64>) {
    bins.clear();
    bins.resize(p.ceil() as usize, 0.0);
    let (inv, last) = (1.0 / p, bins.len() - 1);
    for &(t, w) in onsets {
        bins[((t - (t * inv).floor() * p) as usize).min(last)] += w;
    }
}

/// Sum of the 3 bins centred on `j` (circular).
fn win3(bins: &[f64], j: usize) -> f64 {
    let nb = bins.len();
    bins[(j + nb - 1) % nb] + bins[j] + bins[(j + 1) % nb]
}

fn top3(bins: &[f64]) -> f64 {
    if bins.len() < 3 {
        return (0..bins.len()).map(|j| win3(bins, j)).fold(0.0, f64::max);
    }
    let n = bins.len();
    let edge = win3(bins, 0).max(win3(bins, n - 1));
    bins.windows(3)
        .map(|w| w[0] + w[1] + w[2])
        .fold(edge, f64::max)
}

/// Grid score of `onsets` at period `q` frames: best 3-bin window sum squared times `q` = (share of
/// all onset weight on the grid) x (mean weight per beat), so a half-tempo grid that hits only every
/// other onset scores half.
fn score(onsets: &[(f64, f64)], q: f64, bins: &mut Vec<f64>) -> f64 {
    fold(onsets, q, bins);
    let top = top3(bins);
    top * top * q
}

struct ScoreBound {
    remaining: Vec<f64>,
    divisor: f64,
}

impl ScoreBound {
    fn new(onsets: &[(f64, f64)]) -> Self {
        if onsets.len() < 512 {
            return Self {
                remaining: Vec::new(),
                divisor: 0.0,
            };
        }
        let mut remaining = vec![0.0; onsets.len() + 1];
        for (i, &(_, w)) in onsets.iter().enumerate().rev() {
            remaining[i] = (remaining[i + 1] + w).next_up();
        }
        // Nonnegative bin sums: this covers all accumulation and window-sum rounding,
        // including a partial window rounded down. Unbounded inputs disable pruning.
        let error = (onsets.len() as f64 + 4.0) * f64::EPSILON;
        let divisor = if error < 0.0625 {
            1.0 - 4.0 * error
        } else {
            0.0
        };
        Self { remaining, divisor }
    }

    fn score(&self, onsets: &[(f64, f64)], q: f64, bins: &mut Vec<f64>, best: f64) -> Option<f64> {
        let needed = (best / q).sqrt();
        if onsets.len() < 512
            || q <= 2.0
            || needed < self.remaining[0] * 0.25
            || self.divisor == 0.0
        {
            return Some(score(onsets, q, bins));
        }
        // Check once, after most of the incumbent's required weight is no longer
        // in the unvisited suffix. This changes work only, never the candidate set.
        let at = self.remaining.partition_point(|&w| w > needed * 0.75);
        fold(&onsets[..at], q, bins);
        let top = top3(bins);
        let upper = ((top + self.remaining[at]).next_up() / self.divisor).next_up();
        let upper_score = ((upper * upper).next_up() * q).next_up();
        if upper_score <= best {
            return None;
        }
        let (inv, last) = (1.0 / q, bins.len() - 1);
        for &(t, w) in &onsets[at..] {
            bins[((t - (t * inv).floor() * q) as usize).min(last)] += w;
        }
        let top = top3(bins);
        Some(top * top * q)
    }
}

/// Period in frames of the strongest tempo in `f` (onset flux; `onsets` are its peaks above the mean,
/// as `(frame, weight)`). Autocorrelation proposes candidates, [`score`] of a fine fold decides.
/// `None` when `f` is shorter than 4 beats of the slowest tempo.
fn tempo(
    f: &[f32],
    onsets: &[(f64, f64)],
    fr: f64,
    opt: &GridOptions,
    cancelled: &dyn Fn() -> bool,
) -> Option<f64> {
    let bound = ScoreBound::new(onsets);
    tempo_search(f, fr, opt, cancelled, |q, bins, best| {
        bound.score(onsets, q, bins, best)
    })
}

fn tempo_search(
    f: &[f32],
    fr: f64,
    opt: &GridOptions,
    cancelled: &dyn Fn() -> bool,
    mut evaluate: impl FnMut(f64, &mut Vec<f64>, f64) -> Option<f64>,
) -> Option<f64> {
    let n = f.len();
    let (search, range) = (
        opt.search(),
        (opt.bpm_range.0 as f64, opt.bpm_range.1 as f64),
    );
    let (lag_lo, lag_hi) = (
        (60.0 * fr / search.1).ceil() as usize,
        (60.0 * fr / search.0) as usize,
    );
    if n < lag_hi * 4 {
        return None;
    }
    let mean = f.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
    let fm: Vec<f64> = f.iter().map(|&v| v as f64 - mean).collect();
    let mut ac = Vec::with_capacity(lag_hi - lag_lo + 3);
    for l in lag_lo - 1..=lag_hi + 1 {
        if cancelled() {
            return None;
        }
        let sum = fm
            .iter()
            .zip(&fm[l..])
            .enumerate()
            .take_while(|(i, _)| i & 16383 != 0 || !cancelled())
            .map(|(_, (a, b))| a * b)
            .sum::<f64>();
        if cancelled() {
            return None;
        }
        ac.push(sum / (n - l) as f64);
    }
    let mut peaks: Vec<usize> = (1..ac.len() - 1)
        .filter(|&j| ac[j] > ac[j - 1] && ac[j] >= ac[j + 1])
        .collect();
    peaks.sort_by(|&a, &b| ac[b].total_cmp(&ac[a]));

    // Fine fold search ±0.6 % around each candidate (lag quantisation is ≤0.4 %; candidates that fold
    // to a tempo already searched are skipped). The step keeps the drift between neighbouring
    // candidates under 1.5 frames over the whole track.
    let (mut best, mut bins, mut tried) = ((0.0, 0.0), Vec::new(), Vec::<f64>::new());
    for j in peaks.into_iter().take(5) {
        let bpm0 = fold_bpm(60.0 * fr / (lag_lo - 1 + j) as f64, range);
        if tried.iter().any(|&t| (t / bpm0 - 1.0).abs() < 0.004) {
            continue;
        }
        tried.push(bpm0);
        let step = 1.5 / n as f64;
        let mut r = -0.006;
        while r <= 0.006 {
            if cancelled() {
                return None;
            }
            let p = 60.0 * fr / (bpm0 * (1.0 + r));
            if let Some(s) = evaluate(p, &mut bins, best.0) {
                if s > best.0 {
                    best = (s, p);
                }
            }
            r += step;
        }
    }
    (best.1 > 0.0).then_some(best.1)
}

/// Where in a beat the kick falls: the fraction (0..1) after each of `beats` (frame times, ascending)
/// at which the mid-band (200 Hz - 2.5 kHz) amplitude, averaged over all beats, rises most over
/// [`KICK_S`]. Hi-hats and off-beat bass give onsets as strong as the kick's but no such rise, so this
/// tells the beat from the off-beat. Measured on the test USB: within 2 frames of the stored grid on 24/24 tracks.
fn kick_offset(mid: &[f32], beats: &[f64], fr: f64) -> f64 {
    let nb = ((beats[beats.len() - 1] - beats[0]) / (beats.len() - 1) as f64).round() as usize;
    let (mut sum, mut cnt) = (vec![0f64; nb], vec![0f64; nb]);
    for w in beats.windows(2) {
        for i in w[0].ceil() as usize..(w[1].ceil() as usize).min(mid.len()) {
            let j = (((i as f64 - w[0]) / (w[1] - w[0]) * nb as f64) as usize).min(nb - 1);
            sum[j] += mid[i] as f64;
            cnt[j] += 1.0;
        }
    }
    let at = |j: isize| {
        sum[j.rem_euclid(nb as isize) as usize] / cnt[j.rem_euclid(nb as isize) as usize].max(1.0)
    };
    let w = (KICK_S * fr).round() as isize;
    let step = |j: isize| {
        let (ahead, behind): (f64, f64) = (
            (0..w).map(|o| at(j + o)).sum(),
            (1..=w).map(|o| at(j - o)).sum(),
        );
        ahead * (ahead - behind).max(0.0)
    };
    let kick = (0..nb as isize)
        .max_by(|&a, &b| step(a).total_cmp(&step(b)))
        .unwrap();
    (kick as f64 + 0.5) / nb as f64
}

/// Which of the 4 beats from the first is the bar's first: the one with the most low-band amplitude
/// in [`DOWNBEAT_S`] around it, each beat scaled by its bar's mean so loud passages do not dominate.
/// Measured on the test USB: agrees with the stored bar position on 21/24 tracks (strongest onsets: 11/24).
fn downbeat(low: &[f32], times: &[f64], fr: f64) -> usize {
    let [a, b] = DOWNBEAT_S.map(|s| (s * fr).round() as i64);
    let feat: Vec<f64> = times
        .iter()
        .map(|&t| {
            let t = t as i64;
            (t + a..t + b)
                .filter(|&j| j >= 0)
                .filter_map(|j| low.get(j as usize))
                .map(|&v| v as f64)
                .sum()
        })
        .collect();
    let mut strength = [0.0f64; 4];
    for bar in feat.chunks_exact(4) {
        let mean = bar.iter().sum::<f64>() / 4.0;
        if mean > 0.0 {
            for c in 0..4 {
                strength[c] += bar[c] / mean;
            }
        }
    }
    (0..4)
        .max_by(|&a, &b| strength[a].total_cmp(&strength[b]))
        .unwrap()
}

/// Weighted line fit onset = phi + k*p over the onsets within [`FIT_S`] of the grid `phase + k*p`:
/// refined `(p, phi)`.
fn fit(onsets: &[(f64, f64)], phase: f64, p: f64, fr: f64) -> (f64, f64) {
    let [mut sw, mut sk, mut sx, mut skk, mut skx] = [0.0; 5];
    for &(t, w) in onsets {
        let k = ((t - phase) / p).round();
        if (t - phase - k * p).abs() <= FIT_S * fr {
            [sw, sk, sx, skk, skx] = [
                sw + w,
                sk + w * k,
                sx + w * t,
                skk + w * k * k,
                skx + w * k * t,
            ];
        }
    }
    let den = sw * skk - sk * sk;
    if den > 1e-9 {
        let slope = (sw * skx - sk * sx) / den;
        if (slope / p - 1.0).abs() < 0.01 {
            return (slope, (sx - slope * sk) / sw);
        }
    }
    (p, phase)
}

/// Beats at `times` (frames): bar positions from [`downbeat`], tempo per beat from `bpm_x100`.
fn beats(times: &[f64], low: &[f32], fr: f64, bpm_x100: impl Fn(usize) -> u16) -> Vec<Beat> {
    let m = downbeat(low, times, fr) as i64;
    let at = |k: usize| Beat {
        bar_pos: ((k as i64 - m).rem_euclid(4) + 1) as u16,
        bpm_x100: bpm_x100(k),
        time_ms: (times[k] / fr * 1000.0).round() as u32,
    };
    (0..times.len()).map(at).collect()
}

/// Onsets of the flux `f`: its peaks above the mean as `(frame, weight)`, the frame refined to a fraction by a
/// parabola through the peak and its neighbours when `refine` (high precision).
fn onsets(f: &[f32], refine: bool) -> Vec<(f64, f64)> {
    let n = f.len();
    let mean = f.iter().map(|&v| v as f64).sum::<f64>() / n.max(1) as f64;
    (1..n.saturating_sub(1))
        .filter(|&i| f[i] > f[i - 1] && f[i] >= f[i + 1] && f[i] as f64 > mean)
        .map(|i| {
            let (a, b, c) = (f[i - 1] as f64, f[i] as f64, f[i + 1] as f64);
            let curve = a - 2.0 * b + c;
            let shift = if refine && curve < -1e-12 {
                (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            (i as f64 + shift, b)
        })
        .collect()
}

/// Beat grid over the whole track from the first beat at/after 0 to the end, `bar_pos` 1 on the
/// downbeat, with the default options (steady tempo, 85-175 BPM).
pub fn beat_grid(samples: &[f32], rate: u32) -> Vec<Beat> {
    beat_grid_with(samples, rate, &GridOptions::default())
}

/// [`beat_grid`] under `opt`: constant tempo (one `bpm_x100` throughout) unless the options track the beats
/// ([`GridOptions::tracks`]), then each tempo segment carries its own ([`dynamic::segments`]).
/// Empty when no tempo can be found (silence, too short).
pub fn beat_grid_with(samples: &[f32], rate: u32, opt: &GridOptions) -> Vec<Beat> {
    let (e, fr) = hops(samples, rate, opt.flux_hz());
    from_hops(e, fr, samples.len() / 2, rate, opt)
}

pub(super) fn from_hops(
    e: [Vec<f32>; 3],
    fr: f64,
    frames: usize,
    rate: u32,
    opt: &GridOptions,
) -> Vec<Beat> {
    from_hops_cancellable(e, fr, frames, rate, opt, &|| false).expect("uncancelled grid analysis")
}

pub(super) fn from_hops_cancellable(
    e: [Vec<f32>; 3],
    fr: f64,
    frames: usize,
    rate: u32,
    opt: &GridOptions,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<Beat>> {
    if cancelled() {
        return None;
    }
    let f = flux(&e, fr);
    let n = f.len();
    let onsets = onsets(&f, opt.precision);
    if cancelled() {
        return None;
    }
    if onsets.len() < 8 {
        return Some(Vec::new());
    }
    let period = tempo(&f, &onsets, fr, opt, cancelled);
    if cancelled() {
        return None;
    }
    let Some(p0) = period else {
        return Some(Vec::new());
    };
    let [low, mid, _] = e.map(|b| b.iter().map(|v| v.sqrt()).collect::<Vec<f32>>());
    let grid: Vec<f64> = (0..(n as f64 / p0) as usize)
        .map(|k| k as f64 * p0)
        .collect();
    let (p, phi) = fit(&onsets, kick_offset(&mid, &grid, fr) * p0, p0, fr);
    if cancelled() {
        return None;
    }

    if opt.effective_mode() != Mode::Normal {
        if let Some((times, bpm)) =
            dynamic::tracked(&f, &mid, &onsets, p, fr, |drifts| opt.tracks(drifts))
        {
            return (!cancelled()).then(|| beats(&times, &low, fr, |k| bpm[k]));
        }
    }
    if cancelled() {
        return None;
    }

    // Beats from the first at/after 0 (+0.5 frame: an onset is seen in the frame it starts in).
    let k0 = (-(phi + 0.5) / p).ceil() as i64;
    let frames = frames as f64 / rate as f64 * fr;
    let k_end = ((frames - phi - 0.5) / p).floor() as i64;
    let times: Vec<f64> = (k0..=k_end).map(|k| phi + 0.5 + k as f64 * p).collect();
    let bpm = (6000.0 * fr / p).round() as u16;
    Some(beats(&times, &low, fr, |_| bpm))
}
