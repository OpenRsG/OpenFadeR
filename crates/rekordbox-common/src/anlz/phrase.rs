//! `PSSI` song structure (phrases) in `.EXT`. Layout per crate-digger `anlz.adoc` "Song Structure Tag"
//! and `rekordbox_anlz.ksy` (`song_structure_tag`, pinned commit in the section doc), whose
//! contributor credits Michael Ganss for the analysis:
//! - header (len 0x20): `len_entry_bytes u4 @12` (24), `len_entries u2 @16`, `mood u2 @18`,
//!   `end_beat u2 @0x1a`; entries from 0x20, `len_entry_bytes` each.
//! - entry: `index u2, beat u2, kind u2 @4, k1 @7, k2 @9, k3 @0x13, fill @0x15, beat_fill u2 @0x16`.
//! - mask: rekordbox 6+ XORs every byte after `len_entries` (from 18) with `BASE[i % 19] + len_entries`
//!   (byte add), `i` counted from 18. Masked iff the raw mood is > 20 (the spec's own test).
//!   Observed on a real 24-track export: all 24 are masked, and the unmasked indices run 1..n.
//! - kinds (`phrase-labels`, `high-phrase-variants` tables): the id means different phrases per mood.
//!
//! Beat numbers are 1-based indices into the `PQTZ` grid (observed: first phrase starts at beat 1..4,
//! every start ≤ grid length). Not decoded (no use here): bank, the extra "Up 3" beats, unknown bytes.

use super::{Mood, Phrase, PhraseKind as Kind, Phrases, u16_at, u32_at};

const BASE: [u8; 19] = [
    0xCB, 0xE1, 0xEE, 0xFA, 0xE5, 0xEE, 0xAD, 0xEE, 0xE9, 0xD2, 0xE9, 0xEB, 0xE1, 0xE9, 0xF3, 0xE8,
    0xE9, 0xF4, 0xE1,
];

/// `kind id`, flags → kind and label number, per mood (spec tables).
fn resolve(mood: Mood, id: u16, k1: u8, k2: u8, k3: u8) -> Result<(Kind, u8), String> {
    let style = |one: u8| if k1 == 1 { one } else { one + 1 };
    Ok(match (mood, id) {
        (Mood::High, 1) => (Kind::Intro, style(1)),
        (Mood::High, 2) => (
            Kind::Up,
            if k2 == 1 {
                3
            } else if k3 == 1 {
                2
            } else {
                1
            },
        ),
        (Mood::High, 3) => (Kind::Down, 0),
        (Mood::High, 5) => (Kind::Chorus, style(1)),
        (Mood::High, 6) => (Kind::Outro, style(1)),
        (Mood::Mid, 2..=7) => (Kind::Verse, (id - 1) as u8),
        (Mood::Low, 2..=4) => (Kind::Verse, 1),
        (Mood::Low, 5..=7) => (Kind::Verse, 2),
        (Mood::Mid | Mood::Low, 1) => (Kind::Intro, 0),
        (Mood::Mid | Mood::Low, 8) => (Kind::Bridge, 0),
        (Mood::Mid | Mood::Low, 9) => (Kind::Chorus, 0),
        (Mood::Mid | Mood::Low, 10) => (Kind::Outro, 0),
        _ => return Err(format!("PSSI: unknown phrase id {id} for {mood:?} mood")),
    })
}

/// `s` is the whole section (12-byte tag header included); `header` its `len_header`.
pub(super) fn parse(s: &[u8], header: usize) -> Result<Phrases, String> {
    let (stride, n, raw_mood) = (u32_at(s, 12)? as usize, u16_at(s, 16)?, u16_at(s, 18)?);
    if header < 0x20 || stride < 0x18 || header + n as usize * stride > s.len() {
        return Err("PSSI overflow".into());
    }
    let mut b = s.to_vec();
    if raw_mood > 20 {
        b[18..]
            .iter_mut()
            .zip(BASE.iter().cycle())
            .for_each(|(v, m)| *v ^= m.wrapping_add(n as u8));
    }
    let mood = match u16_at(&b, 18)? {
        1 => Mood::High,
        3 => Mood::Low,
        _ => Mood::Mid, // 2, and the spec's generic reading for unrecognised moods
    };
    let end = u16_at(&b, 0x1a)? as u32;
    let mut list: Vec<Phrase> = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        let e = &b[header + i * stride..];
        let beat = u16_at(e, 2)? as u32;
        let (kind, variant) = resolve(mood, u16_at(e, 4)?, e[7], e[9], e[0x13])?;
        if let Some(prev) = list.last_mut() {
            prev.end_beat = beat;
        }
        let fill = (e[0x15] != 0)
            .then(|| u16_at(e, 0x16))
            .transpose()?
            .map(u32::from);
        list.push(Phrase {
            beat,
            end_beat: end,
            kind,
            variant,
            fill,
        });
    }
    Ok(Phrases { mood, list })
}

#[cfg(test)]
mod tests {
    use super::super::{Beat, parse as parse_anlz, testutil::*};
    use super::*;

    /// (beat, kind id, k1, k2, k3, fill beat or 0)
    fn pssi(mood: u16, end: u16, entries: &[(u16, u16, u8, u8, u8, u16)], masked: bool) -> Vec<u8> {
        let mut body = vec![0u8; 14];
        body[..2].copy_from_slice(&mood.to_be_bytes());
        body[8..10].copy_from_slice(&end.to_be_bytes());
        for (i, &(beat, id, k1, k2, k3, fill)) in entries.iter().enumerate() {
            let mut e = [0u8; 24];
            e[..2].copy_from_slice(&(i as u16 + 1).to_be_bytes());
            e[2..4].copy_from_slice(&beat.to_be_bytes());
            e[4..6].copy_from_slice(&id.to_be_bytes());
            (e[7], e[9], e[0x13]) = (k1, k2, k3);
            e[0x15] = (fill != 0) as u8;
            e[0x16..].copy_from_slice(&fill.to_be_bytes());
            body.extend(e);
        }
        if masked {
            let c = entries.len() as u8;
            body.iter_mut()
                .zip(BASE.iter().cycle())
                .for_each(|(v, m)| *v ^= m.wrapping_add(c));
        }
        // section: header 0x20 = 12 + len_entry_bytes u4 + len_entries u2 + body (mood..)
        let mut s = sec(b"PSSI", 0x20, &body[14..]);
        s[12..16].copy_from_slice(&24u32.to_be_bytes());
        s[16..18].copy_from_slice(&(entries.len() as u16).to_be_bytes());
        s[18..32].copy_from_slice(&body[..14]);
        s
    }

    const HIGH: [(u16, u16, u8, u8, u8, u16); 5] = [
        (1, 1, 1, 0, 0, 32),
        (33, 2, 0, 1, 0, 0),
        (65, 3, 0, 0, 0, 0),
        (97, 5, 0, 0, 0, 0),
        (129, 6, 1, 0, 0, 0),
    ];

    /// Every PSSI id / flag combination of each mood lands on its own legend entry, and every legend entry is reached: the
    /// legend (and so the colour table) is complete for all kinds and moods.
    #[test]
    fn every_phrase_of_every_mood_has_its_own_legend_slot() {
        let flags = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1)];
        for (mood, ids) in [
            (Mood::High, 1..=6u16),
            (Mood::Mid, 1..=10),
            (Mood::Low, 1..=10),
        ] {
            let mut hit = std::collections::BTreeSet::new();
            for id in ids {
                for &(k1, k2, k3) in &flags {
                    let Ok((kind, variant)) = resolve(mood, id, k1, k2, k3) else {
                        continue;
                    };
                    let s = mood
                        .slot(kind, variant)
                        .unwrap_or_else(|| panic!("{mood:?} id {id}: {kind:?} {variant}"));
                    let label = Phrase {
                        beat: 1,
                        end_beat: 2,
                        kind,
                        variant,
                        fill: None,
                    }
                    .label();
                    assert_eq!(mood.legend()[s], label);
                    hit.insert(s);
                }
            }
            assert_eq!(hit.len(), mood.legend().len(), "{mood:?}");
        }
        assert_eq!(Mood::High.slot(Kind::Intro, 0), Some(0)); // our own analysis leaves unnumbered styles at 0
        assert_eq!(Mood::High.slot(Kind::Verse, 1), None);
        assert_eq!(Mood::Low.slot(Kind::Verse, 3), None);
    }

    #[test]
    fn masked_and_unmasked_decode_identically() {
        let plain = parse(&pssi(1, 161, &HIGH, false), 0x20).unwrap();
        let masked = pssi(1, 161, &HIGH, true);
        assert_ne!(masked, pssi(1, 161, &HIGH, false));
        assert_eq!(parse(&masked, 0x20).unwrap(), plain);
        assert_eq!(plain.mood, Mood::High);
        let l: Vec<_> = plain.list.iter().map(Phrase::label).collect();
        assert_eq!(l, ["Intro 1", "Up 3", "Down", "Chorus 2", "Outro 1"]);
        // end = next start; last = header end_beat; fill only when flagged
        assert_eq!(
            plain.list.iter().map(|p| p.end_beat).collect::<Vec<_>>(),
            [33, 65, 97, 129, 161]
        );
        assert_eq!(
            plain.list.iter().map(|p| p.fill).collect::<Vec<_>>(),
            [Some(32), None, None, None, None]
        );
    }

    #[test]
    fn mid_and_low_moods_label_by_their_own_tables() {
        let ids = [
            (1, 1, 0, 0, 0, 0),
            (9, 3, 0, 0, 0, 0),
            (17, 5, 0, 0, 0, 0),
            (25, 8, 0, 0, 0, 0),
            (33, 9, 0, 0, 0, 0),
        ];
        let label = |mood| {
            parse(&pssi(mood, 41, &ids, true), 0x20)
                .unwrap()
                .list
                .iter()
                .map(Phrase::label)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            label(2),
            ["Intro", "Verse 2", "Verse 4", "Bridge", "Chorus"]
        );
        assert_eq!(
            label(3),
            ["Intro", "Verse 1", "Verse 2", "Bridge", "Chorus"]
        );
        assert!(parse(&pssi(1, 33, &[(1, 4, 0, 0, 0, 0)], false), 0x20).is_err()); // no id 4 in high mood
    }

    #[test]
    fn parse_dispatches_pssi_and_rejects_overflow() {
        let s = pssi(1, 161, &HIGH, true);
        assert_eq!(
            parse_anlz(&pmai(&s)).unwrap().phrases.unwrap().list.len(),
            5
        );
        let mut bad = s.clone();
        bad[16..18].copy_from_slice(&200u16.to_be_bytes());
        assert!(parse(&bad, 0x20).is_err());
    }

    #[test]
    fn beat_numbers_map_onto_the_grid_and_extend_past_it() {
        let beats: Vec<_> = (0..4)
            .map(|i| Beat {
                bar_pos: i as u16 % 4 + 1,
                bpm_x100: 12000,
                time_ms: 100 + i * 500,
            })
            .collect();
        let p = Phrase {
            beat: 2,
            end_beat: 6,
            kind: Kind::Verse,
            variant: 0,
            fill: None,
        };
        // beat 6 = two beats past the last (index 3 → 5): 1600 + 2·500
        assert_eq!(p.span_ms(&beats), (600.0, 2600.0));
    }
}
