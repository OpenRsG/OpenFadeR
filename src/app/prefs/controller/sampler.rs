//! SAMPLER tab: slots, capture, Auto Panel Mode, SEQUENCER (Sequence Load; QUANTIZE > SEQUENCER lives with the other quantize rows
//! on OTHERS) and the SLICER start-up LENGTH / ROLL of the deck pads.

use super::{Controller, Row, SAMPLER_SLOTS, SLICER_BARS, SLICER_ROLL};

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    let slots: Vec<String> = SAMPLER_SLOTS.iter().map(|n| tr!("{} slots", n)).collect();
    let at = SAMPLER_SLOTS
        .iter()
        .position(|&n| n as usize == c.slots())
        .unwrap_or(2);
    vec![
        Row::header(tr!("SAMPLER")),
        Row::chips(
            "sampler_slots",
            tr!("SLOT"),
            &[&slots[0], &slots[1], &slots[2]],
            at,
        ),
        Row::check(
            "sampler_keep_loaded",
            tr!("Do not overwrite a loaded slot"),
            c.sampler_keep_loaded,
        ),
        Row::text("capture_dir", tr!("CAPTURE FOLDER"), &c.capture_dir),
        Row::info("", tr!("empty = ~/Music/OpenFadeR/Sampler")),
        Row::check(
            "capture_loop_empty",
            tr!("Capture the loop in an empty slot by pressing its pad during loop playback"),
            c.capture_loop_empty,
        ),
        Row::check(
            "sampler_auto_panel",
            tr!("Auto Panel Mode: the SAMPLER button of a controller opens the sampler"),
            c.sampler_auto_panel,
        ),
        Row::header(tr!("SEQUENCER")),
        Row::check(
            "sequence_load",
            tr!("Sequence Load: a track loaded into a slot loops over its full length and starts"),
            c.sequence_load,
        ),
        Row::info(
            tr!("QUANTIZE"),
            tr!("hits are recorded on the grid of OTHERS > QUANTIZE > SEQUENCER (beat length of QUANTIZE)"),
        ),
        Row::header(tr!("SLICER")),
        Row::chips(
            "slicer_length",
            tr!("LENGTH"),
            &[tr!("1 BAR"), tr!("2 BARS"), tr!("4 BARS"), tr!("8 BARS")],
            SLICER_BARS
                .iter()
                .position(|&b| b == c.slicer_bars())
                .unwrap_or(0),
        )
        .hint(tr!("range cut into 8 slices")),
        Row::chips(
            "slicer_roll",
            tr!("ROLL"),
            &["1/8", "1/16", "1/32", "1/64"],
            SLICER_ROLL
                .iter()
                .position(|&r| r == c.slicer_roll())
                .unwrap_or(0),
        )
        .hint(tr!("of the LENGTH, played while a pad is held")),
    ]
}
