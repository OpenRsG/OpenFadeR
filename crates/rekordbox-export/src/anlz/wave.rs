//! Waveform columns of the ANLZ files. The host's own analysis only yields `three_band_detail` / `three_band_preview`; the
//! rest is derived from whatever the [`Analysis`] carries, real data first. The spec gives no mapping, so these are our choice:
//! - mono column byte = `whiteness << 5 | height`, height = `max(low,mid,high) * 31 / 127`, whiteness = `7 * high / (low+mid+high)`;
//! - `PWAV` (400) / `PWV2` (100, height >> 1) = the overview resampled with the max-height column of each window;
//! - `PWV5` u16 = `r<<13 | g<<10 | b<<7 | h<<2` with r=low, g=mid, b=high scaled so the strongest band is 7 (as `color_detail_rgbh` reads);
//! - `PWV4` column `[0,0,0, 2*low, 2*mid, 2*high]` (bytes 3..5 are RGB energies in the overview renderer).

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
                .map(|&[l, m, h]| {
                    [
                        0,
                        0,
                        0,
                        l.saturating_mul(2),
                        m.saturating_mul(2),
                        h.saturating_mul(2),
                    ]
                })
                .collect(),
        )
    } else {
        Cow::Borrowed(&a.color_preview)
    }
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
