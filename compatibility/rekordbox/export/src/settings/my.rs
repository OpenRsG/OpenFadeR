//! `MYSETTING.DAT` (40 data bytes) and `MYSETTING2.DAT` (40): the DJ player's own settings (rekordbox Preferences > DJ
//! SYSTEM > My Settings, "PLAYER" half).
//!
//! Layout and value bytes: rekordcrate `src/setting.rs` (spec, <https://github.com/Holzhaus/rekordcrate>, fetched
//! 2026-10-13) checked byte for byte against the files of a real rekordbox 7 export (observed): every value byte of the
//! real files is one of the choices below, and the bytes in between are carried over unchanged. The choice labels are
//! the spec's (rekordcrate's names for the preferences entries), not read from rekordbox: they are inferred from there.
//! The value byte is not always `0x80 + index` (QUANTIZE BEAT VALUE, JOG MODE, AUTO CUE LEVEL and HOT CUE AUTO LOAD are
//! reversed), so every choice names its byte.
//!
//! Not modelled: `MYSETTING` bytes 13 (`01`) and 17..19 (`01 01 01`), `MYSETTING2` bytes 5..6 (`80 80`, the two settings
//! newer than the spec), byte 11 (`81`) and the zero padding: unknown, kept as they are. Labels shortened to fit the
//! dialog's 140 px label column (the spec's full names): "PLAY MODE / AUTO PLAY MODE", "DISC SLOT ILLUMINATION",
//! "PAD/BUTTON BRIGHTNESS", "WAVEFORM / PHASE METER", "BEAT JUMP BEAT VALUE".

use super::{
    Field,
    Kind::{My, My2},
    OFF_ON,
};

const UNLOCK_LOCK: &[(u8, &str)] = &[(0x80, "UNLOCK"), (0x81, "LOCK")];
const OFF_DARK_BRIGHT: &[(u8, &str)] = &[(0x80, "OFF"), (0x81, "1 (DARK)"), (0x82, "2 (BRIGHT)")];
const ONE_TO_FIVE: &[(u8, &str)] = &[
    (0x81, "1"),
    (0x82, "2"),
    (0x83, "3"),
    (0x84, "4"),
    (0x85, "5"),
];

const DJ: &str = "PLAYER > DJ SETTING";
const LCD: &str = "PLAYER > DISPLAY (LCD)";
const IND: &str = "PLAYER > DISPLAY (INDICATOR)";

pub(super) const MY: &[Field] = &[
    Field::new(My, "on_air_display", IND, "ON AIR DISPLAY", 8, OFF_ON),
    Field::new(My, "lcd_brightness", LCD, "LCD BRIGHTNESS", 9, ONE_TO_FIVE),
    Field::new(My, "quantize", DJ, "QUANTIZE", 10, OFF_ON),
    Field::new(
        My,
        "auto_cue_level",
        DJ,
        "AUTO CUE LEVEL",
        11,
        &[
            (0x87, "-78dB"),
            (0x86, "-72dB"),
            (0x85, "-66dB"),
            (0x84, "-60dB"),
            (0x83, "-54dB"),
            (0x82, "-48dB"),
            (0x81, "-42dB"),
            (0x80, "-36dB"),
            (0x88, "MEMORY"),
        ],
    ),
    Field::new(
        My,
        "language",
        LCD,
        "LANGUAGE",
        12,
        &[
            (0x81, "English"),
            (0x82, "Français"),
            (0x83, "Deutsch"),
            (0x84, "Italiano"),
            (0x85, "Nederlands"),
            (0x86, "Español"),
            (0x87, "Русский"),
            (0x88, "한국어"),
            (0x89, "简体中文"),
            (0x8a, "繁體中文"),
            (0x8b, "日本語"),
            (0x8c, "Português"),
            (0x8d, "Svenska"),
            (0x8e, "Čeština"),
            (0x8f, "Magyar"),
            (0x90, "Dansk"),
            (0x91, "Ελληνικά"),
            (0x92, "Türkçe"),
        ],
    ),
    Field::new(
        My,
        "jog_ring_brightness",
        IND,
        "JOG RING BRIGHTNESS",
        14,
        OFF_DARK_BRIGHT,
    ),
    Field::new(
        My,
        "jog_ring_indicator",
        IND,
        "JOG RING INDICATOR",
        15,
        OFF_ON,
    ),
    Field::new(My, "slip_flashing", IND, "SLIP FLASHING", 16, OFF_ON),
    Field::new(
        My,
        "disc_slot_illumination",
        IND,
        "DISC SLOT LIGHT",
        20,
        OFF_DARK_BRIGHT,
    ),
    Field::new(
        My,
        "eject_load_lock",
        DJ,
        "EJECT/LOAD LOCK",
        21,
        UNLOCK_LOCK,
    ),
    Field::new(My, "sync", DJ, "SYNC", 22, OFF_ON),
    Field::new(
        My,
        "play_mode",
        DJ,
        "PLAY MODE",
        23,
        &[(0x80, "CONTINUE / ON"), (0x81, "SINGLE / OFF")],
    ),
    Field::new(
        My,
        "quantize_beat_value",
        DJ,
        "QUANTIZE BEAT VALUE",
        24,
        &[
            (0x83, "1/8 BEAT"),
            (0x82, "1/4 BEAT"),
            (0x81, "1/2 BEAT"),
            (0x80, "1 BEAT"),
        ],
    ),
    Field::new(
        My,
        "hot_cue_auto_load",
        DJ,
        "HOT CUE AUTO LOAD",
        25,
        &[(0x80, "OFF"), (0x81, "ON"), (0x82, "rekordbox SETTING")],
    ),
    Field::new(My, "hot_cue_color", DJ, "HOT CUE COLOR", 26, OFF_ON),
    Field::new(My, "needle_lock", DJ, "NEEDLE LOCK", 29, UNLOCK_LOCK),
    Field::new(
        My,
        "time_mode",
        DJ,
        "TIME MODE",
        32,
        &[(0x80, "ELAPSED"), (0x81, "REMAIN")],
    ),
    Field::new(
        My,
        "jog_mode",
        DJ,
        "JOG MODE",
        33,
        &[(0x81, "VINYL"), (0x80, "CDJ")],
    ),
    Field::new(My, "auto_cue", DJ, "AUTO CUE", 34, OFF_ON),
    Field::new(My, "master_tempo", DJ, "MASTER TEMPO", 35, OFF_ON),
    Field::new(
        My,
        "tempo_range",
        DJ,
        "TEMPO RANGE",
        36,
        &[
            (0x80, "±6%"),
            (0x81, "±10%"),
            (0x82, "±16%"),
            (0x83, "WIDE"),
        ],
    ),
    Field::new(
        My,
        "phase_meter",
        DJ,
        "PHASE METER",
        37,
        &[(0x80, "TYPE 1"), (0x81, "TYPE 2")],
    ),
];

pub(super) const MY2: &[Field] = &[
    Field::new(
        My2,
        "vinyl_speed_adjust",
        DJ,
        "VINYL SPEED ADJUST",
        0,
        &[
            (0x80, "TOUCH & RELEASE"),
            (0x81, "TOUCH"),
            (0x82, "RELEASE"),
        ],
    ),
    Field::new(
        My2,
        "jog_display_mode",
        LCD,
        "JOG DISPLAY MODE",
        1,
        &[
            (0x80, "AUTO"),
            (0x81, "INFO"),
            (0x82, "SIMPLE"),
            (0x83, "ARTWORK"),
        ],
    ),
    Field::new(
        My2,
        "pad_button_brightness",
        IND,
        "PAD BRIGHTNESS",
        2,
        &[(0x81, "1"), (0x82, "2"), (0x83, "3"), (0x84, "4")],
    ),
    Field::new(
        My2,
        "jog_lcd_brightness",
        LCD,
        "JOG LCD BRIGHTNESS",
        3,
        ONE_TO_FIVE,
    ),
    Field::new(
        My2,
        "waveform_divisions",
        DJ,
        "WAVEFORM DIVISIONS",
        4,
        &[(0x80, "TIME SCALE"), (0x81, "PHRASE")],
    ),
    Field::new(
        My2,
        "waveform",
        DJ,
        "WAVEFORM TYPE",
        10,
        &[(0x80, "WAVEFORM"), (0x81, "PHASE METER")],
    ),
    Field::new(
        My2,
        "beat_jump_beat_value",
        DJ,
        "BEAT JUMP VALUE",
        12,
        &[
            (0x80, "1/2 BEAT"),
            (0x81, "1 BEAT"),
            (0x82, "2 BEAT"),
            (0x83, "4 BEAT"),
            (0x84, "8 BEAT"),
            (0x85, "16 BEAT"),
            (0x86, "32 BEAT"),
            (0x87, "64 BEAT"),
        ],
    ),
];

/// `MYSETTING.DAT` data of a fresh rekordbox 7 export (observed; the spec's `Default`).
pub(super) fn data() -> Vec<u8> {
    let mut d = vec![0x78, 0x56, 0x34, 0x12, 2, 0, 0, 0];
    d.extend([
        0x81, 0x83, 0x81, 0x88, 0x81, 0x01, 0x82, 0x81, 0x81, 1, 1, 1,
    ]);
    d.extend([0x82, 0x80, 0x80, 0x81, 0x80, 0x81, 0x80, 0, 0, 0x81, 0, 0]);
    d.extend([0x81, 0x81, 0x81, 0x80, 0x81, 0x80, 0, 0]);
    d
}

/// `MYSETTING2.DAT` data of a fresh rekordbox 7 export (observed: `80 80` at 5..7 where the spec has zeros).
pub(super) fn data2() -> Vec<u8> {
    let mut d = vec![
        0x81, 0x80, 0x83, 0x83, 0x81, 0x80, 0x80, 0, 0, 0, 0x80, 0x81, 0x85,
    ];
    d.resize(40, 0);
    d
}
