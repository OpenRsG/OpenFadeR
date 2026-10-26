//! `PSSI` (song structure, `.EXT`).

use super::words;
use openfader_plugin::library::{Kind as PhraseKind, Mood, Phrases};

/// `PSSI` (spec `song_structure_tag`, the inverse of `analysis::phrase`'s reader): header 0x20 = `tag, 0x20, len, entry
/// bytes 24, count u2, mood u2 (1 high, 2 mid, 3 low), 6 zero bytes, end_beat u2, 0 u2, bank 0, 0`, then 24-byte entries
/// `index u2, beat u2, kind u2, 0, k1, 0, k2, 0, b = 0, 6 zero bytes, 0, k3, 0, fill u1, fill beat u2`. The bytes after the
/// count are XOR-masked with the spec's 19-byte key + count, as on every real export (observed). A kind the mood has no
/// id for becomes the nearest one (high: verse → Up, bridge → Down; mid/low: Up → Chorus, Down → Bridge).
pub(super) fn pssi(p: &Phrases) -> Vec<u8> {
    const BASE: [u8; 19] = [
        0xCB, 0xE1, 0xEE, 0xFA, 0xE5, 0xEE, 0xAD, 0xEE, 0xE9, 0xD2, 0xE9, 0xEB, 0xE1, 0xE9, 0xF3,
        0xE8, 0xE9, 0xF4, 0xE1,
    ];
    let n = p.list.len();
    let mut s = vec![0u8; 0x20 + 24 * n];
    s[12..16].copy_from_slice(&24u32.to_be_bytes());
    s[16..18].copy_from_slice(&(n as u16).to_be_bytes());
    s[18..20].copy_from_slice(
        &(match p.mood {
            Mood::High => 1u16,
            Mood::Mid => 2,
            Mood::Low => 3,
        })
        .to_be_bytes(),
    );
    s[26..28].copy_from_slice(&(p.list.last().map_or(0, |l| l.end_beat) as u16).to_be_bytes());
    for (i, ph) in p.list.iter().enumerate() {
        let (id, k1, k2, k3) = phrase_id(p.mood, ph.kind, ph.variant);
        let e = &mut s[0x20 + 24 * i..0x20 + 24 * (i + 1)];
        e[0..2].copy_from_slice(&(i as u16 + 1).to_be_bytes());
        e[2..4].copy_from_slice(&(ph.beat as u16).to_be_bytes());
        e[4..6].copy_from_slice(&id.to_be_bytes());
        (e[7], e[9], e[0x13]) = (k1, k2, k3);
        e[0x15] = u8::from(ph.fill.is_some());
        e[0x16..0x18].copy_from_slice(&(ph.fill.unwrap_or(0) as u16).to_be_bytes());
    }
    for (v, m) in s[18..].iter_mut().zip(BASE.iter().cycle()) {
        *v ^= m.wrapping_add(n as u8);
    }
    let total = s.len() as u32;
    s[..12].copy_from_slice(&[b"PSSI".as_slice(), &words(&[0x20, total])].concat());
    s
}

/// Phrase id and the `k1`/`k2`/`k3` flags of a phrase (spec mood tables; see `analysis::phrase::resolve`).
fn phrase_id(mood: Mood, kind: PhraseKind, variant: u8) -> (u16, u8, u8, u8) {
    let k1 = if variant <= 1 { 1 } else { 2 };
    let v = variant.max(1);
    match (mood, kind) {
        (Mood::High, PhraseKind::Intro) => (1, k1, 0, 0),
        (Mood::High, PhraseKind::Up | PhraseKind::Verse) => {
            (2, 0, u8::from(variant == 3), u8::from(variant == 2))
        }
        (Mood::High, PhraseKind::Down | PhraseKind::Bridge) => (3, 0, 0, 0),
        (Mood::High, PhraseKind::Chorus) => (5, k1, 0, 0),
        (Mood::High, PhraseKind::Outro) => (6, k1, 0, 0),
        (_, PhraseKind::Intro) => (1, 0, 0, 0),
        (Mood::Mid, PhraseKind::Verse) => (u16::from(v.min(6)) + 1, 0, 0, 0),
        (_, PhraseKind::Verse) => (if v == 1 { 2 } else { 5 }, 0, 0, 0),
        (_, PhraseKind::Bridge | PhraseKind::Down) => (8, 0, 0, 0),
        (_, PhraseKind::Chorus | PhraseKind::Up) => (9, 0, 0, 0),
        (_, PhraseKind::Outro) => (10, 0, 0, 0),
    }
}
