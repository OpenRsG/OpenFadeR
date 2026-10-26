//! Cue sections: `PCOB` / `PCPT` (`.DAT`, `.EXT`) and `PCO2` / `PCP2` (`.EXT`).

use super::{sec, utf16, words};
use openfader_plugin::library::Cue;

/// Hot lists are sorted by descending time (observed), memory lists ascending.
pub(super) fn by_time(mut c: Vec<&Cue>, descending: bool) -> Vec<&Cue> {
    c.sort_by_key(|c| c.time_ms);
    if descending {
        c.reverse();
    }
    c
}

/// The 12 bytes both entry kinds carry: type u1 (1 cue, 2 loop), `00 03 e8`, time u4, loop end u4 (`0xffffffff` for a cue).
fn span(c: &Cue) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0] = if c.loop_end_ms.is_some() { 2 } else { 1 };
    b[1..4].copy_from_slice(&[0, 3, 0xe8]);
    b[4..8].copy_from_slice(&c.time_ms.to_be_bytes());
    b[8..].copy_from_slice(&c.loop_end_ms.unwrap_or(u32::MAX).to_be_bytes());
    b
}

/// `PCOB` (type 1 hot, 0 memory): `type u4, 0 u2, num_cues u2, 0xffffffff`, 56-byte `PCPT` entries.
/// Hot entries have order 0xffff/0xffff; memory entries carry previous/next index (0xffff at the ends).
pub(super) fn pcob(ty: u32, cues: &[&Cue]) -> Vec<u8> {
    let entries: Vec<u8> = cues
        .iter()
        .enumerate()
        .flat_map(|(i, c)| {
            let (first, last) = if ty == 1 {
                (0xffff, 0xffff)
            } else {
                (
                    i.checked_sub(1).map_or(0xffff, |p| p as u16),
                    if i + 1 < cues.len() {
                        i as u16 + 1
                    } else {
                        0xffff
                    },
                )
            };
            let mut e = sec(b"PCPT", 28, &[], &[0; 28]);
            e[12..16].copy_from_slice(&u32::from(c.hot_cue).to_be_bytes());
            // status 0 @16 (observed), 0x00010000 @20
            e[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes());
            e[24..26].copy_from_slice(&first.to_be_bytes());
            e[26..28].copy_from_slice(&last.to_be_bytes());
            e[28..40].copy_from_slice(&span(c));
            e
        })
        .collect();
    sec(
        b"PCOB",
        24,
        &words(&[ty, cues.len() as u32, u32::MAX]),
        &entries,
    )
}

/// `PCO2` (type 1 hot, 0 memory): `type u4, num_cues u2, 0 u2`, `PCP2` entries (see [`pcp2`]).
pub(super) fn pco2(ty: u32, cues: &[&Cue]) -> Vec<u8> {
    sec(
        b"PCO2",
        20,
        &words(&[ty, (cues.len() as u32) << 16]),
        &cues.iter().flat_map(|c| pcp2(c)).collect::<Vec<u8>>(),
    )
}

/// `PCP2`: header 16; `hot_cue u4 @12, type u1 @16, 00 03 e8, time u4 @20, loop u4 @24, color_id u1 @28, 01 @29`,
/// `len_comment u4 @40` (bytes incl. NUL, 0 when empty), comment UTF-16BE + NUL, `color_code u1, r, g, b`, 40 zero bytes
/// (observed: 88 bytes for an empty comment). `color_code` is 0 as in the observed hot cues (they carry rgb with code 0);
/// no colour: rgb 0,0,0.
fn pcp2(c: &Cue) -> Vec<u8> {
    let comment = c
        .comment
        .as_deref()
        .filter(|t| !t.is_empty())
        .map(utf16)
        .unwrap_or_default();
    let [r, g, b] = c.color.unwrap_or_default();
    let mut e = sec(b"PCP2", 16, &[], &[0; 28]);
    e[12..16].copy_from_slice(&u32::from(c.hot_cue).to_be_bytes());
    e[16..28].copy_from_slice(&span(c));
    e[29] = 1;
    e[40..44].copy_from_slice(&(comment.len() as u32).to_be_bytes());
    e.extend(comment);
    e.extend([0, r, g, b]);
    e.extend([0; 40]);
    let len = e.len() as u32;
    e[8..12].copy_from_slice(&len.to_be_bytes());
    e
}
