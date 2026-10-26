//! `PQTZ` beatgrid. Layout per the pinned crate-digger spec (see the section doc): header, then
//! 8-byte entries `bar_pos u2, bpm_x100 u2, time_ms u4`; entry count at section offset 20.

use super::{Beat, u16_at, u32_at};

pub(super) fn parse(s: &[u8], header: usize) -> Result<Vec<Beat>, String> {
    let n = u32_at(s, 20)? as usize;
    if header + n * 8 > s.len() {
        return Err("PQTZ overflow".into());
    }
    (0..n)
        .map(|i| {
            let e = header + i * 8;
            Ok(Beat {
                bar_pos: u16_at(s, e)?,
                bpm_x100: u16_at(s, e + 2)?,
                time_ms: u32_at(s, e + 4)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{parse, testutil::*};
    use super::*;

    #[test]
    fn decodes_beats_and_rejects_overflow() {
        let beats = [
            [0, 1, 0x30, 0x0c, 0, 0, 0, 0],
            [0, 2, 0x30, 0x0c, 0, 0, 0x01, 0xe8],
        ]
        .concat();
        let mut pqtz = sec(b"PQTZ", 24, &beats);
        pqtz[20..24].copy_from_slice(&2u32.to_be_bytes());
        let a = parse(&pmai(&pqtz)).unwrap();
        assert_eq!(
            a.beats[1],
            Beat {
                bar_pos: 2,
                bpm_x100: 12300,
                time_ms: 488
            }
        );
        pqtz[20..24].copy_from_slice(&3u32.to_be_bytes());
        assert!(parse(&pmai(&pqtz)).is_err());
    }
}
