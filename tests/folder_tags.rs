//! Folder library tags for MP3 (ID3v2.3/2.4), FLAC (Vorbis comments) and M4A (ilst), on files whose bytes
//! are built here: no encoder or other host tool is involved.
mod common;

use openfader::library::{Track, open};

fn track(name: &str, bytes: &[u8]) -> Track {
    let root = common::fresh_dir(&format!("tags-{name}"));
    std::fs::write(root.join(name), bytes).unwrap();
    let t = open(&root).unwrap().tracks(None).unwrap().remove(0);
    std::fs::remove_dir_all(&root).unwrap();
    t
}

// MP3

fn synchsafe(n: usize) -> [u8; 4] {
    [
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]
}

/// ID3v2.`ver` tag (3 or 4) with the given (frame id, payload) pairs; v2.4 sizes are synchsafe.
fn id3(ver: u8, frames: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, data) in frames {
        body.extend(id.as_bytes());
        if ver == 4 {
            body.extend(synchsafe(data.len()));
        } else {
            body.extend((data.len() as u32).to_be_bytes());
        }
        body.extend([0, 0]);
        body.extend(data);
    }
    let mut b = b"ID3".to_vec();
    b.extend([ver, 0, 0]);
    b.extend(synchsafe(body.len()));
    b.extend(body);
    b
}

fn text(enc: u8, s: &[u8]) -> Vec<u8> {
    [&[enc][..], s].concat()
}

fn comm(s: &str) -> Vec<u8> {
    [&[0u8][..], b"eng", &[0], s.as_bytes()].concat() // latin-1, empty short description
}

/// `tag` + 77 silent MPEG-1 Layer III frames (128 kbit/s, 44.1 kHz, stereo = 417 bytes each) = 2.01 s.
fn mp3(tag: Vec<u8>) -> Vec<u8> {
    let mut b = tag;
    for _ in 0..77 {
        b.extend([0xFF, 0xFB, 0x90, 0x00]);
        b.extend([0u8; 413]);
    }
    b
}

#[test]
fn mp3_id3v23() {
    let utf16 = [
        &[0xFF, 0xFE][..],
        &"Ärtist"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    ]
    .concat();
    let t = track(
        "v23.mp3",
        &mp3(id3(
            3,
            &[
                ("TIT2", text(0, b"Caf\xe9 Title")), // latin-1
                ("TALB", text(0, b"Alb\xe9m")),
                ("TPE1", text(1, &utf16)),
                ("TCON", text(0, b"(17)Rock")),
                ("TBPM", text(0, b"128")),
                ("TKEY", text(0, b"Am")),
                ("COMM", comm("a comment")),
            ],
        )),
    );
    assert_eq!(
        (
            t.title.as_str(),
            t.artist.as_str(),
            t.album.as_str(),
            t.genre.as_str()
        ),
        ("Café Title", "Ärtist", "Albém", "Rock")
    );
    assert_eq!(
        (t.bpm_x100, t.key.as_str(), t.comment.as_str()),
        (12800, "Am", "a comment")
    );
    assert_eq!((t.length_s, t.sample_rate), (2, 44100));
}

#[test]
fn mp3_id3v24_multi_values_and_decimals() {
    let t = track(
        "v24.mp3",
        &mp3(id3(
            4,
            &[
                ("TIT2", text(3, "Title ✓".as_bytes())),
                ("TALB", text(3, "Album ✓".as_bytes())),
                ("TPE1", text(3, b"Artist A\0Artist B")),
                ("TCON", text(3, b"Tech House\0Techno")),
                ("TBPM", text(3, b"127.5")),
                ("TKEY", text(3, b"8A")), // Camelot stays as written
                ("COMM", comm("note")),
            ],
        )),
    );
    assert_eq!(
        (
            t.title.as_str(),
            t.artist.as_str(),
            t.album.as_str(),
            t.genre.as_str()
        ),
        ("Title ✓", "Artist A, Artist B", "Album ✓", "Tech House")
    );
    assert_eq!(
        (t.bpm_x100, t.key.as_str(), t.comment.as_str()),
        (12750, "8A", "note")
    );
    assert_eq!(t.length_s, 2);
}

#[test]
fn mp3_edge_values() {
    // TBPM "0" = unknown; "(13)" is an unresolved ID3v1 index; no TIT2 → file stem; musical key kept.
    let t = track(
        "edge.mp3",
        &mp3(id3(
            3,
            &[
                ("TBPM", text(0, b"0")),
                ("TCON", text(0, b"(13)")),
                ("TKEY", text(0, b"F#m")),
            ],
        )),
    );
    assert_eq!(
        (
            t.title.as_str(),
            t.genre.as_str(),
            t.bpm_x100,
            t.key.as_str()
        ),
        ("edge", "", 0, "F#m")
    );
    // Decimal comma and a unit suffix; a later frame fills a zero BPM.
    let t = track(
        "comma.mp3",
        &mp3(id3(
            3,
            &[("TBPM", text(0, b"0")), ("TBPM", text(0, b"174,5 BPM"))],
        )),
    );
    assert_eq!(t.bpm_x100, 17450);
}

// FLAC

fn crc8(d: &[u8]) -> u8 {
    d.iter().fold(0, |c, &b| {
        (0..8).fold(c ^ b, |c, _| {
            if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            }
        })
    })
}

fn crc16(d: &[u8]) -> u16 {
    d.iter().fold(0, |c, &b| {
        (0..8).fold(c ^ (u16::from(b) << 8), |c, _| {
            if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            }
        })
    })
}

/// STREAMINFO + VORBIS_COMMENT + 11 mono 16-bit CONSTANT-subframe frames of 4096 samples at 44.1 kHz
/// (45056 samples = 1.02 s).
fn flac(comments: &[&str]) -> Vec<u8> {
    const BLOCK: u64 = 4096;
    const FRAMES: u64 = 11;
    let mut b = b"fLaC".to_vec();
    b.extend([0, 0, 0, 34]); // STREAMINFO, not last
    for _ in 0..2 {
        b.extend((BLOCK as u16).to_be_bytes()); // min / max block size
    }
    b.extend([0, 0, 11, 0, 0, 11]); // min / max frame size
    b.extend((44100u64 << 44 | 15 << 36 | BLOCK * FRAMES).to_be_bytes()); // rate, 1 ch, 16 bit, total samples
    b.extend([0u8; 16]); // MD5 unknown
    let vendor = b"rk-test";
    let mut vc = (vendor.len() as u32).to_le_bytes().to_vec();
    vc.extend(vendor);
    vc.extend((comments.len() as u32).to_le_bytes());
    for c in comments {
        vc.extend((c.len() as u32).to_le_bytes());
        vc.extend(c.as_bytes());
    }
    b.push(0x80 | 4); // VORBIS_COMMENT, last
    b.extend(&(vc.len() as u32).to_be_bytes()[1..]);
    b.extend(vc);
    for n in 0..FRAMES {
        // sync + fixed blocksize, 4096 samples, rate/bits from STREAMINFO, mono, frame number, CRC-8
        let mut f = vec![0xFF, 0xF8, 0xC0, 0x00, n as u8];
        f.push(crc8(&f));
        f.extend([0x00, 0x12, 0x34]); // CONSTANT subframe, value 0x1234
        f.extend(crc16(&f).to_be_bytes());
        b.extend(f);
    }
    b
}

#[test]
fn flac_vorbis_comments() {
    let t = track(
        "t.flac",
        &flac(&[
            "TITLE=Flac Title",
            "ARTIST=Flac Artist",
            "ALBUM=Flac Album",
            "GENRE=Techno",
            "GENRE=House",
            "BPM=128.50",
            "INITIALKEY=8A",
            "initialkey=Am",
            "COMMENT=flac note",
        ]),
    );
    assert_eq!(
        (
            t.title.as_str(),
            t.artist.as_str(),
            t.album.as_str(),
            t.genre.as_str()
        ),
        ("Flac Title", "Flac Artist", "Flac Album", "Techno")
    );
    assert_eq!(
        (t.bpm_x100, t.key.as_str(), t.comment.as_str()),
        (12850, "8A", "flac note")
    );
    assert_eq!((t.length_s, t.sample_rate), (1, 44100));
}

#[test]
fn flac_untagged_falls_back_to_stem() {
    let t = track("plain.flac", &flac(&[]));
    assert_eq!(
        (
            t.title.as_str(),
            t.artist.as_str(),
            t.bpm_x100,
            t.key.as_str()
        ),
        ("plain", "", 0, "")
    );
}

// M4A

fn atom(t: &[u8], body: &[u8]) -> Vec<u8> {
    [&((8 + body.len()) as u32).to_be_bytes()[..], t, body].concat()
}

/// Version-0 "full" atom; `flags` is the low 24 bits.
fn full(t: &[u8], flags: u32, body: &[u8]) -> Vec<u8> {
    atom(t, &[&[0][..], &flags.to_be_bytes()[1..], body].concat())
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|n| n.to_be_bytes()).collect()
}

fn padded(mut v: Vec<u8>, n: usize) -> Vec<u8> {
    v.resize(n, 0); // unused tail of mvhd/tkhd (matrix etc.) left zero
    v
}

/// ilst item: `kind` is the `data` type code (1 = UTF-8, 21 = signed int, 0 = implicit).
fn item(t: &[u8], kind: u32, payload: &[u8]) -> Vec<u8> {
    atom(t, &full(b"data", kind, &[&[0u8; 4][..], payload].concat()))
}

fn freeform(name: &str, value: &str) -> Vec<u8> {
    let parts = [
        full(b"mean", 0, b"com.apple.iTunes"),
        full(b"name", 0, name.as_bytes()),
        full(b"data", 1, &[&[0u8; 4][..], value.as_bytes()].concat()),
    ];
    atom(b"----", &parts.concat())
}

/// ftyp, mdat, moov(mvhd, one mono 16-bit 8 kHz `sowt` PCM track of 2 s, udta/meta/ilst).
/// ponytail: PCM instead of AAC keeps the file decodable without an AAC frame; tag parsing doesn't depend on the codec.
fn m4a(items: &[Vec<u8>]) -> Vec<u8> {
    const RATE: u32 = 8000;
    const N: u32 = 2 * RATE;
    let ftyp = atom(b"ftyp", b"M4A \0\0\0\0M4A mp42isom");
    let mdat = atom(b"mdat", &vec![0u8; 2 * N as usize]);
    let mvhd = atom(
        b"mvhd",
        &padded(
            [u32s(&[0, 0, 0, RATE, N, 0x10000]), vec![1, 0]].concat(),
            100,
        ),
    );
    let tkhd = atom(b"tkhd", &padded([u32s(&[3, 0, 0, 1, 0, N])].concat(), 84));
    let mdhd = full(
        b"mdhd",
        0,
        &[u32s(&[0, 0, RATE, N]), vec![0x55, 0xC4, 0, 0]].concat(),
    );
    let hdlr = full(
        b"hdlr",
        0,
        &[&[0u8; 4][..], b"soun", &[0; 12], &[0]].concat(),
    );
    let entry = [
        &[0u8; 6][..],
        &[0, 1, 0, 0],
        &[0; 6],
        &[0, 1, 0, 16],
        &[0; 4],
        &(RATE << 16).to_be_bytes(),
    ]
    .concat(); // mono, 16 bit
    let stsd = full(b"stsd", 0, &[u32s(&[1]), atom(b"sowt", &entry)].concat());
    let stbl = atom(
        b"stbl",
        &[
            stsd,
            full(b"stts", 0, &u32s(&[1, N, 1])),
            full(b"stsc", 0, &u32s(&[1, 1, N, 1])),
            full(b"stsz", 0, &u32s(&[2, N])),
            full(b"stco", 0, &u32s(&[1, ftyp.len() as u32 + 8])), // payload of the mdat that follows ftyp
        ]
        .concat(),
    );
    let minf = atom(b"minf", &[full(b"smhd", 0, &[0; 4]), stbl].concat());
    let trak = atom(
        b"trak",
        &[tkhd, atom(b"mdia", &[mdhd, hdlr, minf].concat())].concat(),
    );
    let meta_hdlr = full(
        b"hdlr",
        0,
        &[&[0u8; 4][..], b"mdirappl", &[0; 8], &[0]].concat(),
    );
    let udta = atom(
        b"udta",
        &full(
            b"meta",
            0,
            &[meta_hdlr, atom(b"ilst", &items.concat())].concat(),
        ),
    );
    [ftyp, mdat, atom(b"moov", &[mvhd, trak, udta].concat())].concat()
}

#[test]
fn m4a_ilst() {
    let t = track(
        "t.m4a",
        &m4a(&[
            item(b"\xa9nam", 1, b"M4A Title"),
            item(b"\xa9ART", 1, b"M4A Artist"),
            item(b"\xa9alb", 1, b"M4A Album"),
            item(b"\xa9gen", 1, b"Drum & Bass"),
            item(b"tmpo", 21, &174i16.to_be_bytes()),
            freeform("initialkey", "11A"),
            item(b"\xa9cmt", 1, b"m4a note"),
        ]),
    );
    assert_eq!(
        (
            t.title.as_str(),
            t.artist.as_str(),
            t.album.as_str(),
            t.genre.as_str()
        ),
        ("M4A Title", "M4A Artist", "M4A Album", "Drum & Bass")
    );
    assert_eq!(
        (t.bpm_x100, t.key.as_str(), t.comment.as_str()),
        (17400, "11A", "m4a note")
    );
    assert_eq!((t.length_s, t.sample_rate), (2, 8000));
}

#[test]
fn m4a_numeric_genre_and_zero_tempo() {
    // gnre holds a 1-based ID3v1 index; tmpo 0 = unknown; no ©nam → file stem.
    let t = track(
        "bare.m4a",
        &m4a(&[
            item(b"gnre", 0, &2u16.to_be_bytes()),
            item(b"tmpo", 21, &0i16.to_be_bytes()),
        ]),
    );
    assert_eq!(
        (t.title.as_str(), t.genre.as_str(), t.bpm_x100),
        ("bare", "Classic rock", 0)
    );
}
