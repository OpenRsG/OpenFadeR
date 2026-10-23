//! OTHERS tab: quantize, the MIDI clock output, soft takeover. (Ableton Link is network-based and left out.)

use super::{Controller, Row};

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    vec![
        Row::header(tr!("OTHERS (needs Q on)")),
        Row::chips(
            "quantize_mode",
            tr!("MODE"),
            &[tr!("IMMEDIATE"), tr!("NEXT BEAT")],
            c.quantize_mode as usize,
        )
        .hint(tr!("hot cue / loop")),
        Row::of("snap_length", tr!("SNAP"), c.snap_length).hint(tr!("beats")),
        Row::of("quantize_length", tr!("QUANTIZE"), c.quantize_length).hint(tr!("beats")),
        Row::check("quantize_hot_cue", tr!("QUANTIZE: HOT CUE"), c.quantize_hot_cue),
        Row::check(
            "quantize_loop",
            tr!("QUANTIZE: LOOP/SAMPLER(LOOP)"),
            c.quantize_loop,
        ),
        Row::check("quantize_reverse", tr!("QUANTIZE: REVERSE"), c.quantize_reverse),
        Row::check(
            "quantize_sequencer",
            tr!("QUANTIZE: SEQUENCER"),
            c.quantize_sequencer,
        ),
        Row::check(
            "quantize_all_decks",
            tr!("Q switches all decks at once"),
            c.quantize_all_decks,
        ),
        Row::header(tr!("MIDI CLOCK OUT")),
        Row::text("midi_clock_port", tr!("OUTPUT PORT"), &c.midi_clock_port),
        Row::info(
            "",
            tr!("part of the port name; empty = off. 24 PPQN at the playing master deck's tempo and beat, START on its next bar"),
        ),
        Row::header(tr!("MIDI CONTROLS")),
        Row::check(
            "soft_takeover",
            tr!("Soft takeover: a knob or fader waits until it reaches the value on screen"),
            c.soft_takeover,
        ),
    ]
}
