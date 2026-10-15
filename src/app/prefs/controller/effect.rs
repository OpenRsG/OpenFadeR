//! EFFECT tab.

use super::{Controller, Row};

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    vec![
        Row::header("EFFECT"),
        Row::chips(
            "release_units",
            "RELEASE FX UNITS",
            &["1 (MASTER)", "2 (EACH UNIT)"],
            c.release_units() as usize - 1,
        )
        .hint("1 = on the master output"),
        Row::check(
            "release_cfx_off",
            "Apply RELEASE FX on CFX: SOUND COLOR FX goes off while RELEASE is held",
            c.release_cfx_off,
        ),
        Row::check("merge_enabled", "Enable MERGE FX", c.merge_enabled),
        Row::check(
            "merge_stop_deck",
            "Stop playing the operating deck after MERGE FX finishes",
            c.merge_stop_deck,
        )
        .enabled(c.merge_enabled),
    ]
}
