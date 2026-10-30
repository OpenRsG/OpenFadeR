//! Beat grid from decoded PCM (stereo f32, see [`super`]). The design is ours.
//!
//! Tempo: onset flux at 400 Hz (1000 Hz with [`GridOptions::precision`]; band-split, [`flux`]);
//! autocorrelation proposes candidates and a fold of the onsets modulo each fine-stepped period scores
//! them ([`tempo`]). Searched around the BPM range of [`GridOptions`] (default 85-175: searched 70-180) and
//! folded into that range ([`fold_bpm`]).
//!
//! Phase: the beat is where the low band (below 200 Hz) amplitude rises (the kick), not where
//! the onsets are strongest ([`kick_offset`]). A weighted line fit through the onsets near that grid
//! refines tempo and phase ([`fit`]). Bar positions: beat 1 is the phase of four with the best sum of
//! three pieces of evidence: low-band accent, spectral change from the previous beat, and the start of
//! the music ([`downbeat`], [`phase_scores`]).
//!
//! Tempo changes ([`GridOptions::mode`]; [`dynamic`]): tempo is also estimated per 8 s window. Auto (the default) tracks the
//! beats and merges them into sections of one tempo each when a constant grid does not fit a quarter of the periodic windows;
//! a steady track keeps one constant grid. Dynamic always tracks. Normal never does: one steady tempo.
//!
//! Measured on the 24-track test USB (tests/detect_usb.rs) with the mid-band kick phase used before: tempo equals the stored
//! library grid's in 24/24 and every grid is constant, like the stored ones. The first beat landed within 3 ms of a stored
//! beat in 24/24 (a strongest-onset phase: 18/24 within 30 ms, 4 of the other 6 half a beat off). Our bar position agreed
//! with the stored one on 21/24 with the accent alone (strongest onsets: 11/24); the windows never flag drift (worst
//! window 0.59 of its own best score, flagged below 0.5). The low-band kick phase and the bar evidence are not re-measured
//! there.
//!
//! Bar position on the Ballroom dataset (ISMIR 2004 audio, 30 s clips; downbeats from CPJKU/BallroomAnnotations; 4/4
//! genres, the 297 clips whose detected beats match the annotated ones within 70 ms; `measure_bar_phase_on_annotated_set`):
//! the accent alone 179/297 = 60.3%, with the two other pieces 192/297 = 64.6%. The two weights were chosen on these
//! clips (with the mid-band phase); chosen on the other folds instead, held-out accuracy is 191/297 = 64.3% (2-fold) and
//! 190/297 = 64.0% (5-fold). Against the accent alone they fix 33 clips and break 20 (sign test p = 0.10). Full tracks
//! with drops and breakdowns: 5 NCS releases at 128 BPM (Candyland, Heroes Tonight, Nekozilla, Sky High, Spectre), the
//! reference being Beat This! (final0) beats and downbeats: beats within 70 ms on 97-100%, bar position agreeing on
//! 96-100% of the matched beats in each track (the accent alone: 0% on Heroes Tonight, the others as now). A 174 BPM
//! drum & bass track the reference counts at 87 BPM is left out. ponytail: few real tracks set the bands and the
//! thresholds; other genres may need other bands. Drift tracking is verified on synthetic click tracks only (no real
//! variable-tempo audio available).
//!
//! Octave: the preferences' BPM ranges each span an octave, so the range alone picks the reading ([`fold_bpm`]); where a
//! range leaves two (the overlap at its edges, or a wider custom one) [`octave`] takes the one the onsets support, with a
//! tempo prior around 125 BPM as the tie-break.
//!
//! Meter: detection always numbers bars in fours; a 3/4 grid comes from other software, the grid editor or an import (the
//! `bar_pos` of a beat carries it) and everything downstream that counts bars follows the grid's own length. shortcut: triple
//! meter is not detected. The bar evidence above is a 64.6% pick between 4 phases on 4/4 music (Ballroom, see above), so an
//! accent-period test of 3 against 4 beats would rest on features that are right about two times in three at 4/4 and
//! would turn a missed accent into a 3/4 grid on a 4/4 track, the costlier error (bars, phrases, loops and automix all
//! follow it). No waltz audio with downbeats was available to measure a false-positive rate; add the test with that data.

mod dynamic;
#[cfg(test)]
mod tests;

use super::{FLUX_HZ, PRECISE_FLUX_HZ, hops};
use crate::analysis::Beat;
use crate::audio::Frames;

/// Whether and when the beats are tracked into tempo sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// One steady tempo and a fixed grid.
    Normal,
    /// Tempo-varying grid: segments of one tempo each.
    Dynamic,
    /// Dynamic when the tempo drifts or changes, else Normal.
    #[default]
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
            mode: Mode::Auto,
            precision: false,
            bpm_range: (85, 175),
        }
    }
}

impl GridOptions {
    /// Whether the beats are tracked into tempo segments, given whether the windows show drift ([`dynamic::drifts`]).
    fn tracks(&self, drifts: bool) -> bool {
        match self.mode {
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
/// Bar phase ([`downbeat`]): weights of the novelty and start evidence against the accent, the quietest beat that
/// still counts as music (share of the median beat's energy), the latest beat (from the grid's start) it may start on,
/// and the floor of the novelty log ratio (share of the band's mean).
const NOVELTY_W: f64 = 0.5;
const START_W: f64 = 0.15;
const START_LEVEL: f64 = 0.5;
const START_MAX: usize = 16;
const NOVELTY_FLOOR: f64 = 0.05;
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
    let n = e[0].len();
    let mut out = vec![0f32; n];
    // Per band: a running window sum for the smoothing, and the maximum of the smoothed log energy over the `look`
    // hops before as a monotonic deque of (hop, value): O(n) instead of a window scan per hop.
    let mut deque: std::collections::VecDeque<(usize, f32)> = std::collections::VecDeque::new();
    for b in e {
        deque.clear();
        let mut sum = 0.0f32;
        for i in 0..n {
            sum += b[i];
            if i >= smooth {
                sum -= b[i - smooth];
            }
            let s = lg(sum.max(0.0) / smooth as f32);
            let floor = i.saturating_sub(look);
            while deque.front().is_some_and(|&(j, _)| j < floor) {
                deque.pop_front();
            }
            let max = deque.front().map_or(0.0, |&(_, v)| v);
            out[i] += (s - max).max(0.0);
            while deque.back().is_some_and(|&(_, v)| v <= s) {
                deque.pop_back();
            }
            deque.push_back((i, s));
        }
    }
    out
}

/// `b` doubled or halved into `range` (BPM). The range is the user's promise of which reading they want ("Every range spans
/// an octave", [`GridOptions::bpm_range`]), so a track whose true tempo is outside it reads at the octave inside it; only
/// [`octave`] chooses between two readings that both fit.
fn fold_bpm(mut b: f64, range: (f64, f64)) -> f64 {
    while b < range.0 {
        b *= 2.0;
    }
    while b > range.1 {
        b /= 2.0;
    }
    b
}

/// Centre and width (octaves, one standard deviation) of the tempo prior of [`octave`]: log-normal around the 120-130 BPM
/// band most dance and pop music sits in, wide enough that 87 and 174 BPM weigh about the same (0.81 and 0.84) and the onsets
/// decide there.
const PRIOR_BPM: f64 = 125.0;
const PRIOR_OCTAVES: f64 = 0.8;

/// How likely a tempo is before listening: 1.0 at [`PRIOR_BPM`], falling off per octave.
fn prior(bpm: f64) -> f64 {
    let d = (bpm / PRIOR_BPM).log2() / PRIOR_OCTAVES;
    (-0.5 * d * d).exp()
}

/// The octave reading of period `p` frames that the onset pattern supports best: `p` itself, half of it or twice it, the two
/// others only when their tempo is inside `range` too, by [`score`] times [`prior`]. [`score`] is already the salience: a
/// reading that skips onsets (half time over a steady beat) keeps half of them on its grid, one that adds beats between
/// the real ones (double time over a plain kick) puts the same weight on twice as many beats, and only a pattern
/// whose off-beats are about as strong as its beats (8th-note hats, a ghost-kick groove) wins by doubling. `p` wins ties.
/// Only a range wider than an octave leaves a choice; the preferences' ranges (70-140 to 110-220) do so in their overlap
/// of a few BPM at the edges (85-87.5 against 170-175).
fn octave(onsets: &[(f64, f64)], p: f64, fr: f64, range: (f64, f64)) -> f64 {
    let mut bins = Vec::new();
    let mut at = |q: f64| score(onsets, q, &mut bins) * prior(60.0 * fr / q);
    let mut best = (at(p), p);
    for q in [p / 2.0, p * 2.0] {
        if (range.0..=range.1).contains(&(60.0 * fr / q)) {
            let s = at(q);
            if s > best.0 {
                best = (s, q);
            }
        }
    }
    fold(onsets, best.1, &mut bins);
    let q = best.1 * 2.0 / 3.0;
    if dotted(&bins) && (range.0..=range.1).contains(&(60.0 * fr / q)) {
        return q;
    }
    best.1
}

/// Whether a fold has its off-beats on the thirds of the period and none on its half: a kick every one and a half beats
/// read as the beat (the dotted bass pattern of much tech house, issue 43). The hats and claps on the real beat's halves
/// land on the thirds of that reading, where a straight track at this tempo has nothing, and its half is empty; [`score`]
/// cannot tell, as it rewards the longer period that carries every kick. A shuffle keeps weight on the half.
fn dotted(bins: &[f64]) -> bool {
    let n = bins.len();
    if n < 12 {
        return false;
    }
    let top = (0..n)
        .max_by(|&a, &b| win3(bins, a).total_cmp(&win3(bins, b)))
        .unwrap();
    let at = |f: f64| win3(bins, (top + (n as f64 * f).round() as usize) % n);
    let (half, thirds) = (at(0.5), at(1.0 / 3.0) + at(2.0 / 3.0));
    thirds > 0.25 * win3(bins, top) && half < 0.15 * thirds
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
    // The autocorrelation only proposes candidates (the fine fold below refines them by ±0.6 %), so it runs on the flux
    // summed down to at most 500 Hz: 1000 Hz hops would cost 2.5× the lags over 2.5× the hops for the same candidates.
    let d = ((fr / 500.0).ceil() as usize).max(1);
    let (fr_full, n_full) = (fr, n);
    let (fr, n) = (fr / d as f64, n / d);
    let (lag_lo, lag_hi) = (
        (60.0 * fr / search.1).ceil() as usize,
        (60.0 * fr / search.0) as usize,
    );
    if n < lag_hi * 4 {
        return None;
    }
    let sum_d = |i: usize| f[i * d..(i + 1) * d].iter().map(|&v| v as f64).sum::<f64>();
    let mean = (0..n).map(sum_d).sum::<f64>() / n as f64;
    let fm: Vec<f64> = (0..n).map(|i| sum_d(i) - mean).collect();
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

    // Fine fold search ±0.6 % around each candidate (lag quantisation is ≤0.6 % at 500 Hz; candidates that fold
    // to a tempo already searched are skipped). The step keeps the drift between neighbouring
    // candidates under 1.5 frames over the whole track.
    let (mut best, mut bins, mut tried) = ((0.0, 0.0), Vec::new(), Vec::<f64>::new());
    for j in peaks.into_iter().take(5) {
        let bpm0 = fold_bpm(60.0 * fr / (lag_lo - 1 + j) as f64, range);
        if tried.iter().any(|&t| (t / bpm0 - 1.0).abs() < 0.004) {
            continue;
        }
        tried.push(bpm0);
        let step = 1.5 / n_full as f64;
        let mut r = -0.006;
        while r <= 0.006 {
            if cancelled() {
                return None;
            }
            let p = 60.0 * fr_full / (bpm0 * (1.0 + r));
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
/// at which the low-band (below 200 Hz) amplitude `low`, averaged over all beats, rises most over
/// [`KICK_S`]. Hi-hats give onsets as strong as the kick's but no such rise, so this tells the beat from the
/// off-beat. The mid band (200 Hz - 2.5 kHz, used before) put the beat on the off-beat in sidechain-pumped dance
/// tracks, where pads and bass swell back up between kicks: on 5 full-length 128 BPM NCS tracks, beats matched a
/// reference tracker (Beat This!, within 70 ms) on 1/5 with the mid band and 5/5 (97-100% of beats) with the low band;
/// on Ballroom 297 clips matched instead of 287. The mid band was within 2 frames of the test USB's stored grid on
/// 24/24 tracks; the low band is not re-measured there.
fn kick_offset(low: &[f32], beats: &[f64], fr: f64) -> f64 {
    let nb = ((beats[beats.len() - 1] - beats[0]) / (beats.len() - 1) as f64).round() as usize;
    let (mut sum, mut cnt) = (vec![0f64; nb], vec![0f64; nb]);
    for w in beats.windows(2) {
        for i in w[0].ceil() as usize..(w[1].ceil() as usize).min(low.len()) {
            let j = (((i as f64 - w[0]) / (w[1] - w[0]) * nb as f64) as usize).min(nb - 1);
            sum[j] += low[i] as f64;
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

/// Mean amplitude of each band `[low, mid, high]` over each interval between consecutive `times` (frames).
fn beat_bands(amp: &[Vec<f32>; 3], times: &[f64]) -> Vec<[f64; 3]> {
    times
        .windows(2)
        .map(|w| {
            std::array::from_fn(|b| {
                let n = amp[b].len();
                let seg = &amp[b][(w[0].max(0.0) as usize).min(n)..(w[1].max(0.0) as usize).min(n)];
                seg.iter().map(|&v| v as f64).sum::<f64>() / seg.len().max(1) as f64
            })
        })
        .collect()
}

/// Per bar position the share of `of` (one value per beat) that falls on it, 1.0 when all four are equal.
fn share(of: impl Iterator<Item = f64>) -> [f64; 4] {
    let mut s = [0.0f64; 4];
    for (k, v) in of.enumerate() {
        s[k % 4] += v;
    }
    let total: f64 = s.iter().sum();
    if total > 0.0 {
        s.map(|v| 4.0 * v / total)
    } else {
        [1.0; 4]
    }
}

/// Evidence for each of the 4 phases being the bar's first beat, each on a scale where "no evidence" is 1.0:
/// `[accent, novelty, start]`.
/// - accent: low-band amplitude in [`DOWNBEAT_S`] around the beat, each beat scaled by its bar's mean so loud
///   passages do not dominate, averaged over the bars.
/// - novelty: how much the band levels change from the previous beat to this one (`|ln this/previous|` summed over the
///   3 bands, [`NOVELTY_FLOOR`] of the band's mean keeping a quiet band from dominating), as the share of all change that
///   lands on this phase (a beat-synchronous spectral difference, Davies & Plumbley 2006): a new chord, drum pattern or
///   section starts on bar 1 even when the kick does not accent it.
/// - start: 2.0 for the phase of the first beat that carries music ([`START_LEVEL`] of the median beat), when that is
///   within [`START_MAX`] beats of the grid's start: dance tracks begin on a bar's first beat. 1.0 elsewhere.
fn phase_scores(amp: &[Vec<f32>; 3], times: &[f64], fr: f64) -> [[f64; 4]; 3] {
    let [a, b] = DOWNBEAT_S.map(|s| (s * fr).round() as i64);
    let low = &amp[0];
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
    let (mut accent, mut bars) = ([0.0f64; 4], 0.0);
    for bar in feat.chunks_exact(4) {
        let mean = bar.iter().sum::<f64>() / 4.0;
        if mean > 0.0 {
            for c in 0..4 {
                accent[c] += bar[c] / mean;
            }
            bars += 1.0;
        }
    }
    let accent = if bars > 0.0 {
        accent.map(|v| v / bars)
    } else {
        [1.0; 4]
    };

    let bands = beat_bands(amp, times);
    let floor: [f64; 3] = std::array::from_fn(|i| {
        NOVELTY_FLOOR * bands.iter().map(|b| b[i]).sum::<f64>() / bands.len().max(1) as f64
    });
    let novelty = share((0..times.len()).map(|j| {
        if j == 0 || j >= bands.len() {
            return 0.0;
        }
        (0..3)
            .map(|i| {
                ((bands[j][i] + floor[i]) / (bands[j - 1][i] + floor[i]))
                    .ln()
                    .abs()
            })
            .sum()
    }));

    let beat_energy: Vec<f64> = bands.iter().map(|b| b.iter().sum()).collect();
    let mut sorted = beat_energy.clone();
    sorted.sort_by(f64::total_cmp);
    let heard = START_LEVEL * sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let mut start = [1.0; 4];
    if let Some(k) = beat_energy
        .iter()
        .position(|&e| e >= heard)
        .filter(|&k| k <= START_MAX)
    {
        start[k % 4] = 2.0;
    }
    [accent, novelty, start]
}

/// Which of the 4 beats from the first is the bar's first: the phase with the highest sum of [`phase_scores`]
/// (accent + [`NOVELTY_W`] novelty + [`START_W`] start). Measured: see the module doc.
fn downbeat(amp: &[Vec<f32>; 3], times: &[f64], fr: f64) -> usize {
    let [accent, novelty, start] = phase_scores(amp, times, fr);
    (0..4)
        .max_by(|&a, &b| {
            let s = |c: usize| accent[c] + NOVELTY_W * novelty[c] + START_W * start[c];
            s(a).total_cmp(&s(b))
        })
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

/// The beat sits where the kick starts, not where it peaks: window around each beat in which its rise is looked for
/// (seconds), the share of the window's rise (min to peak of the low band) that marks the start, and the share of the median
/// beat's energy under which a leading beat is silence.
const KICK_LEAD_S: f64 = 0.04;
const KICK_RISE: f32 = 0.1;
const SILENT: f64 = 0.02;

/// How far (frames, usually negative) the start of the kick's rise sits from the beats: per beat, the low band's peak
/// within [`KICK_LEAD_S`] walked back to where it first rose above [`KICK_RISE`] of the way from the window's minimum before
/// the peak, median over the beats with a clear rise (peak at least 3× that minimum). 0 when fewer than 8 beats have one.
/// The onset flux the phase is fitted to peaks where the energy rises fastest, 15-25 ms into a real kick (issue 43).
fn kick_lead(low: &[f32], times: &[f64], fr: f64) -> f64 {
    let w = (KICK_LEAD_S * fr).round() as i64;
    let mut leads: Vec<f64> = times
        .iter()
        .filter_map(|&t| {
            let t = t.round() as i64;
            let lo = (t - w).max(0) as usize;
            let hi = ((t + w).max(0) as usize).min(low.len());
            let seg = low.get(lo..hi).filter(|s| s.len() > 1)?;
            let peak = (0..seg.len()).max_by(|&a, &b| seg[a].total_cmp(&seg[b]))?;
            let min = seg[..=peak].iter().copied().fold(f32::MAX, f32::min);
            if seg[peak] <= 0.0 || seg[peak] < 3.0 * min {
                return None;
            }
            let level = min + KICK_RISE * (seg[peak] - min);
            let mut k = peak;
            while k > 0 && seg[k - 1] >= level {
                k -= 1;
            }
            Some((lo + k) as f64 - t as f64)
        })
        .collect();
    if leads.len() < 8 {
        return 0.0;
    }
    leads.sort_by(f64::total_cmp);
    leads[leads.len() / 2]
}

/// Beats at `times` (frames), moved to the start of the kick ([`kick_lead`]) and starting at the first beat that carries
/// sound (a beat whose energy is under [`SILENT`] of the median beat's is the silence before the music, not a beat);
/// bar positions from [`downbeat`] of the band amplitudes `amp`, tempo per beat from `bpm_x100` (indexed as `times`).
fn beats(
    times: &[f64],
    amp: &[Vec<f32>; 3],
    fr: f64,
    bpm_x100: impl Fn(usize) -> u16,
) -> Vec<Beat> {
    let lead = kick_lead(&amp[0], times, fr);
    let times: Vec<f64> = times.iter().map(|t| (t + lead).max(0.0)).collect();
    let energy: Vec<f64> = beat_bands(amp, &times)
        .iter()
        .map(|b| b.iter().sum())
        .collect();
    let mut sorted = energy.clone();
    sorted.sort_by(f64::total_cmp);
    let silent = SILENT * sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let first = energy.iter().position(|&e| e > silent).unwrap_or(0);
    let times = &times[first..];
    let m = downbeat(amp, times, fr) as i64;
    let at = |k: usize| Beat {
        bar_pos: ((k as i64 - m).rem_euclid(4) + 1) as u16,
        bpm_x100: bpm_x100(k + first),
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
pub fn beat_grid<S: Frames + ?Sized>(samples: &S, rate: u32) -> Vec<Beat> {
    beat_grid_with(samples, rate, &GridOptions::default())
}

/// [`beat_grid`] under `opt`: constant tempo (one `bpm_x100` throughout) unless the options track the beats
/// ([`GridOptions::tracks`]), then each tempo segment carries its own ([`dynamic::segments`]).
/// Empty when no tempo can be found (silence, too short).
pub fn beat_grid_with<S: Frames + ?Sized>(samples: &S, rate: u32, opt: &GridOptions) -> Vec<Beat> {
    let (e, fr) = hops(samples, rate, opt.flux_hz());
    from_hops(e, fr, samples.frame_count(), rate, opt)
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
    let p0 = octave(&onsets, p0, fr, (opt.bpm_range.0 as f64, opt.bpm_range.1 as f64));
    let amp = e.map(|b| b.iter().map(|v| v.sqrt()).collect::<Vec<f32>>());
    let low = &amp[0];
    let grid: Vec<f64> = (0..(n as f64 / p0) as usize)
        .map(|k| k as f64 * p0)
        .collect();
    let (p, phi) = fit(&onsets, kick_offset(low, &grid, fr) * p0, p0, fr);
    if cancelled() {
        return None;
    }

    if opt.mode != Mode::Normal {
        if let Some((times, bpm)) =
            dynamic::tracked(&f, low, &onsets, p, fr, |drifts| opt.tracks(drifts))
        {
            return (!cancelled()).then(|| beats(&times, &amp, fr, |k| bpm[k]));
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
    Some(beats(&times, &amp, fr, |_| bpm))
}
