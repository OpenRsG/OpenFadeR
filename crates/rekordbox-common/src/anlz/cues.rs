//! Cue tags: `PCOB`/`PCPT` (`.DAT`) and `PCO2`/`PCP2` (`.EXT`). `PCP2` offsets are spec-derived
//! (crate-digger); `PCPT` entry offsets match rekordbox.exe's PCPT reader (observed), which reads
//! 0x0c and 0x0e as separate u16s; field meanings are from public format documentation.

use super::{Cue, u16_at, u32_at, utf16_be};

/// rekordbox 7's 65-entry "rainbow" hot-cue colour table (index 0 and 63 black, 64 white). Observed:
/// rekordbox.exe static analysis, global at 0x1461453a0 filled by the initialiser at 0x140c9b7e0; extraction and
/// provenance in `.local/re/cues/palette.md`. These are plain RGB facts, also what `PCP2` stores for a coloured cue.
pub const RAINBOW: [[u8; 3]; 65] = [
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0xff],
    [0x00, 0x1c, 0xff],
    [0x00, 0x38, 0xff],
    [0x00, 0x54, 0xff],
    [0x00, 0x70, 0xff],
    [0x00, 0x8c, 0xff],
    [0x00, 0xa8, 0xff],
    [0x00, 0xc4, 0xff],
    [0x00, 0xe0, 0xff],
    [0x00, 0xff, 0xff],
    [0x00, 0xff, 0xe8],
    [0x00, 0xff, 0xd1],
    [0x00, 0xff, 0xba],
    [0x00, 0xff, 0xa3],
    [0x00, 0xff, 0x8c],
    [0x00, 0xff, 0x75],
    [0x00, 0xff, 0x5e],
    [0x00, 0xff, 0x47],
    [0x00, 0xff, 0x30],
    [0x00, 0xff, 0x1a],
    [0x00, 0xff, 0x00],
    [0x1a, 0xff, 0x00],
    [0x33, 0xff, 0x00],
    [0x4d, 0xff, 0x00],
    [0x66, 0xff, 0x00],
    [0x80, 0xff, 0x00],
    [0x99, 0xff, 0x00],
    [0xb3, 0xff, 0x00],
    [0xcc, 0xff, 0x00],
    [0xe6, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0xff, 0xe8, 0x00],
    [0xff, 0xd1, 0x00],
    [0xff, 0xba, 0x00],
    [0xff, 0xa3, 0x00],
    [0xff, 0x8c, 0x00],
    [0xff, 0x75, 0x00],
    [0xff, 0x5e, 0x00],
    [0xff, 0x47, 0x00],
    [0xff, 0x30, 0x00],
    [0xff, 0x1a, 0x00],
    [0xff, 0x00, 0x00],
    [0xff, 0x00, 0x17],
    [0xff, 0x00, 0x2e],
    [0xff, 0x00, 0x45],
    [0xff, 0x00, 0x5c],
    [0xff, 0x00, 0x73],
    [0xff, 0x00, 0x8a],
    [0xff, 0x00, 0xa1],
    [0xff, 0x00, 0xb8],
    [0xff, 0x00, 0xcf],
    [0xff, 0x00, 0xe6],
    [0xff, 0x00, 0xff],
    [0xe6, 0x00, 0xff],
    [0xcc, 0x00, 0xff],
    [0xb3, 0x00, 0xff],
    [0x99, 0x00, 0xff],
    [0x80, 0x00, 0xff],
    [0x66, 0x00, 0xff],
    [0x4d, 0x00, 0xff],
    [0x33, 0x00, 0xff],
    [0x1a, 0x00, 0xff],
    [0x00, 0x00, 0x00],
    [0xff, 0xff, 0xff],
];

/// The colours the cue colour picker offers: [`RAINBOW`] without its two black entries (0, 63).
pub fn picker() -> impl Iterator<Item = [u8; 3]> {
    RAINBOW
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != 0 && i != 63)
        .map(|(_, &c)| c)
}

/// Cue list: `PCOB` header holds `num_cues u2` at 18; `PCO2` header (`type u4, num_cues u2, pad u2`) at 16.
/// Entries start at the section header end; each is `fourcc, header, len, ...`.
fn entries(
    s: &[u8],
    header: usize,
    count_at: usize,
    fourcc: &[u8; 4],
    what: &str,
) -> Result<Vec<(usize, usize)>, String> {
    let count = u16_at(s, count_at)? as usize;
    let mut out = Vec::with_capacity(count);
    let mut o = header;
    for _ in 0..count {
        if s.get(o..o + 4) != Some(fourcc) {
            return Err(format!("expected {what} at {o}"));
        }
        let len = u32_at(s, o + 8)? as usize;
        if len < 40 || o + len > s.len() {
            return Err(format!("{what} overflow"));
        }
        out.push((o, len));
        o += len;
    }
    Ok(out)
}

pub(super) fn parse_pcob(s: &[u8], header: usize) -> Result<Vec<Cue>, String> {
    entries(s, header, 18, b"PCPT", "PCPT")?
        .into_iter()
        .map(|(o, len)| {
            let e = &s[o..o + len];
            Ok(Cue {
                hot_cue: u16_at(e, 0x0e)?,
                time_ms: u32_at(e, 0x20)?,
                loop_end_ms: (e[0x1c] == 2).then(|| u32_at(e, 0x24)).transpose()?,
                color: None,
                comment: None,
            })
        })
        .collect()
}

pub(super) fn parse_pco2(s: &[u8], header: usize) -> Result<Vec<Cue>, String> {
    entries(s, header, 16, b"PCP2", "PCP2")?
        .into_iter()
        .map(|(o, len)| {
            let e = &s[o..o + len];
            let (mut comment, mut color) = (None, None);
            if len > 43 {
                let n = u32_at(e, 40)? as usize;
                let text = utf16_be(e.get(44..44 + n).ok_or("PCP2 comment overflow")?);
                comment = (!text.is_empty()).then_some(text);
                // color_code at 44+n, then red, green, blue.
                color = e.get(45 + n..48 + n).map(|c| [c[0], c[1], c[2]]);
            }
            Ok(Cue {
                hot_cue: u16_at(e, 14)?,
                time_ms: u32_at(e, 20)?,
                loop_end_ms: (e[16] == 2).then(|| u32_at(e, 24)).transpose()?,
                color,
                comment,
            })
        })
        .collect()
}

/// Adds `new` cues; any existing cue with the same (hot_cue, time) is replaced.
pub(super) fn merge_cues(cues: &mut Vec<Cue>, new: Vec<Cue>) {
    cues.retain(|c| {
        !new.iter()
            .any(|n| (n.hot_cue, n.time_ms) == (c.hot_cue, c.time_ms))
    });
    cues.extend(new);
    cues.sort_by_key(|c| c.time_ms);
}

#[cfg(test)]
mod tests {
    use super::super::{parse, testutil::*};
    use super::*;

    fn pcpt(hot: u16, time: u32, lp: Option<u32>) -> Vec<u8> {
        let mut p = [
            b"PCPT".as_slice(),
            &28u32.to_be_bytes(),
            &56u32.to_be_bytes(),
            &2u32.to_be_bytes(),
        ]
        .concat();
        p.resize(56, 0);
        p[14..16].copy_from_slice(&hot.to_be_bytes());
        p[32..36].copy_from_slice(&time.to_be_bytes());
        if let Some(end) = lp {
            p[28] = 2;
            p[36..40].copy_from_slice(&end.to_be_bytes());
        }
        p
    }

    #[test]
    fn pcob_loop_cue() {
        let mut pcob = sec(b"PCOB", 24, &pcpt(2, 1000, Some(3000)));
        pcob[18..20].copy_from_slice(&1u16.to_be_bytes());
        let a = parse(&pmai(&pcob)).unwrap();
        assert_eq!(
            a.cues,
            [Cue {
                hot_cue: 2,
                time_ms: 1000,
                loop_end_ms: Some(3000),
                color: None,
                comment: None
            }]
        );
        pcob[18..20].copy_from_slice(&2u16.to_be_bytes());
        assert!(parse(&pmai(&pcob)).is_err());
    }

    #[test]
    fn rainbow_picker() {
        assert_eq!(picker().count(), 63);
        assert!(picker().all(|c| c != [0, 0, 0]));
        assert_eq!(RAINBOW[64], [255, 255, 255]);
    }

    #[test]
    fn pco2_comment_color_and_replaces_pcob() {
        let mut pcob = sec(b"PCOB", 24, &pcpt(1, 500, None));
        pcob[18..20].copy_from_slice(&1u16.to_be_bytes());
        let f = pmai(
            &[
                pcob,
                pco2(&[
                    pcp2(1, 1, 500, 0, "Drop é", [10, 20, 30]),
                    pcp2(0, 2, 900, 1900, "", [0, 0, 0]),
                ]),
            ]
            .concat(),
        );
        let a = parse(&f).unwrap();
        assert_eq!(a.cues.len(), 2);
        assert_eq!(
            a.cues[0],
            Cue {
                hot_cue: 1,
                time_ms: 500,
                loop_end_ms: None,
                color: Some([10, 20, 30]),
                comment: Some("Drop é".into())
            }
        );
        assert_eq!(a.cues[1].loop_end_ms, Some(1900));
        assert_eq!(a.cues[1].comment, None);
        // comment length overflowing the entry
        let mut bad = pco2(&[pcp2(1, 1, 500, 0, "x", [0; 3])]);
        bad[20 + 40..20 + 44].copy_from_slice(&999u32.to_be_bytes());
        assert!(parse(&pmai(&bad)).is_err());
        // truncated entry list
        let mut few = pco2(&[pcp2(1, 1, 500, 0, "x", [0; 3])]);
        few[16..18].copy_from_slice(&2u16.to_be_bytes());
        assert!(parse(&pmai(&few)).is_err());
    }
}
