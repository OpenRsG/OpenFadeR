//! EFFECT tab.

use super::{Controller, Row};

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    vec![
        Row::header(tr!("EFFECT")),
        Row::chips(
            "release_units",
            tr!("RELEASE FX UNITS"),
            &[tr!("1 (MASTER)"), tr!("2 (EACH UNIT)")],
            c.release_units() as usize - 1,
        )
        .hint(tr!("1 = on the master output")),
        Row::check(
            "release_cfx_off",
            tr!("Apply RELEASE FX on CFX: SOUND COLOR FX goes off while RELEASE is held"),
            c.release_cfx_off,
        ),
        Row::check("merge_enabled", tr!("Enable MERGE FX"), c.merge_enabled),
        Row::check(
            "merge_stop_deck",
            tr!("Stop playing the operating deck after MERGE FX finishes"),
            c.merge_stop_deck,
        )
        .enabled(c.merge_enabled),
    ]
}
