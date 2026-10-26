//! Waveform tags: `PWAV` (`.DAT` overview), `PWV3`/`PWV4`/`PWV5` (`.EXT`), `PWV6`/`PWV7`/`PWVC`
//! (`.2EX`). Entry-table layout `len_entry_bytes u4, len_entries u4, u4` is spec (crate-digger);
//! `PWV5` bit layout is from `anlz.adoc`; `PWV6`/`PWV7`/`PWVC` layouts are observed on a real export.
//! `PWV4` (colour preview, 6 bytes x 1200 columns) layout is spec (crate-digger `anlz.adoc`, "Waveform Color Preview
//! Tag"); the per-byte meaning is inferred (beat-link `segmentColor`) and applied by the overview renderer.

use super::{Analysis, u16_at, u32_at};

/// Entry table of `N`-byte entries, starting at the section header end.
fn entries<const N: usize>(s: &[u8], header: usize, what: &str) -> Result<Vec<[u8; N]>, String> {
    if u32_at(s, 12)? as usize != N {
        return Err(format!("{what}: entry size is not {N}"));
    }
    let n = u32_at(s, 16)? as usize;
    let raw = s
        .get(header..header + n * N)
        .ok_or_else(|| format!("{what} overflow"))?;
    Ok(raw.chunks_exact(N).map(|c| c.try_into().unwrap()).collect())
}

pub(super) fn parse(a: &mut Analysis, tag: &[u8], s: &[u8], header: usize) -> Result<(), String> {
    match tag {
        b"PWAV" => {
            let n = u32_at(s, 12)? as usize;
            a.preview = s.get(header..header + n).ok_or("PWAV overflow")?.to_vec();
        }
        b"PWV3" => {
            a.detail = entries::<1>(s, header, "PWV3")?
                .into_iter()
                .map(|[v]| v)
                .collect()
        }
        b"PWV4" => a.color_preview = entries(s, header, "PWV4")?,
        b"PWV5" => {
            a.color_detail = entries::<2>(s, header, "PWV5")?
                .into_iter()
                .map(u16::from_be_bytes)
                .collect()
        }
        b"PWV6" => a.three_band_preview = entries(s, header, "PWV6")?,
        b"PWV7" => a.three_band_detail = entries(s, header, "PWV7")?,
        b"PWVC" => a.vocal_thresholds = Some([u16_at(s, 14)?, u16_at(s, 16)?, u16_at(s, 18)?]),
        _ => unreachable!("waveform::parse called with {tag:?}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{parse, testutil::*};

    #[test]
    fn wave_sections() {
        let f = pmai(
            &[
                wave(b"PWV3", 1, 3, &[0xff, 0x21, 5]),
                wave(b"PWV4", 6, 2, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]),
                wave(b"PWV5", 2, 1, &[0xFF, 0xFC]),
            ]
            .concat(),
        );
        let a = parse(&f).unwrap();
        assert_eq!(a.detail, [0xff, 0x21, 5]);
        assert_eq!(a.color_preview, [[1, 2, 3, 4, 5, 6], [7, 8, 9, 10, 11, 12]]);
        assert_eq!(a.color_detail, [0xFFFC]);
        // overflow: claims 4 entries, carries 3 bytes; wrong entry size; truncated section
        assert!(parse(&pmai(&wave(b"PWV3", 1, 4, &[1, 2, 3]))).is_err());
        assert!(parse(&pmai(&wave(b"PWV4", 5, 1, &[0; 5]))).is_err());
        assert!(parse(&pmai(&wave(b"PWV5", 2, 2, &[0; 2]))).is_err());
        assert!(parse(&f[..f.len() - 1]).is_err());
    }

    #[test]
    fn pwav_preview_and_three_band_sections() {
        let mut pwav = sec(b"PWAV", 20, &[0xff, 0x1f, 0]);
        pwav[12..16].copy_from_slice(&3u32.to_be_bytes());
        assert_eq!(parse(&pmai(&pwav)).unwrap().preview, [0xff, 0x1f, 0]);
        let a = parse(&pmai(&three_band_body())).unwrap();
        assert_eq!(a.three_band_preview, [[1, 2, 3], [4, 5, 6]]);
        assert_eq!(a.three_band_detail, [[7, 8, 9]]);
        assert_eq!(a.vocal_thresholds, Some([80, 90, 100]));
        assert!(parse(&pmai(&wave(b"PWV7", 3, 2, &[0; 3]))).is_err());
        assert!(parse(&pmai(&wave(b"PWV7", 2, 1, &[0; 2]))).is_err());
    }
}
