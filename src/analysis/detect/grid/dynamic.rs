//! Tempo-varying grid (Track Analysis Mode Dynamic / Auto): tempo per 8 s window ([`windows`]), beats tracked
//! through those tempi ([`track`]), the beats merged into segments that each carry one tempo ([`segments`]).
//! Verified on synthetic click tracks only.

use super::{TIGHT, WINDOW_S, kick_offset, score};

/// A beat may sit this far (seconds) from the regular grid of its tempo segment.
const ERR_S: f64 = 0.010;
/// Segments shorter than this many beats (two bars) are absorbed by their neighbour.
const MIN_BEATS: usize = 8;
/// A segment boundary is searched this many beats either side of where the greedy growth cut.
const BOUNDARY_BEATS: usize = 6;

/// Tempo over time: for windows of [`WINDOW_S`] every half window, `[centre frame, best period within
/// -12..+14 % of p, its score, score of p itself]`.
fn windows(onsets: &[(f64, f64)], p: f64, n: usize, fr: f64) -> Vec<[f64; 4]> {
    let w = WINDOW_S * fr;
    let (mut out, mut bins, mut a) = (Vec::new(), Vec::new(), 0.0);
    while a + w <= n as f64 {
        let on =
            &onsets[onsets.partition_point(|o| o.0 < a)..onsets.partition_point(|o| o.0 < a + w)];
        let (mut best, mut r) = ((0.0, p), -0.12);
        while r <= 0.14 {
            let q = p * (1.0 + r);
            let s = score(on, q, &mut bins);
            if s > best.0 {
                best = (s, q);
            }
            r += 3.0 / w;
        }
        out.push([a + w / 2.0, best.1, best.0, score(on, p, &mut bins)]);
        a += w / 2.0;
    }
    out
}

/// The windows that are periodic at all (best score at least half the median): `(centre frame, best period)` of each,
/// and how many of them the constant period fits badly (it scores under half of the window's own best). `None`
/// without windows (a track shorter than one).
fn periodic(w: &[[f64; 4]]) -> Option<(Vec<(f64, f64)>, usize)> {
    let mut s: Vec<f64> = w.iter().map(|x| x[2]).collect();
    s.sort_by(|a, b| a.total_cmp(b));
    let floor = 0.5 * s.get(s.len() / 2)?;
    let periodic: Vec<&[f64; 4]> = w.iter().filter(|x| x[2] >= floor).collect();
    let off = periodic.iter().filter(|x| x[3] < 0.5 * x[2]).count();
    Some((periodic.iter().map(|x| (x[0], x[1])).collect(), off))
}

/// The tempo drifts: at least a quarter (and 2) of the `points` periodic windows are `off` the constant period.
pub(super) fn drifts(points: usize, off: usize) -> bool {
    off >= 2 && off * 4 >= points
}

/// Beat frames of a drifting tempo: the chain of flux peaks that maximises the summed flux minus
/// [`TIGHT`] times the squared relative deviation of each interval from the local period (linear
/// between the `(frame, period)` points `pts`), by dynamic programming (the beat tracker of Ellis,
/// 2007). Intervals are limited to 0.75-1.35 periods.
fn track(f: &[f32], pts: &[(f64, f64)]) -> Vec<f64> {
    let period = |t: f64| {
        let i = pts.partition_point(|x| x.0 < t);
        match (i.checked_sub(1).map(|j| pts[j]), pts.get(i).copied()) {
            (Some(a), Some(b)) => a.1 + (b.1 - a.1) * (t - a.0) / (b.0 - a.0),
            (Some(x), None) | (None, Some(x)) => x.1,
            (None, None) => unreachable!("periodic() returns windows"),
        }
    };
    let n = f.len();
    let rms = (f.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / n as f64)
        .sqrt()
        .max(1e-9);
    let (mut best, mut from) = (vec![0.0f64; n], vec![usize::MAX; n]);
    for i in 0..n {
        let q = period(i as f64);
        let (mut top, mut at) = (0.0, usize::MAX);
        for d in ((0.75 * q) as usize).max(1)..=(1.35 * q) as usize {
            if d > i {
                break;
            }
            let r = (d as f64 - q) / q;
            let s = best[i - d] - TIGHT * r * r;
            if s > top {
                (top, at) = (s, i - d);
            }
        }
        (best[i], from[i]) = (f[i] as f64 / rms + top, at);
    }
    let tail = n.saturating_sub((1.35 * period(n as f64)) as usize);
    let mut i = (tail..n)
        .max_by(|&a, &b| best[a].total_cmp(&best[b]))
        .unwrap();
    let mut out = vec![i as f64];
    while from[i] != usize::MAX {
        i = from[i];
        out.push(i as f64);
    }
    out.reverse();
    out
}

/// Running sums of `k`, `k²`, `t`, `k·t`, `t²` over the beats, so the least-squares line of any run of beats is O(1).
struct Sums(Vec<[f64; 5]>);

impl Sums {
    /// `times` relative to the first beat (keeps the sums small).
    fn new(times: &[f64]) -> Self {
        let mut acc = vec![[0.0; 5]];
        for (k, &t) in times.iter().enumerate() {
            let (k, t) = (k as f64, t - times[0]);
            let last = acc[acc.len() - 1];
            acc.push([
                last[0] + k,
                last[1] + k * k,
                last[2] + t,
                last[3] + k * t,
                last[4] + t * t,
            ]);
        }
        Sums(acc)
    }

    fn range(&self, a: usize, b: usize) -> [f64; 5] {
        std::array::from_fn(|i| self.0[b][i] - self.0[a][i])
    }

    /// Slope and intercept (relative to the first beat's time) of the line `t = c + s·k` through beats `a..b`, and its squared error.
    fn fit(&self, a: usize, b: usize) -> (f64, f64, f64) {
        let [sk, skk, st, skt, stt] = self.range(a, b);
        let n = (b - a) as f64;
        let den = n * skk - sk * sk;
        if den < 1e-9 {
            return (0.0, st / n, 0.0);
        }
        let s = (n * skt - sk * st) / den;
        let c = (st - s * sk) / n;
        (s, c, (stt - c * st - s * skt).max(0.0))
    }
}

/// `times` (beat frames, at least 2) as segments of one tempo each, every beat within [`ERR_S`] of its segment's
/// regular grid: segments grow from the left while the least-squares line through their beats stays that close
/// (piecewise-linear segmentation); segments under [`MIN_BEATS`] join their neighbour with the closer tempo, neighbours
/// one line fits join each other; each boundary then moves to the beat (within [`BOUNDARY_BEATS`]) where the two lines
/// fit best, which also fixes the late cut the greedy growth makes at a tempo step. Returns every beat's time on its
/// segment's regular grid and the segment's `bpm_x100`: a steady track is one segment, one regular grid.
pub(super) fn segments(times: &[f64], fr: f64) -> (Vec<f64>, Vec<u16>) {
    let n = times.len();
    let sums = Sums::new(times);
    let err = ERR_S * fr;
    let worst = |a: usize, b: usize| {
        let (s, c, _) = sums.fit(a, b);
        (a..b)
            .map(|k| (times[0] + c + s * k as f64 - times[k]).abs())
            .fold(0.0, f64::max)
    };
    let mut seg: Vec<(usize, usize)> = Vec::new();
    let mut a = 0;
    while a < n {
        let mut e = (a + 2).min(n);
        while e < n && worst(a, e + 1) <= err {
            e += 1;
        }
        seg.push((a, e));
        a = e;
    }

    loop {
        let short = (0..seg.len())
            .filter(|&i| seg.len() > 1 && seg[i].1 - seg[i].0 < MIN_BEATS)
            .min_by_key(|&i| seg[i].1 - seg[i].0);
        let i = match short {
            Some(i) => {
                let slope = |j: usize| sums.fit(seg[j].0, seg[j].1).0;
                let gap = |j: usize| (slope(j) - slope(i)).abs();
                if i == 0 {
                    0
                } else if i + 1 == seg.len() || gap(i - 1) <= gap(i + 1) {
                    i - 1
                } else {
                    i
                }
            }
            None => match (0..seg.len().saturating_sub(1))
                .find(|&i| worst(seg[i].0, seg[i + 1].1) <= err)
            {
                Some(i) => i,
                None => break,
            },
        };
        // `i` and `i + 1` become one segment.
        seg[i].1 = seg[i + 1].1;
        seg.remove(i + 1);
    }

    for i in 0..seg.len().saturating_sub(1) {
        let (a, b, e) = (seg[i].0, seg[i].1, seg[i + 1].1);
        let (lo, hi) = (
            b.saturating_sub(BOUNDARY_BEATS).max(a + MIN_BEATS),
            (b + BOUNDARY_BEATS).min(e.saturating_sub(MIN_BEATS)),
        );
        let cost = |c: usize| sums.fit(a, c).2 + sums.fit(c, e).2;
        if let Some(c) = (lo..=hi).min_by(|&x, &y| cost(x).total_cmp(&cost(y))) {
            seg[i].1 = c;
            seg[i + 1].0 = c;
        }
    }

    let (mut out, mut bpm) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for &(a, b) in &seg {
        let (s, c, _) = sums.fit(a, b);
        let tempo = if s > 0.0 {
            (6000.0 * fr / s).round().min(u16::MAX as f64) as u16
        } else {
            0
        };
        out.extend((a..b).map(|k| times[0] + c + s * k as f64));
        bpm.extend((a..b).map(|_| tempo));
    }
    (out, bpm)
}

/// The beats of a track whose tempo changes, with each beat's `bpm_x100`: `None` when the window tempi say the
/// track is steady and `wanted(drifts)` declines (Auto), when the track is shorter than one window or has fewer
/// than 8 beats. `p` is the constant period (frames) of the whole track, `f` the onset flux, `mid` the mid-band
/// amplitude (for the kick phase) and `onsets` as in [`super::beat_grid_with`].
pub(super) fn tracked(
    f: &[f32],
    mid: &[f32],
    onsets: &[(f64, f64)],
    p: f64,
    fr: f64,
    wanted: impl Fn(bool) -> bool,
) -> Option<(Vec<f64>, Vec<u16>)> {
    let (pts, off) = periodic(&windows(onsets, p, f.len(), fr))?;
    if !wanted(drifts(pts.len(), off)) {
        return None;
    }
    let b = track(f, &pts);
    if b.len() < MIN_BEATS {
        return None;
    }
    // The chain may sit on the off-beat; a grossly shifted kick is moved to the beat.
    let u = kick_offset(mid, &b, fr);
    let s = if u > 0.5 { u - 1.0 } else { u };
    let gap = |k: usize| {
        if k + 1 < b.len() && s >= 0.0 {
            b[k + 1] - b[k]
        } else if k > 0 {
            b[k] - b[k - 1]
        } else {
            b[1] - b[0]
        }
    };
    // +0.5 frame: an onset is seen in the frame it starts in.
    let times: Vec<f64> = (0..b.len())
        .map(|k| b[k] + 0.5 + if s.abs() > 0.15 { s * gap(k) } else { 0.0 })
        .filter(|&t| t >= 0.0)
        .collect();
    (times.len() >= MIN_BEATS).then(|| segments(&times, fr))
}
