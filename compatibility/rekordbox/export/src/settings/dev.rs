//! `DEVSETTING.DAT` (32 data bytes): Preferences > DJ SYSTEM > General (waveform colour, overview type, key format,
//! waveform position). Layout and value bytes: rekordcrate `src/setting.rs` (spec; defaults of rekordbox 6.6.1). The real
//! rekordbox 7 export we have carries no `DEVSETTING.DAT`, so these bytes are unobserved on a 7.x device; labels are the
//! spec's (rekordcrate) and match the UI strings found in `rekordbox.exe` ("Half Waveform", "Full Waveform", "Classic",
//! "Alphanumeric", "CENTER"; observed). LEFT is the spec's second position (the exe lists only CENTER: inferred).
//! Bytes 0..9 are `78 56 34 12 01 00 00 00 01`, byte 11 is `01`, the rest zero (spec). Two labels are shortened to fit the
//! dialog's 140 px label column: "Type of the Overview Waveform" and "Waveform Current Position" (the exe's full texts).

use super::{Field, Kind::Dev};

const GENERAL: &str = "GENERAL";

pub(super) const FIELDS: &[Field] = &[
    Field::new(
        Dev,
        "overview_waveform",
        GENERAL,
        "Overview Waveform",
        9,
        &[(0x01, "Half Waveform"), (0x02, "Full Waveform")],
    ),
    Field::new(
        Dev,
        "waveform_color",
        GENERAL,
        "Waveform Color",
        10,
        &[(0x01, "BLUE"), (0x03, "RGB"), (0x04, "3Band")],
    ),
    Field::new(
        Dev,
        "key_display",
        GENERAL,
        "Key display format",
        12,
        &[(0x01, "Classic"), (0x02, "Alphanumeric")],
    ),
    Field::new(
        Dev,
        "waveform_position",
        GENERAL,
        "Waveform Position",
        13,
        &[(0x01, "CENTER"), (0x02, "LEFT")],
    ),
];

/// `DEVSETTING.DAT` data with the spec's defaults.
pub(super) fn data() -> Vec<u8> {
    let mut d = vec![0x78, 0x56, 0x34, 0x12, 1, 0, 0, 0, 1, 1, 1, 1, 1, 1];
    d.resize(32, 0);
    d
}
