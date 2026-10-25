//! ANLZ writer: the `ANLZ0000.DAT`, `.EXT` and `.2EX` files of one exported track. Big-endian `PMAI` container,
//! `PMAI, len_header 28, len_file, 1, 0x10000, 0x10000, 0` (observed on a real rekordbox 7 export), then sections
//! `fourcc, len_header, len_tag`. Section order and field layouts are the observed ones in `.local/re/export/notes.md`
//! (ANLZ); `rekordbox_common::anlz` decodes what this writes.
//! Children: `cues` (`PCOB`, `PCO2`), `wave` (the waveform columns derived from what the [`Analysis`] carries) and `phrase` (`PSSI`).
//!
//! Not written: `PVDI` (players read `PQTZ`; `PQT2` is written, see [`pqt2`]). `PSSI` (song structure) is written when the
//! [`Analysis`] carries a non-empty phrase list (the stored phrase edit is laid over the analysis before this, `store::apply`).
//! The `PVBR` body is all zero (observed for CBR). Waveform sections with nothing to derive from (`PWV3`..`PWV7`) are omitted;
//! `PWAV`/`PWV2` are always present.

mod cues;
mod phrase;
mod wave;

use cues::{by_time, pco2, pcob};
use openfader_plugin::library::{Analysis, Beat, Cue};

/// The three analysis files of one track.
pub struct Files {
    pub dat: Vec<u8>,
    pub ext: Vec<u8>,
    pub ex2: Vec<u8>,
}

/// `PWAV` / `PWV2` columns (observed).
const PWAV_COLUMNS: usize = 400;
const PWV2_COLUMNS: usize = 100;
/// Hot cues stored in the `.DAT` `PCOB` (A..C); the `.EXT` `PCOB` holds the rest (observed 3 + 5).
const DAT_HOT_CUES: u16 = 3;

/// Encodes the three analysis files. `audio_path` is stored in every `PPTH`, e.g. `/Contents/Artist/Album/x.mp3`.
pub fn encode(a: &Analysis, audio_path: &str) -> Files {
    let ppth = ppth(audio_path);
    let (hot, memory): (Vec<&Cue>, Vec<&Cue>) = a.cues.iter().partition(|c| c.hot_cue >= 1);
    let (hot, memory) = (by_time(hot, true), by_time(memory, false));
    let (dat_hot, ext_hot): (Vec<&Cue>, Vec<&Cue>) =
        hot.iter().partition(|c| c.hot_cue <= DAT_HOT_CUES);

    let pre = wave::mono_of(&a.preview, &a.three_band_preview);
    let det = wave::mono_of(&a.detail, &a.three_band_detail);
    let overview: &[u8] = if pre.is_empty() { &det } else { &pre };
    let resampled = |n| wave::resample(overview, n, |v| v & 31);
    let pwav = resampled(PWAV_COLUMNS);
    let pwv2: Vec<u8> = resampled(PWV2_COLUMNS)
        .iter()
        .map(|v| (v & 31) >> 1)
        .collect();
    let color_detail = wave::color_detail(a);
    let color_preview = wave::color_preview(a);

    let beats: Vec<u8> = a.beats.iter().flat_map(beat).collect();
    let dat = file(vec![
        ppth.clone(),
        sec(b"PVBR", 16, &[], &[0u8; 1604]), // observed: header 16, 1620 bytes in all, zeros for CBR
        sec(
            b"PQTZ",
            24,
            &words(&[0, 0x0008_0000, a.beats.len() as u32]),
            &beats,
        ),
        sec(
            b"PWAV",
            20,
            &words(&[PWAV_COLUMNS as u32, 0x0001_0000]),
            &pwav,
        ),
        sec(
            b"PWV2",
            20,
            &words(&[PWV2_COLUMNS as u32, 0x0001_0000]),
            &pwv2,
        ),
        pcob(1, &dat_hot),
        pcob(0, &memory),
    ]);

    let mut ext = vec![ppth.clone()];
    if !det.is_empty() {
        ext.push(sec(
            b"PWV3",
            24,
            &words(&[1, det.len() as u32, 0x0096_0000]),
            &det,
        ));
    }
    ext.extend([
        pcob(1, &ext_hot),
        pcob(0, &memory),
        pco2(1, &hot),
        pco2(0, &memory),
    ]);
    ext.extend(pqt2(&a.beats));
    if !color_detail.is_empty() {
        let body: Vec<u8> = color_detail.iter().flat_map(|v| v.to_be_bytes()).collect();
        ext.push(sec(
            b"PWV5",
            24,
            &words(&[2, color_detail.len() as u32, 0x0096_0305]),
            &body,
        ));
    }
    if !color_preview.is_empty() {
        ext.push(sec(
            b"PWV4",
            24,
            &words(&[6, color_preview.len() as u32, 0]),
            &color_preview.concat(),
        ));
    }
    ext.extend(
        a.phrases
            .as_ref()
            .filter(|p| !p.list.is_empty()) // an empty list = the user deleted every phrase: no PSSI
            .map(phrase::pssi),
    );

    let mut ex2 = vec![ppth];
    if !a.three_band_detail.is_empty() {
        let n = a.three_band_detail.len() as u32;
        ex2.push(sec(
            b"PWV7",
            24,
            &words(&[3, n, 0x0096_0000]),
            &a.three_band_detail.concat(),
        ));
    }
    if !a.three_band_preview.is_empty() {
        let n = a.three_band_preview.len() as u32;
        ex2.push(sec(
            b"PWV6",
            20,
            &words(&[3, n]),
            &a.three_band_preview.concat(),
        ));
    }
    // PWVC: `0 u2`, then the observed constants 0x50, 0x64, 0x6a.
    ex2.push(sec(b"PWVC", 14, &[0, 0], &[0, 0x50, 0, 0x64, 0, 0x6a]));
    Files {
        dat,
        ext: file(ext),
        ex2: file(ex2),
    }
}

/// `PMAI` container around `sections`; `len_file` is the whole file.
fn file(sections: Vec<Vec<u8>>) -> Vec<u8> {
    let body = sections.concat();
    let mut f = [
        b"PMAI".as_slice(),
        &words(&[28, 28 + body.len() as u32, 1, 0x10000, 0x10000, 0]),
    ]
    .concat();
    f.extend(body);
    f
}

/// Section: `tag, len_header, len_tag`, `fields` (header bytes after the 12-byte tag header), zero padding up to
/// `header`, then `body`.
fn sec(tag: &[u8; 4], header: usize, fields: &[u8], body: &[u8]) -> Vec<u8> {
    let mut s = [
        tag.as_slice(),
        &words(&[header as u32, (header + body.len()) as u32]),
        fields,
    ]
    .concat();
    s.resize(header, 0);
    s.extend_from_slice(body);
    s
}

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn utf16(t: &str) -> Vec<u8> {
    t.encode_utf16()
        .chain([0])
        .flat_map(u16::to_be_bytes)
        .collect()
}

/// `PPTH`: `len_path u4` (bytes incl. NUL), UTF-16BE path + NUL.
fn ppth(path: &str) -> Vec<u8> {
    let p = utf16(path);
    sec(b"PPTH", 16, &words(&[p.len() as u32]), &p)
}

fn beat(b: &Beat) -> [u8; 8] {
    let (p, t) = (b.bar_pos.to_be_bytes(), b.time_ms.to_be_bytes());
    let bpm = b.bpm_x100.to_be_bytes();
    [p[0], p[1], bpm[0], bpm[1], t[0], t[1], t[2], t[3]]
}

/// `PQT2`, the extended beat grid (nxs2/CDJ-3000). No spec in crate-digger, dysentery or rekordcrate; the header layout is
/// pyrekordbox `anlz/structs.py` (`PQT2`, "len_header 56", fields it calls unknown), and what the fields hold was read off the
/// 20 `PQT2` sections of a real rekordbox 7 export (observed, every claim held in all 20, see `.local/re/export/notes.md`):
/// header 56 = `tag, 56, len, 0 u4, 0x01000002 u4, 0 u4`, the first and the last beat as 8-byte `PQTZ` entries, `num_beats u4`,
/// `sum u4` (wrapping sum of `bar_pos + bpm_x100 + time_ms` over all beats), `0 u4, 0 u4`; then one u2 per beat: the
/// fraction of that beat's millisecond in thousandths (0..=999; the beat time is `time_ms + u2 / 1000`).
// ponytail: `Beat` times are whole milliseconds, so every fraction is 0 (a real file has 14..51 zeros per ~600 entries, the
// rest sample-quantised); keep sub-millisecond times in `Beat` and fill the fractions when phase accuracy beyond 1 ms matters.
fn pqt2(beats: &[Beat]) -> Option<Vec<u8>> {
    let (first, last) = (beats.first()?, beats.last()?);
    let sum = beats.iter().fold(0u32, |s, b| {
        s.wrapping_add(u32::from(b.bar_pos))
            .wrapping_add(u32::from(b.bpm_x100))
            .wrapping_add(b.time_ms)
    });
    let fields = [
        words(&[0, 0x0100_0002, 0]),
        beat(first).to_vec(),
        beat(last).to_vec(),
        words(&[beats.len() as u32, sum, 0, 0]),
    ]
    .concat();
    Some(sec(b"PQT2", 56, &fields, &vec![0u8; 2 * beats.len()]))
}

#[cfg(test)]
mod tests {
    use super::wave::{color, mono};
    use super::*;
    use openfader_plugin::library::{Kind as PhraseKind, Mood, PREVIEW_ENTRIES, Phrase, Phrases};
    use rekordbox_common::anlz::{load_track, parse};

    fn cue(hot: u16, time: u32, end: Option<u32>, rgb: [u8; 3], comment: &str) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: time,
            loop_end_ms: end,
            color: Some(rgb),
            comment: (!comment.is_empty()).then(|| comment.into()),
        }
    }

    fn track() -> Analysis {
        let bands = |n: usize| {
            (0..n)
                .map(|i| {
                    [
                        (i % 128) as u8,
                        ((i * 3) % 128) as u8,
                        ((i * 7) % 128) as u8,
                    ]
                })
                .collect()
        };
        Analysis {
            beats: vec![
                Beat {
                    bar_pos: 1,
                    bpm_x100: 12800,
                    time_ms: 100,
                },
                Beat {
                    bar_pos: 2,
                    bpm_x100: 12800,
                    time_ms: 569,
                },
            ],
            cues: vec![
                cue(1, 1000, None, [255, 0, 23], "Intro"),
                cue(2, 900, None, [0, 196, 255], ""),
                cue(3, 3000, Some(4000), [1, 2, 3], "loop"),
                cue(4, 5000, None, [51, 255, 0], "Café ✓ 日本"),
                cue(8, 7000, None, [255, 232, 0], ""),
                cue(0, 2000, None, [10, 20, 30], "mem"),
                cue(0, 8000, Some(9000), [40, 50, 60], ""),
            ],
            three_band_detail: bands(450),
            three_band_preview: bands(PREVIEW_ENTRIES),
            ..Analysis::default()
        }
    }

    /// `(tag, offset, len)` of every section; asserts `len_file` and that sections tile the file.
    fn walk(f: &[u8]) -> Vec<([u8; 4], usize, usize)> {
        assert_eq!(&f[..4], b"PMAI");
        assert_eq!(
            u32::from_be_bytes(f[8..12].try_into().unwrap()) as usize,
            f.len()
        );
        let (mut o, mut out) = (28, vec![]);
        while o < f.len() {
            let len = u32::from_be_bytes(f[o + 8..o + 12].try_into().unwrap()) as usize;
            out.push((f[o..o + 4].try_into().unwrap(), o, len));
            o += len;
        }
        assert_eq!(o, f.len());
        out
    }

    fn tags(f: &[u8]) -> Vec<String> {
        walk(f)
            .iter()
            .map(|s| String::from_utf8_lossy(&s.0).into_owned())
            .collect()
    }

    #[test]
    fn round_trips_through_load_track() {
        let a = track();
        let files = encode(&a, "/Contents/Artist/Album/x.mp3");
        let dir = std::env::temp_dir().join(format!("rk_anlz_enc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ANLZ0000.DAT"), &files.dat).unwrap();
        std::fs::write(dir.join("ANLZ0000.EXT"), &files.ext).unwrap();
        std::fs::write(dir.join("ANLZ0000.2EX"), &files.ex2).unwrap();
        let back = load_track(&dir.join("ANLZ0000.DAT"));
        let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
        let back = back.unwrap();
        assert_eq!(back.path.as_deref(), Some("/Contents/Artist/Album/x.mp3"));
        assert_eq!(back.beats, a.beats);
        let mut want = a.cues.clone();
        want.sort_by_key(|c| (c.time_ms, c.hot_cue));
        let mut got = back.cues.clone();
        got.sort_by_key(|c| (c.time_ms, c.hot_cue));
        assert_eq!(got, want);
        assert_eq!(back.three_band_detail, a.three_band_detail);
        assert_eq!(back.three_band_preview, a.three_band_preview);
        assert_eq!(back.vocal_thresholds, Some([0x50, 0x64, 0x6a]));
    }

    /// Header and checksum as observed in real files: first/last beat, count, sum of every beat's three fields.
    #[test]
    fn pqt2_header_sum_and_entries() {
        let beats = vec![
            Beat {
                bar_pos: 1,
                bpm_x100: 14800,
                time_ms: 21,
            },
            Beat {
                bar_pos: 2,
                bpm_x100: 14800,
                time_ms: 427,
            },
            Beat {
                bar_pos: 3,
                bpm_x100: 14800,
                time_ms: 832,
            },
        ];
        let s = pqt2(&beats).unwrap();
        let u32_at = |o: usize| u32::from_be_bytes(s[o..o + 4].try_into().unwrap());
        assert_eq!(
            (&s[..4], u32_at(4), u32_at(8) as usize, s.len()),
            (b"PQT2".as_slice(), 56, 56 + 6, 62)
        );
        assert_eq!((u32_at(12), u32_at(16), u32_at(20)), (0, 0x0100_0002, 0));
        assert_eq!(s[24..32], beat(&beats[0]));
        assert_eq!(s[32..40], beat(&beats[2]));
        assert_eq!(u32_at(40), 3);
        assert_eq!(u32_at(44), (1 + 2 + 3) + 3 * 14800 + (21 + 427 + 832));
        assert_eq!((u32_at(48), u32_at(52)), (0, 0));
        assert_eq!(s[56..], [0; 6]);
        assert!(pqt2(&[]).is_none());
    }

    #[test]
    fn section_order_and_lengths() {
        let f = encode(&track(), "/Contents/a.mp3");
        assert_eq!(
            tags(&f.dat),
            ["PPTH", "PVBR", "PQTZ", "PWAV", "PWV2", "PCOB", "PCOB"]
        );
        assert_eq!(
            tags(&f.ext),
            [
                "PPTH", "PWV3", "PCOB", "PCOB", "PCO2", "PCO2", "PQT2", "PWV5", "PWV4"
            ]
        );
        assert_eq!(tags(&f.ex2), ["PPTH", "PWV7", "PWV6", "PWVC"]);
        let pvbr = walk(&f.dat)[1];
        assert_eq!(
            (
                pvbr.2,
                u32::from_be_bytes(f.dat[pvbr.1 + 4..pvbr.1 + 8].try_into().unwrap())
            ),
            (1620, 16)
        );
        // Nothing to derive from: only PWAV/PWV2 (zeros) stay.
        let empty = encode(&Analysis::default(), "/x.mp3");
        assert_eq!(tags(&empty.ext), ["PPTH", "PCOB", "PCOB", "PCO2", "PCO2"]);
        assert_eq!(
            tags(&empty.dat),
            ["PPTH", "PVBR", "PQTZ", "PWAV", "PWV2", "PCOB", "PCOB"]
        );
        assert_eq!(tags(&empty.ex2), ["PPTH", "PWVC"]);
    }

    #[test]
    fn hot_cue_split_and_waveform_lengths() {
        let f = encode(&track(), "/Contents/a.mp3");
        let count = |file: &[u8], i: usize, at: usize| {
            let s = walk(file)[i];
            u16::from_be_bytes(file[s.1 + at..s.1 + at + 2].try_into().unwrap())
        };
        assert_eq!((count(&f.dat, 5, 18), count(&f.dat, 6, 18)), (3, 2)); // hot 1..3; memory
        assert_eq!((count(&f.ext, 2, 18), count(&f.ext, 3, 18)), (2, 2)); // hot 4, 8; memory
        assert_eq!((count(&f.ext, 4, 16), count(&f.ext, 5, 16)), (5, 2)); // PCO2 all hot; memory
        let dat = parse(&f.dat).unwrap();
        let ext = parse(&f.ext).unwrap();
        let ex2 = parse(&f.ex2).unwrap();
        assert_eq!(dat.preview.len(), 400);
        assert_eq!(walk(&f.dat)[4].2, 20 + 100);
        assert_eq!(ext.detail.len(), 450);
        assert_eq!(ext.color_preview.len(), 1200);
        assert_eq!(ext.color_detail.len(), 450);
        assert_eq!(ex2.three_band_preview.len(), 1200);
        assert_eq!(ex2.three_band_detail.len(), 450);
        // [l,m,h] = [127,0,0] -> height 31, whiteness 0; PWV5 red 7, height 31.
        assert_eq!(mono(&[127, 0, 0]), 31);
        assert_eq!(color(&[127, 0, 0]), 7 << 13 | 31 << 2);
        assert_eq!(mono(&[0, 0, 0]), 0);
    }

    #[test]
    fn pssi_round_trips_for_every_mood() {
        let ph = |beat, end_beat, kind, variant, fill| Phrase {
            beat,
            end_beat,
            kind,
            variant,
            fill,
        };
        let moods = [
            Phrases {
                mood: Mood::High,
                list: vec![
                    ph(1, 17, PhraseKind::Intro, 2, None),
                    ph(17, 33, PhraseKind::Up, 3, Some(31)),
                    ph(33, 49, PhraseKind::Down, 0, None),
                    ph(49, 65, PhraseKind::Chorus, 1, None),
                    ph(65, 70, PhraseKind::Up, 2, None),
                ],
            },
            Phrases {
                mood: Mood::Mid,
                list: vec![
                    ph(1, 9, PhraseKind::Intro, 0, None),
                    ph(9, 25, PhraseKind::Verse, 5, Some(23)),
                    ph(25, 41, PhraseKind::Bridge, 0, None),
                    ph(41, 57, PhraseKind::Chorus, 0, None),
                    ph(57, 65, PhraseKind::Outro, 0, None),
                ],
            },
            Phrases {
                mood: Mood::Low,
                list: vec![
                    ph(1, 9, PhraseKind::Verse, 1, None),
                    ph(9, 17, PhraseKind::Verse, 2, None),
                ],
            },
        ];
        for p in moods {
            let a = Analysis {
                phrases: Some(p.clone()),
                ..track()
            };
            let f = encode(&a, "/x.mp3");
            assert_eq!(*tags(&f.ext).last().unwrap(), "PSSI");
            assert_eq!(parse(&f.ext).unwrap().phrases, Some(p));
        }
    }
}
