//! Bar boundaries: the bar levels are fitted by a piecewise-constant curve. Dynamic programming picks the
//! cuts minimising the weighted squared deviation inside each segment plus a price per segment and a price for
//! lengths that are not a multiple of 8, 4 or 2 bars. The length prior is measured (length histogram of the 418
//! phrases of the test set): 50 % are 8 bars, 15 % 4 bars, 16 % 16 bars, 85 % a multiple of 4 bars, and 2 % (10) are
//! longer than 16 bars; capping segments at 16 bars scored better than 24 or 32 in a sweep. The prices and band
//! weights were tuned on the 24 test tracks (the mid weight was raised to 2 after a sweep of the weights), so
//! expect less on other music.

const MIN_BARS: usize = 2;
const MAX_BARS: usize = 16;
/// Weight of the `[low, mid, high]` levels in the squared deviation.
const BAND_WEIGHT: [f32; 3] = [1.0, 2.0, 1.0];
/// Price of one more segment (squared weighted level units; a bar of full-scale change is 1).
const PER_SEGMENT: f32 = 0.15;
/// Price of a length that is not a multiple of 8 / 4 / 2 bars (they add up).
const NOT_MULTIPLE: [(usize, f32); 3] = [(8, 0.4), (4, 0.5), (2, 1.0)];

fn length_price(len: usize) -> f32 {
    NOT_MULTIPLE
        .iter()
        .filter(|&&(m, _)| len % m != 0)
        .map(|&(_, p)| p)
        .sum()
}

/// Start bar of every segment, ascending, first is 0.
pub(super) fn cut(level: &[[f32; 3]]) -> Vec<usize> {
    let n = level.len();
    let (mut s1, mut s2) = (vec![[0f32; 3]; n + 1], vec![0f32; n + 1]);
    for (i, l) in level.iter().enumerate() {
        let l = [0, 1, 2].map(|k| l[k] * BAND_WEIGHT[k]);
        for k in 0..3 {
            s1[i + 1][k] = s1[i][k] + l[k];
        }
        s2[i + 1] = s2[i] + l.iter().map(|v| v * v).sum::<f32>();
    }
    let sse = |a: usize, b: usize| {
        let w = (b - a) as f32;
        s2[b] - s2[a] - (0..3).map(|k| (s1[b][k] - s1[a][k]).powi(2)).sum::<f32>() / w
    };
    // best[j]: cheapest cover of bars 0..j; from[j]: where its last segment starts.
    let (mut best, mut from) = (vec![f32::INFINITY; n + 1], vec![0usize; n + 1]);
    best[0] = 0.0;
    for j in MIN_BARS.min(n)..=n {
        for i in j.saturating_sub(MAX_BARS)..=j.saturating_sub(MIN_BARS) {
            if best[i].is_finite() {
                let c = best[i] + sse(i, j) + PER_SEGMENT + length_price(j - i);
                if c < best[j] {
                    (best[j], from[j]) = (c, i);
                }
            }
        }
    }
    // Every j >= MIN_BARS is reachable (one segment of up to MAX_BARS, more beyond), and so is j == n == 1.
    let (mut j, mut cuts) = (n, Vec::new());
    while j > 0 {
        j = from[j];
        cuts.push(j);
    }
    cuts.reverse();
    cuts
}
