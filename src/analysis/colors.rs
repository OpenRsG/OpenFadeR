//! Neutral colour helpers of the analysis data: the hot-cue colour table, the colours the cue colour picker offers, the default
//! hot-cue colour per pad, and the decoder of one colour-detail waveform entry.

use openfader_plugin::library::colors::RAINBOW;
pub use openfader_plugin::library::colors::picker;

/// The default hot-cue colours, one per pad A-H (the colour a new hot cue gets): entries 43, 8, 23, 60, 19, 38, 1 and 32 of
/// [`RAINBOW`].
pub const COLORFUL: [[u8; 3]; 8] = {
    const IDX: [usize; 8] = [43, 8, 23, 60, 19, 38, 1, 32];
    let mut o = [[0; 3]; 8];
    let mut i = 0;
    while i < 8 {
        o[i] = RAINBOW[IDX[i]];
        i += 1;
    }
    o
};

/// Colour detail waveform entry: bits 15..13 red, 12..10 green, 9..7 blue, 6..2 height, 1..0 unused. Returns RGB scaled to
/// 0..=255 and height 0..=31.
pub fn color_detail_rgbh(v: u16) -> ([u8; 3], u8) {
    let c = |shift: u16| (((v >> shift) & 7) * 255 / 7) as u8;
    ([c(13), c(10), c(7)], ((v >> 2) & 31) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_and_default_pad_colours() {
        assert_eq!(
            COLORFUL,
            [
                [0xff, 0, 0x17],
                [0, 0xc4, 0xff],
                [0x33, 0xff, 0],
                [0x4d, 0, 0xff],
                [0, 0xff, 0x30],
                [0xff, 0x5e, 0],
                [0, 0, 0xff],
                [0xff, 0xe8, 0]
            ]
        );
        assert_eq!(picker().count(), 63);
        assert!(picker().all(|c| c != [0, 0, 0]));
        assert_eq!(RAINBOW[64], [255, 255, 255]);
    }

    #[test]
    fn colour_detail_entries_decode() {
        let v = 0b101_010_111_10001_00u16;
        assert_eq!(color_detail_rgbh(v), ([182, 72, 255], 17));
        assert_eq!(color_detail_rgbh(0xFFFF), ([255, 255, 255], 31));
    }
}
