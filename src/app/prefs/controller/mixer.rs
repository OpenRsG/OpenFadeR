//! MIXER tab.

use super::{Fader, KnobMode, OUTPUT_DB, Prefs, Row};
use crate::app::prefs::rows::Kind;

/// A row of the host's entries then the plugins' (`names` as stored: lower case): chips for up to 4, else a drop-down.
fn named(key: &str, label: &str, names: Vec<String>, current: usize) -> Row {
    let mut row = Row::dropdown_of(
        key,
        label,
        names
            .iter()
            .map(|n| crate::app::i18n::tr_dyn(&n.to_uppercase()).to_owned())
            .collect(),
        current,
    );
    if names.len() <= 4 {
        row.kind = Kind::Chips;
    }
    row
}

pub(super) fn rows(p: &Prefs) -> Vec<Row> {
    let c = &p.controller;
    let levels: Vec<String> = OUTPUT_DB
        .iter()
        .map(|&d| {
            if d == 0 {
                tr!("NONE").into()
            } else {
                format!("{d} dB")
            }
        })
        .collect();
    let level = OUTPUT_DB
        .iter()
        .position(|&d| d == c.output_level)
        .unwrap_or(OUTPUT_DB.len() - 1);
    vec![
        Row::header(tr!("MIXER")),
        Row::check(
            "auto_gain",
            tr!("Auto Gain: even out the loudness of loaded tracks"),
            c.auto_gain,
        ),
        Row::of("eq_type", tr!("EQ"), c.eq_type).hint(tr!("kill at the bottom")),
        named(
            "fader_curve",
            tr!("CHANNEL FADER CURVE"),
            Fader::names(),
            c.fader_curve.0,
        ),
        named(
            "cfx_knob",
            tr!("COLOR FX KNOB"),
            KnobMode::names(),
            c.cfx_knob.0,
        )
        .hint(tr!("SOUND COLOR FX knobs")),
        Row::check(
            "crossfader_enabled",
            tr!("Enable crossfader"),
            c.crossfader_enabled,
        )
        .hint(tr!("Automix transitions remain active")),
        Row::of("curve", tr!("CROSSFADER CURVE"), p.crossfader_curve),
        Row::dropdown_of("output_level", tr!("OUTPUT LEVEL"), levels, level).hint(tr!("all decks")),
        Row::chips(
            "controller_audio",
            tr!("CONTROLLER AUDIO"),
            &[tr!("ON"), tr!("OFF")],
            !p.controller_audio as usize,
        )
        .hint(tr!("on connect")),
        Row::info(
            tr!("MICROPHONE / TALK OVER"),
            tr!("needs a microphone input: OpenFadeR has none"),
        ),
    ]
}

/// CROSSFADER Curve and Controller audio are base fields of [`Prefs`].
pub(super) fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    ["curve", "controller_audio"].contains(&key) && p.choose(key, i)
}
