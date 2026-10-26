//! `DJMMYSETTING.DAT` (52 data bytes): the DJ mixer's settings (Preferences > DJ SYSTEM > My Settings, "MIXER" half).
//! Layout, value bytes and labels as in [`super::my`] (rekordcrate `src/setting.rs`; the real rekordbox 7 file's bytes all
//! fall on the choices; labels inferred from the spec). Bytes 25..30 hold `80 82 84 81 81` in the real file where the spec
//! has zeros (five settings newer than the spec; unknown, kept as they are). Two labels are shortened to fit the dialog's
//! 140 px label column: "HEADPHONES MONO SPLIT" and "CH FADER CURVE (LONG FADER)" (the spec's full names).

use super::{Field, Kind::Djm, OFF_ON};

const DJ: &str = "MIXER > DJ SETTING";
const BRIGHT: &str = "MIXER > BRIGHTNESS";

pub(super) const FIELDS: &[Field] = &[
    Field::new(
        Djm,
        "ch_fader_curve",
        DJ,
        "CH FADER CURVE",
        12,
        &[
            (0x80, "STEEP TOP"),
            (0x81, "LINEAR"),
            (0x82, "STEEP BOTTOM"),
        ],
    ),
    Field::new(
        Djm,
        "crossfader_curve",
        DJ,
        "CROSSFADER CURVE",
        13,
        &[
            (0x80, "CONSTANT POWER"),
            (0x81, "SLOW CUT"),
            (0x82, "FAST CUT"),
        ],
    ),
    Field::new(
        Djm,
        "headphones_pre_eq",
        DJ,
        "HEADPHONES PRE EQ",
        14,
        &[(0x80, "POST EQ"), (0x81, "PRE EQ")],
    ),
    Field::new(
        Djm,
        "headphones_mono_split",
        DJ,
        "PHONES MONO SPLIT",
        15,
        &[(0x80, "STEREO"), (0x81, "MONO SPLIT")],
    ),
    Field::new(Djm, "beat_fx_quantize", DJ, "BEAT FX QUANTIZE", 16, OFF_ON),
    Field::new(
        Djm,
        "mic_low_cut",
        DJ,
        "MIC LOW CUT",
        17,
        &[(0x80, "OFF"), (0x81, "ON (FOR MC)")],
    ),
    Field::new(
        Djm,
        "talk_over_mode",
        DJ,
        "TALK OVER MODE",
        18,
        &[(0x80, "ADVANCED"), (0x81, "NORMAL")],
    ),
    Field::new(
        Djm,
        "talk_over_level",
        DJ,
        "TALK OVER LEVEL",
        19,
        &[
            (0x80, "-24dB"),
            (0x81, "-18dB"),
            (0x82, "-12dB"),
            (0x83, "-6dB"),
        ],
    ),
    Field::new(
        Djm,
        "midi_channel",
        DJ,
        "MIDI CH",
        20,
        &[
            (0x80, "1"),
            (0x81, "2"),
            (0x82, "3"),
            (0x83, "4"),
            (0x84, "5"),
            (0x85, "6"),
            (0x86, "7"),
            (0x87, "8"),
            (0x88, "9"),
            (0x89, "10"),
            (0x8a, "11"),
            (0x8b, "12"),
            (0x8c, "13"),
            (0x8d, "14"),
            (0x8e, "15"),
            (0x8f, "16"),
        ],
    ),
    Field::new(
        Djm,
        "midi_button_type",
        DJ,
        "MIDI BUTTON TYPE",
        21,
        &[(0x80, "TOGGLE"), (0x81, "TRIGGER")],
    ),
    Field::new(
        Djm,
        "display_brightness",
        BRIGHT,
        "DISPLAY",
        22,
        &[
            (0x80, "WHITE"),
            (0x81, "1"),
            (0x82, "2"),
            (0x83, "3"),
            (0x84, "4"),
            (0x85, "5"),
        ],
    ),
    Field::new(
        Djm,
        "indicator_brightness",
        BRIGHT,
        "INDICATOR",
        23,
        &[(0x80, "1"), (0x81, "2"), (0x82, "3")],
    ),
    Field::new(
        Djm,
        "ch_fader_curve_long",
        DJ,
        "LONG FADER CURVE",
        24,
        &[(0x80, "EXPONENTIAL"), (0x81, "SMOOTH"), (0x82, "LINEAR")],
    ),
];

/// `DJMMYSETTING.DAT` data of a fresh rekordbox 7 export (observed).
pub(super) fn data() -> Vec<u8> {
    let mut d = vec![0x78, 0x56, 0x34, 0x12, 1, 0, 0, 0, 0x20, 0, 0, 0];
    d.extend([
        0x81, 0x82, 0x80, 0x80, 0x81, 0x81, 0x80, 0x81, 0x80, 0x80, 0x85, 0x82, 0x80,
    ]);
    d.extend([0x80, 0x82, 0x84, 0x81, 0x81]);
    d.resize(52, 0);
    d
}
