//! OTHERS tab: quantize. (Ableton Link is network-based and left out.)

use super::{Controller, Row};

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    vec![
        Row::header("OTHERS (needs Q on)"),
        Row::chips(
            "quantize_mode",
            "MODE",
            &["IMMEDIATE", "NEXT BEAT"],
            c.quantize_mode as usize,
        )
        .hint("hot cue / loop"),
        Row::of("snap_length", "SNAP", c.snap_length).hint("beats"),
        Row::of("quantize_length", "QUANTIZE", c.quantize_length).hint("beats"),
        Row::check("quantize_hot_cue", "QUANTIZE: HOT CUE", c.quantize_hot_cue),
        Row::check(
            "quantize_loop",
            "QUANTIZE: LOOP/SAMPLER(LOOP)",
            c.quantize_loop,
        ),
        Row::check("quantize_reverse", "QUANTIZE: REVERSE", c.quantize_reverse),
        Row::check(
            "quantize_sequencer",
            "QUANTIZE: SEQUENCER",
            c.quantize_sequencer,
        ),
        Row::check(
            "quantize_all_decks",
            "Q switches all decks at once",
            c.quantize_all_decks,
        ),
    ]
}
