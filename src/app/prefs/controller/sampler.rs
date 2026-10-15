//! SAMPLER tab: slots, capture, Auto Panel Mode, SEQUENCER (Sequence Load; QUANTIZE > SEQUENCER lives with the other quantize rows
//! on OTHERS) and the SLICER start-up LENGTH / ROLL of the deck pads.

use super::{Controller, Row, SAMPLER_SLOTS, SLICER_BARS, SLICER_ROLL};

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    let slots: Vec<String> = SAMPLER_SLOTS.iter().map(|n| format!("{n} slots")).collect();
    let at = SAMPLER_SLOTS
        .iter()
        .position(|&n| n as usize == c.slots())
        .unwrap_or(2);
    vec![
        Row::header("SAMPLER"),
        Row::chips(
            "sampler_slots",
            "SLOT",
            &[&slots[0], &slots[1], &slots[2]],
            at,
        ),
        Row::check(
            "sampler_keep_loaded",
            "Do not overwrite a loaded slot",
            c.sampler_keep_loaded,
        ),
        Row::text("capture_dir", "CAPTURE FOLDER", &c.capture_dir),
        Row::info("", "empty = ~/Music/OpenFadeR/Sampler"),
        Row::check(
            "capture_loop_empty",
            "Capture the loop in an empty slot by pressing its pad during loop playback",
            c.capture_loop_empty,
        ),
        Row::check(
            "sampler_auto_panel",
            "Auto Panel Mode: the SAMPLER button of a controller opens the sampler",
            c.sampler_auto_panel,
        ),
        Row::header("SEQUENCER"),
        Row::check(
            "sequence_load",
            "Sequence Load: a track loaded into a slot loops over its full length and starts",
            c.sequence_load,
        ),
        Row::info(
            "QUANTIZE",
            "hits are recorded on the grid of OTHERS > QUANTIZE > SEQUENCER (beat length of QUANTIZE)",
        ),
        Row::header("SLICER"),
        Row::chips(
            "slicer_length",
            "LENGTH",
            &["1 BAR", "2 BARS", "4 BARS", "8 BARS"],
            SLICER_BARS
                .iter()
                .position(|&b| b == c.slicer_bars())
                .unwrap_or(0),
        )
        .hint("range cut into 8 slices"),
        Row::chips(
            "slicer_roll",
            "ROLL",
            &["1/8", "1/16", "1/32", "1/64"],
            SLICER_ROLL
                .iter()
                .position(|&r| r == c.slicer_roll())
                .unwrap_or(0),
        )
        .hint("of the LENGTH, played while a pad is held"),
    ]
}
