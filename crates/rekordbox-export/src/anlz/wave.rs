//! Waveform columns of the ANLZ files. The host's own analysis only yields `three_band_detail` / `three_band_preview`; the
//! rest is derived from whatever the [`Analysis`] carries, real data first. The spec gives no mapping, so these are our choice:
//! - mono column byte = `whiteness << 5 | height`, height = `max(low,mid,high) * 31 / 127`, whiteness = `7 * high / (low+mid+high)`;
//! - `PWAV` (400): a real preview is resampled (max-height column of each window); a derived one is the mean of each window,
//!   heights stretched to 2..=25 and whiteness capped at 5 (every one of the 24 PWAV of a real rekordbox 7 export spans exactly
//!   2..=25 with whiteness 0..=5, and follows the window mean of its PWV3 (r 0.93) far better than the max (r 0.81));
//! - `PWV2` (100): the mean height of 4 PWAV columns stretched to 1..=15 (real: max 15 in every file, min 1..3);
//! - `PWV5` u16 = `r<<13 | g<<10 | b<<7 | h<<2` with r=low, g=mid, b=high scaled so the strongest band is 7 (as `color_detail_rgbh` reads);
//! - `PWV4` column from the `[low, mid, high]` preview column (fitted on the 24 real files against their PWV6, bytes as
//!   djl-analysis names them): 0 = `0.97 l + 0.64 m + 0.54 h + 45`, 1 = `256 - byte 0` (sum 256 is the mode), 2 (bottom-half
//!   energy) = `1.75 l + 0.36 m + 21`, 3..5 = `2 l, 2 m, 2 h`; every byte at most 127 like the real ones (byte 1 255), a silent
//!   column all zero.

use openfader_plugin::library::Analysis;
use std::borrow::Cow;

/// Mono column byte (`whiteness << 5 | height`) of a `[low, mid, high]` column.
pub(super) fn mono([l, m, h]: &[u8; 3]) -> u8 {
    let (l, m, h) = (u32::from(*l), u32::from(*m), u32::from(*h));
    let sum = l + m + h;
    let white = if sum == 0 { 0 } else { 7 * h / sum };
    (white << 5 | (l.max(m).max(h) * 31 / 127).min(31)) as u8
}

/// `PWV5` word of a `[low, mid, high]` column.
pub(super) fn color([l, m, h]: &[u8; 3]) -> u16 {
    let (l, m, h) = (u16::from(*l), u16::from(*m), u16::from(*h));
    let max = l.max(m).max(h);
    let c = |v: u16| if max == 0 { 0 } else { v * 7 / max };
    c(l) << 13 | c(m) << 10 | c(h) << 7 | (max * 31 / 127).min(31) << 2
}

/// The real mono waveform when the source carries one, else derived from the 3-band one.
pub(super) fn mono_of<'a>(real: &'a [u8], bands: &[[u8; 3]]) -> Cow<'a, [u8]> {
    if real.is_empty() {
        Cow::Owned(bands.iter().map(mono).collect())
    } else {
        Cow::Borrowed(real)
    }
}

/// The colour detail (`PWV5` words) the source carries, else derived from the 3-band detail.
pub(super) fn color_detail(a: &Analysis) -> Cow<'_, [u16]> {
    if a.color_detail.is_empty() {
        Cow::Owned(a.three_band_detail.iter().map(color).collect())
    } else {
        Cow::Borrowed(&a.color_detail)
    }
}

/// The colour preview (`PWV4` columns) the source carries, else derived from the 3-band preview.
pub(super) fn color_preview(a: &Analysis) -> Cow<'_, [[u8; 6]]> {
    if a.color_preview.is_empty() {
        Cow::Owned(
            a.three_band_preview
                .iter()
                .map(|&c| color_preview_column(c))
                .collect(),
        )
    } else {
        Cow::Borrowed(&a.color_preview)
    }
}

/// `PWV4` column of a `[low, mid, high]` preview column (see the module doc).
pub(super) fn color_preview_column([l, m, h]: [u8; 3]) -> [u8; 6] {
    let (l, m, h) = (u32::from(l), u32::from(m), u32::from(h));
    if l + m + h == 0 {
        return [0; 6];
    }
    let cap = |v: u32| v.min(127) as u8;
    let white = cap((97 * l + 64 * m + 54 * h) / 100 + 45);
    [
        white,
        (256 - u32::from(white)).min(255) as u8,
        cap((175 * l + 36 * m) / 100 + 21),
        cap(2 * l),
        cap(2 * m),
        cap(2 * h),
    ]
}

/// `PWAV` columns derived from a mono overview: the mean of each window, heights stretched to 2..=25, whiteness at most 5.
pub(super) fn preview_of(overview: &[u8], n: usize) -> Vec<u8> {
    if overview.is_empty() {
        return vec![0; n];
    }
    let means: Vec<(u32, u32)> = (0..n)
        .map(|i| {
            let a = i * overview.len() / n;
            let b = ((i + 1) * overview.len() / n).max(a + 1).min(overview.len());
            let w = &overview[a..b];
            let k = w.len() as u32;
            let sum = |f: fn(u8) -> u32| w.iter().map(|&v| f(v)).sum::<u32>();
            (sum(|v| u32::from(v & 31)) / k, sum(|v| u32::from(v >> 5)) / k)
        })
        .collect();
    let stretched = stretch(means.iter().map(|m| m.0), 2, 25);
    means
        .iter()
        .zip(stretched)
        .map(|(&(_, white), h)| (white.min(5) << 5) as u8 | h)
        .collect()
}

/// `PWV2` (100 columns) from the `PWAV` columns: the mean height of 4 columns stretched to 1..=15.
pub(super) fn tiny_of(pwav: &[u8]) -> Vec<u8> {
    let n = 100;
    let means = (0..n).map(|i| {
        let a = i * pwav.len() / n;
        let w = &pwav[a.min(pwav.len())..((i + 1) * pwav.len() / n).max(a + 1).min(pwav.len())];
        w.iter().map(|&v| u32::from(v & 31)).sum::<u32>() / w.len().max(1) as u32
    });
    stretch(means, 1, 15)
}

/// `values` mapped linearly onto `lo..=hi` (their own minimum to `lo`, maximum to `hi`); all equal stays as is (silence).
fn stretch(values: impl Iterator<Item = u32>, lo: u32, hi: u32) -> Vec<u8> {
    let v: Vec<u32> = values.collect();
    let (min, max) = (v.iter().copied().min().unwrap_or(0), v.iter().copied().max().unwrap_or(0));
    v.iter()
        .map(|&x| if max == min { x.min(hi) } else { lo + (x - min) * (hi - lo) / (max - min) } as u8)
        .collect()
}

/// `n` columns; each takes the entry with the largest `key` of its window (nearest entry when upsampling);
/// default entries when the source is empty.
pub(super) fn resample<T: Copy + Default>(src: &[T], n: usize, key: fn(&T) -> u8) -> Vec<T> {
    (0..n)
        .map(|i| {
            let a = i * src.len() / n;
            let b = ((i + 1) * src.len() / n).max(a + 1).min(src.len());
            src.get(a..b)
                .and_then(|w| w.iter().copied().max_by_key(key))
                .unwrap_or_default()
        })
        .collect()
}
