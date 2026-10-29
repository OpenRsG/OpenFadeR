//! Stem files: an MP4 (`.stem.mp4` / `.stem.m4a`) holding the stereo mix as its first audio track and four stem tracks
//! after it (usually AAC), with the part names in a JSON box.
//!
//! Format facts, from the public stem-file description and labelled inferred where we rely on convention rather than a
//! checked rule: the JSON sits as the payload of a `stem` box inside `moov/udta` (inferred); it holds a `stems` array of four
//! objects with a `name` (and a `color`, which we ignore: the deck's part colours are the theme's); the four stem tracks follow
//! the mix in the order of that array, conventionally drums, bass, melody (other instruments), vocals (inferred). symphonia
//! lists every `trak` of the MP4 and tags each packet with its track, so no box reading beyond the JSON is needed (ours).
//!
//! The parts are mapped onto our four stems ([`VOCAL`], [`INST`], [`BASS`], [`DRUMS`]) by their names (see [`slot_of`]); when
//! the names do not give each of the four exactly once, by the conventional order above.

use super::decode::{open_format, stereo};
use super::{BASS, DRUMS, INST, STEM_COUNT, VOCAL};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymErr;

/// The conventional order of the four tracks after the mix, as our stem indices.
const CONVENTION: [usize; STEM_COUNT] = [DRUMS, BASS, INST, VOCAL];
/// The JSON of a real file is a few hundred bytes; anything larger is not one.
const MAX_JSON: u64 = 1 << 20;

/// What a stem file says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// The four part names, in the order of the file's tracks after the mix.
    pub names: [String; STEM_COUNT],
    /// `slot[k]` = our stem ([`VOCAL`]…[`DRUMS`]) that the file's part `k` feeds.
    pub slot: [usize; STEM_COUNT],
}

impl Info {
    /// The file's name for our stem `stem`.
    pub fn name_of(&self, stem: usize) -> &str {
        let k = self.slot.iter().position(|&s| s == stem).unwrap_or(0);
        &self.names[k]
    }

    /// The names in the file's order: "Drums · Bass · Melody · Vocals".
    pub fn label(&self) -> String {
        self.names.join(" · ")
    }
}

/// Whether the file name says stem file (`.stem.mp4` / `.stem.m4a`): the browser's badge, no file access.
pub fn is_stem_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|n| n.ends_with(".stem.mp4") || n.ends_with(".stem.m4a"))
}

/// Our stem a part called `name` feeds: vocals (vocal, vox, voice, acapella), drums (drum, perc), bass, else instruments.
pub fn slot_of(name: &str) -> usize {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["vocal", "vox", "voice", "sing", "acap"]) {
        VOCAL
    } else if has(&["drum", "perc", "kick"]) {
        DRUMS
    } else if has(&["bass"]) {
        BASS
    } else {
        INST
    }
}

/// [`Info`] of the JSON a `stem` box holds; `None` unless it names exactly four parts.
fn parse(json: &[u8]) -> Option<Info> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let parts = v.get("stems")?.as_array()?;
    if parts.len() != STEM_COUNT {
        return None;
    }
    let names: [String; STEM_COUNT] = std::array::from_fn(|k| {
        parts[k]
            .get("name")
            .and_then(|n| n.as_str())
            .map_or_else(|| format!("Stem {}", k + 1), str::to_owned)
    });
    let by_name: [usize; STEM_COUNT] = std::array::from_fn(|k| slot_of(&names[k]));
    let each_once = (0..STEM_COUNT).all(|s| by_name.contains(&s));
    Some(Info {
        slot: if each_once { by_name } else { CONVENTION },
        names,
    })
}

/// The payload of the first box at `path` (box types from `from`'s level down) inside `from..to`, scanning every sibling.
fn find(r: &mut (impl Read + Seek), mut from: u64, to: u64, path: &[&[u8; 4]]) -> Option<Vec<u8>> {
    while from + 8 <= to {
        r.seek(SeekFrom::Start(from)).ok()?;
        let mut h = [0u8; 8];
        r.read_exact(&mut h).ok()?;
        let (size32, kind) = (u32::from_be_bytes(h[..4].try_into().ok()?), &h[4..8]);
        let (head, size) = match size32 {
            0 => (8, to - from),
            1 => {
                let mut big = [0u8; 8];
                r.read_exact(&mut big).ok()?;
                (16, u64::from_be_bytes(big))
            }
            n => (8, u64::from(n)),
        };
        if size < head || from + size > to {
            return None;
        }
        if kind == path[0] {
            if path.len() == 1 {
                let len = size - head;
                if len > MAX_JSON {
                    return None;
                }
                let mut out = vec![0; len as usize];
                r.read_exact(&mut out).ok()?;
                return Some(out);
            }
            if let Some(found) = find(r, from + head, from + size, &path[1..]) {
                return Some(found);
            }
        }
        from += size;
    }
    None
}

/// [`Info`] of an MP4 read from `r` (`len` bytes), `None` when it has no `moov/udta/stem` box naming four parts.
fn read_from(r: &mut (impl Read + Seek), len: u64) -> Option<Info> {
    parse(&find(r, 0, len, &[b"moov", b"udta", b"stem"])?)
}

/// What the file at `path` says about its parts: reads only the box headers down to the JSON, never the audio. `None` for
/// anything that is not a stem file, and for a file that is not an MP4 by its extension (it is not opened at all).
pub fn read_info(path: &Path) -> Option<Info> {
    if !crate::paths::has_extension(path, &["mp4", "m4a", "m4v"]) {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    read_from(&mut std::io::BufReader::new(&mut f), len)
}

/// A sample as the stems store it: ±1.0 is ±32768 (not [`Track16`](super::Track16)'s ±2.0 range).
fn to_i16(x: f32) -> i16 {
    (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

/// The four parts of `path` decoded to interleaved stereo 16-bit at the mix's rate, as our stems (index [`VOCAL`]…[`DRUMS`],
/// by `info.slot`), each cut or padded with silence to `frames` frames (the mix's length: every part shares the mix's read
/// position). `progress` gets 0..1 as the file is read; `cancelled` is checked between packets. Needs the mix and four more
/// audio tracks of the mix's sample rate.
pub fn decode_parts(
    path: &Path,
    info: &Info,
    frames: usize,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(f32),
) -> Result<[Vec<i16>; STEM_COUNT], String> {
    let mut fmt = open_format(path)?;
    let tracks: Vec<_> = fmt
        .tracks()
        .iter()
        .filter(|t| t.codec_params.sample_rate.is_some() && t.codec_params.codec != CODEC_TYPE_NULL)
        .map(|t| (t.id, t.codec_params.clone()))
        .collect();
    if tracks.len() < 1 + STEM_COUNT {
        return Err(format!(
            "stem file: {} audio track(s), the mix and {STEM_COUNT} parts are needed",
            tracks.len()
        ));
    }
    let rate = tracks[0].1.sample_rate;
    let mut parts: Vec<(u32, Box<dyn Decoder>)> = Vec::with_capacity(STEM_COUNT);
    for (id, params) in &tracks[1..=STEM_COUNT] {
        if params.sample_rate != rate {
            return Err("stem file: the parts and the mix differ in sample rate".into());
        }
        let dec = symphonia::default::get_codecs()
            .make(params, &DecoderOptions::default())
            .map_err(|e| format!("codec: {e}"))?;
        parts.push((*id, dec));
    }
    let capacity = frames
        .checked_mul(2)
        .ok_or("stem file: output size overflow")?;
    let mut out: [Vec<i16>; STEM_COUNT] = std::array::from_fn(|_| Vec::with_capacity(capacity));
    let mut seen = [false; STEM_COUNT];
    let mut bufs: [Option<SampleBuffer<f32>>; STEM_COUNT] = std::array::from_fn(|_| None);
    loop {
        if cancelled() {
            return Err("cancelled".into());
        }
        let pkt = match fmt.next_packet() {
            Ok(p) => p,
            Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymErr::ResetRequired) => break,
            Err(e) => return Err(format!("read: {e}")),
        };
        let Some(k) = parts.iter().position(|(id, _)| *id == pkt.track_id()) else {
            continue;
        };
        let decoded = match parts[k].1.decode(&pkt) {
            Ok(d) => d,
            Err(SymErr::DecodeError(_)) => continue, // a corrupt frame: skipped, as the mix's decode does
            Err(e) => return Err(format!("decode: {e}")),
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        let buf = bufs[k].get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        buf.copy_interleaved_ref(decoded);
        seen[k] |= ch > 0 && buf.samples().len() >= ch;
        let remaining = capacity - out[k].len();
        out[k].extend(
            stereo(buf.samples(), ch)
                .flatten()
                .take(remaining)
                .map(to_i16),
        );
        if k == 0 && frames > 0 {
            progress((out[0].len() / 2) as f32 / frames as f32);
        }
    }
    if seen.iter().any(|&seen| !seen) {
        return Err("stem file: a part holds no audio".into());
    }
    let mut by_stem: [Vec<i16>; STEM_COUNT] = std::array::from_fn(|_| Vec::new());
    for (k, mut part) in out.into_iter().enumerate() {
        part.resize(capacity, 0);
        by_stem[info.slot[k]] = part;
    }
    Ok(by_stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        b.extend(kind);
        b.extend(payload);
        b
    }

    /// An MP4 skeleton: a `free` box, then `moov` with a `trak` (so the scan has to skip siblings) and `udta/stem`.
    fn skeleton(json: &str) -> Vec<u8> {
        let udta = boxed(b"udta", &boxed(b"stem", json.as_bytes()));
        let moov = boxed(b"moov", &[boxed(b"trak", &[0; 12]), udta].concat());
        [boxed(b"free", &[0; 5]), moov].concat()
    }

    fn info_of(json: &str) -> Option<Info> {
        let file = skeleton(json);
        read_from(&mut Cursor::new(&file), file.len() as u64)
    }

    #[test]
    fn names_decide_which_stem_a_part_feeds() {
        let info = info_of(r#"{"stems":[{"name":"Vox"},{"name":"Kick & Percussion"},{"name":"Keys"},{"name":"Sub Bass"}]}"#)
            .unwrap();
        assert_eq!(info.slot, [VOCAL, DRUMS, INST, BASS]);
        assert_eq!(info.name_of(DRUMS), "Kick & Percussion");
        assert_eq!(info.label(), "Vox · Kick & Percussion · Keys · Sub Bass");
    }

    #[test]
    fn names_that_do_not_cover_the_four_stems_fall_back_to_the_conventional_order() {
        let info =
            info_of(r#"{"stems":[{"name":"A"},{"name":"B"},{"name":"C"},{"name":"D"}]}"#).unwrap();
        assert_eq!(info.slot, [DRUMS, BASS, INST, VOCAL]);
        // a missing name is numbered; two drum parts are not a mapping
        let info = info_of(r#"{"stems":[{"name":"Drums"},{"name":"Drum 2"},{},{"name":"Vocal"}]}"#)
            .unwrap();
        assert_eq!(
            (info.slot, info.names[2].as_str()),
            ([DRUMS, BASS, INST, VOCAL], "Stem 3")
        );
    }

    #[test]
    fn only_a_box_with_four_named_parts_is_a_stem_file() {
        assert!(info_of(r#"{"stems":[{"name":"a"},{"name":"b"}]}"#).is_none());
        assert!(info_of("not json").is_none());
        let plain = [boxed(b"moov", &boxed(b"udta", &boxed(b"meta", b"x")))].concat();
        assert!(read_from(&mut Cursor::new(&plain), plain.len() as u64).is_none());
        // a box that claims more than the file holds ends the scan instead of reading past it
        let mut cut = skeleton(r#"{"stems":[]}"#);
        cut[0] = 0x7f;
        assert!(read_from(&mut Cursor::new(&cut), cut.len() as u64).is_none());
    }

    #[test]
    fn file_names_mark_stem_files() {
        assert!(is_stem_name(Path::new("/m/Song.STEM.mp4")));
        assert!(is_stem_name(Path::new("a.stem.m4a")));
        assert!(!is_stem_name(Path::new("a.mp4")));
        assert!(!is_stem_name(Path::new("stem.m4a")));
    }

    /// `tests/data/four-parts.stem.mp4`: 1.5 s of AAC, the mix and four parts, one sine each (drums 220 Hz, bass 110, melody
    /// 880, vocals 1760; the mix holds all four), made with ffmpeg (five `sine` inputs, `amix` for the mix, `-map` per
    /// track, `-c:a aac`) with the `stem` box appended inside `moov` (our own JSON).
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/four-parts.stem.mp4"
    );

    /// Energy of the left channel of `x` (interleaved stereo, 44.1 kHz) at `hz`: projection onto a sine and a cosine.
    fn energy_at(x: &[f32], hz: f64) -> f64 {
        let (mut s, mut c, n) = (0.0, 0.0, x.len() / 2);
        for i in 0..n {
            let (a, v) = (
                std::f64::consts::TAU * hz * i as f64 / 44_100.0,
                f64::from(x[2 * i]),
            );
            s += v * a.sin();
            c += v * a.cos();
        }
        (s * s + c * c) / (n * n) as f64
    }

    #[test]
    fn each_part_of_a_stem_file_holds_its_own_signal() {
        let path = Path::new(FIXTURE);
        let info = read_info(path).expect("the fixture is a stem file");
        assert_eq!(info.label(), "Drums · Bass · Melody · Vocals");
        let (mix, _) = crate::audio::decode_track16_cancellable(path, &|| false).unwrap();
        let parts = decode_parts(path, &info, mix.frames(), &|| false, &|_| {})
            .unwrap()
            .map(|p| {
                p.iter()
                    .map(|&v| f32::from(v) / 32768.0)
                    .collect::<Vec<f32>>()
            });
        let tones = [
            (VOCAL, 1760.0),
            (INST, 880.0),
            (BASS, 110.0),
            (DRUMS, 220.0),
        ];
        for (stem, hz) in tones {
            assert_eq!(parts[stem].len(), 2 * mix.frames());
            let own = energy_at(&parts[stem], hz);
            assert!(own > 1e-3, "stem {stem} holds {hz} Hz: {own}");
            for (other, other_hz) in tones.into_iter().filter(|t| t.0 != stem) {
                let leak = energy_at(&parts[stem], other_hz);
                assert!(
                    leak < own * 1e-3,
                    "stem {stem} leaks the {other_hz} Hz of stem {other}: {leak} vs {own}"
                );
            }
        }
        // the first audio track is the mix: it plays as the track and holds every part's tone
        let mix: Vec<f32> = mix.iter_f32(0..mix.frames()).collect();
        assert!(tones.iter().all(|&(_, hz)| energy_at(&mix, hz) > 1e-4));
    }

    #[test]
    fn a_file_with_fewer_tracks_than_a_stem_file_is_refused() {
        let wav = std::env::temp_dir().join(format!("rk-stemfile-{}.wav", std::process::id()));
        let silence = crate::audio::Pcm {
            rate: 44_100,
            samples: vec![0.0; 2000],
        };
        crate::audio::write_wav(&wav, &silence, 44_100).unwrap();
        let info =
            info_of(r#"{"stems":[{"name":"a"},{"name":"b"},{"name":"c"},{"name":"d"}]}"#).unwrap();
        let err = decode_parts(&wav, &info, 1000, &|| false, &|_| {}).unwrap_err();
        std::fs::remove_file(&wav).ok();
        assert!(err.contains("1 audio track"), "{err}");
    }

    #[test]
    fn parts_are_bounded_to_the_mix_length_and_short_parts_are_zero_padded() {
        let path = Path::new(FIXTURE);
        let info = read_info(path).unwrap();
        let (mix, _) = crate::audio::decode_track16_cancellable(path, &|| false).unwrap();
        let whole = decode_parts(path, &info, mix.frames(), &|| false, &|_| {}).unwrap();
        let frames = mix.frames() / 2;
        let short = decode_parts(path, &info, frames, &|| false, &|_| {}).unwrap();
        let padded = decode_parts(path, &info, mix.frames() + 100, &|| false, &|_| {}).unwrap();
        for k in 0..STEM_COUNT {
            assert_eq!(short[k], whole[k][..2 * frames]);
            assert_eq!(
                short[k].capacity(),
                2 * frames,
                "long parts must not grow beyond the reservation"
            );
            assert_eq!(&padded[k][..whole[k].len()], whole[k]);
            assert!(padded[k][whole[k].len()..].iter().all(|&v| v == 0));
        }
        assert!(
            decode_parts(path, &info, 0, &|| false, &|_| {})
                .unwrap()
                .iter()
                .all(Vec::is_empty)
        );
    }
}
