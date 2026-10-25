//! Rekordbox analysis files (`ANLZ*.DAT`, `ANLZ*.EXT`, `ANLZ*.2EX`). Big-endian `PMAI` container of
//! tagged sections: `fourcc, header_len u32, section_len u32, ...`.
//!
//! Layouts follow the public Deep Symmetry crate-digger spec, pinned at commit
//! `7c9d5358d8a3ce663f566380f2dc79ab1393201e`:
//! <https://github.com/Deep-Symmetry/crate-digger/blob/7c9d5358d8a3ce663f566380f2dc79ab1393201e/src/main/kaitai/rekordbox_anlz.ksy>
//! and `doc/modules/ROOT/pages/anlz.adoc` (PWV5 bit layout) in the same commit.
//! `.DAT`, `.EXT` and `.2EX` layouts were additionally checked against a real 24-track USB export
//! (`rekordbox-device/tests/anlz.rs`): section/header lengths, entry sizes, counts and cue fields all matched.
//!
//! This base owns the container walker, the byte readers, [`parse`] and [`load_track`]; the result is the SDK's
//! [`Analysis`]. Children, one per tag family:
//! - [`beatgrid`]: `PQTZ`
//! - [`cues`]: `PCOB`/`PCPT`, `PCO2`/`PCP2`
//! - [`waveform`]: `PWAV`, `PWV3`..`PWV7`, `PWVC`
//! - [`phrase`]: `PSSI` song structure (masked phrases + mood)

mod beatgrid;
pub mod cues;
mod phrase;
mod waveform;

pub use openfader_plugin::library::{
    Analysis, Beat, Cue, Kind as PhraseKind, Mood, Phrase, Phrases,
};

pub(crate) fn u16_at(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| format!("short read at {o}"))
}
pub(crate) fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("short read at {o}"))
}
/// Big-endian UTF-16 text, trailing NULs trimmed.
pub(crate) fn utf16_be(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_owned()
}

/// Parses one analysis file (any of the three kinds): the sections it holds fill the matching parts of the result.
pub fn parse(b: &[u8]) -> Result<Analysis, String> {
    if b.get(..4) != Some(b"PMAI") {
        return Err("not a PMAI file".into());
    }
    let declared = u32_at(b, 8)? as usize;
    if declared > b.len() {
        return Err("PMAI truncated: declared length exceeds file size".into());
    }
    let b = &b[..declared];
    let mut a = Analysis::default();
    let mut ext_cues = Vec::new();
    let mut o = u32_at(b, 4)? as usize;
    while o < b.len() {
        let tag = b.get(o..o + 4).ok_or("truncated tag")?;
        let header = u32_at(b, o + 4)? as usize;
        let len = u32_at(b, o + 8)? as usize;
        if len < 12 || header > len || o + len > b.len() {
            return Err(format!("bad section length at {o}"));
        }
        let s = &b[o..o + len];
        match tag {
            b"PPTH" => {
                let n = u32_at(s, 12)? as usize;
                a.path = Some(utf16_be(s.get(16..16 + n).ok_or("PPTH overflow")?));
            }
            b"PQTZ" => a.beats = beatgrid::parse(s, header)?,
            b"PCOB" => a.cues.extend(cues::parse_pcob(s, header)?),
            b"PCO2" => ext_cues.extend(cues::parse_pco2(s, header)?),
            b"PWAV" | b"PWV3" | b"PWV4" | b"PWV5" | b"PWV6" | b"PWV7" | b"PWVC" => {
                waveform::parse(&mut a, tag, s, header)?
            }
            b"PSSI" => a.phrases = Some(phrase::parse(s, header)?),
            _ => {}
        }
        o += len;
    }
    cues::merge_cues(&mut a.cues, ext_cues);
    Ok(a)
}

/// Loads `ANLZ0000.DAT` and, when present, merges the sibling `.EXT` and `.2EX` sections.
pub fn load_track(dat: &std::path::Path) -> Result<Analysis, String> {
    let read = |p: &std::path::Path| crate::at(std::fs::read(p), p);
    let mut a = parse(&read(dat)?)?;
    let ext = dat.with_extension("EXT");
    if ext.exists() {
        let x = parse(&read(&ext)?)?;
        a.detail = x.detail;
        a.color_preview = x.color_preview;
        a.color_detail = x.color_detail;
        a.phrases = x.phrases;
        cues::merge_cues(&mut a.cues, x.cues);
    }
    let ex2 = dat.with_extension("2EX");
    if ex2.exists() {
        let x = parse(&read(&ex2)?)?;
        a.three_band_preview = x.three_band_preview;
        a.three_band_detail = x.three_band_detail;
        a.vocal_thresholds = x.vocal_thresholds;
    }
    Ok(a)
}

/// How many cues (hot, memory, loops) the bytes of a `.DAT` hold, with those of its `.EXT` when given.
pub fn count_cues(dat: &[u8], ext: Option<&[u8]>) -> Result<usize, String> {
    let mut a = parse(dat)?;
    if let Some(x) = ext {
        cues::merge_cues(&mut a.cues, parse(x)?.cues);
    }
    Ok(a.cues.len())
}

/// Synthetic-file builders shared by the child tests.
#[cfg(test)]
mod testutil {
    pub fn sec(tag: &[u8; 4], header: usize, body: &[u8]) -> Vec<u8> {
        let mut s = [
            tag.as_slice(),
            &(header as u32).to_be_bytes(),
            &((header + body.len()) as u32).to_be_bytes(),
        ]
        .concat();
        s.resize(header, 0);
        s.extend_from_slice(body);
        s
    }
    pub fn pmai(body: &[u8]) -> Vec<u8> {
        let mut f = [
            b"PMAI".as_slice(),
            &28u32.to_be_bytes(),
            &(28 + body.len() as u32).to_be_bytes(),
        ]
        .concat();
        f.resize(28, 0);
        f.extend_from_slice(body);
        f
    }
    /// Wave tag: header 24 = 12 + entry_bytes u4 + n u4 + u4.
    pub fn wave(tag: &[u8; 4], eb: u32, n: u32, data: &[u8]) -> Vec<u8> {
        let mut s = sec(tag, 24, data);
        s[12..16].copy_from_slice(&eb.to_be_bytes());
        s[16..20].copy_from_slice(&n.to_be_bytes());
        s
    }
    /// `.2EX` body: PWV6 header 20 (no unknown u4), PWV7 header 24, PWVC header 14 + three u16 (layouts as observed on a real export).
    pub fn three_band_body() -> Vec<u8> {
        let mut pwv6 = sec(b"PWV6", 20, &[1, 2, 3, 4, 5, 6]);
        pwv6[12..16].copy_from_slice(&3u32.to_be_bytes());
        pwv6[16..20].copy_from_slice(&2u32.to_be_bytes());
        let pwvc = sec(b"PWVC", 14, &[0, 80, 0, 90, 0, 100]);
        [pwv6, wave(b"PWV7", 3, 1, &[7, 8, 9]), pwvc].concat()
    }
    fn utf16(t: &str) -> Vec<u8> {
        t.encode_utf16()
            .chain([0])
            .flat_map(u16::to_be_bytes)
            .collect()
    }
    /// PCP2 entry per spec: hot_cue u4@12, type u1@16, time@20, loop@24, color_id@28, comment len@40.
    pub fn pcp2(hot: u32, ty: u8, time: u32, lp: u32, comment: &str, rgb: [u8; 3]) -> Vec<u8> {
        let c = utf16(comment);
        let mut e = vec![0u8; 44];
        e[12..16].copy_from_slice(&hot.to_be_bytes());
        e[16] = ty;
        e[20..24].copy_from_slice(&time.to_be_bytes());
        e[24..28].copy_from_slice(&lp.to_be_bytes());
        e[40..44].copy_from_slice(&(c.len() as u32).to_be_bytes());
        e.extend(&c);
        e.extend([1, rgb[0], rgb[1], rgb[2]]);
        e.extend([0; 4]);
        e[..4].copy_from_slice(b"PCP2");
        e[4..8].copy_from_slice(&16u32.to_be_bytes());
        let n = e.len() as u32;
        e[8..12].copy_from_slice(&n.to_be_bytes());
        e
    }
    pub fn pco2(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut s = sec(b"PCO2", 20, &entries.concat());
        s[12..16].copy_from_slice(&1u32.to_be_bytes());
        s[16..18].copy_from_slice(&(entries.len() as u16).to_be_bytes());
        s
    }
}

#[cfg(test)]
mod tests {
    use super::{testutil::*, *};

    #[test]
    fn pmai_length_trailing_and_truncated() {
        let mut f = pmai(&sec(b"PWAV", 20, &[1, 2, 3]));
        f.extend([0xAA; 5]); // trailing junk beyond declared length
        assert!(parse(&f).is_ok());
        assert!(parse(&f[..f.len() - 6]).is_err());
        assert!(parse(&f[..10]).is_err());
        assert!(parse(b"nope").is_err());
    }

    #[test]
    fn load_track_merges_ext_and_2ex() {
        let dir = std::env::temp_dir().join(format!("rk_anlz_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dat = dir.join("ANLZ0000.DAT");
        std::fs::write(&dat, pmai(&sec(b"PWAV", 20, &[9]))).unwrap();
        assert!(load_track(&dat).unwrap().detail.is_empty()); // no .EXT yet
        let ext = [
            wave(b"PWV3", 1, 2, &[7, 8]),
            pco2(&[pcp2(3, 1, 42, 0, "hi", [1, 2, 3])]),
        ]
        .concat();
        std::fs::write(dir.join("ANLZ0000.EXT"), pmai(&ext)).unwrap();
        std::fs::write(dir.join("ANLZ0000.2EX"), pmai(&three_band_body())).unwrap();
        let a = load_track(&dat).unwrap();
        let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
        assert_eq!(a.detail, [7, 8]);
        assert_eq!(a.cues[0].comment.as_deref(), Some("hi"));
        assert_eq!(a.three_band_detail, [[7, 8, 9]]);
        assert_eq!(a.vocal_thresholds, Some([80, 90, 100]));
    }
}
