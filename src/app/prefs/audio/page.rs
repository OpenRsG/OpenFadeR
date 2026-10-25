//! The AUDIO page rows (see the parent doc).

use super::{
    aux_choices, buffer_in_use, caps_of, db_label, external_ok, nearest, pairs_of, MixerMode,
    AUX_RELEASE_MS, BOOTH_DB, MIC_DB, PREVIEW_DB, TALK_DB, VOLUME_STEP,
};
use crate::app::prefs::{Prefs, Row};
use openfader::audio::{
    self, auxin,
    device::{self, card, Caps},
    metronome::TONES,
    mic,
    route::{self, EXTERNAL_NAMES},
    Status,
};

/// AUTO plus the supported values, and the selected index. A saved value the device does not support is listed
/// as such, so the row never lies.
pub(super) fn value_options(supported: &[u32], current: u32, unit: &str) -> (Vec<String>, usize) {
    let mut o = vec![tr!("AUTO").to_owned()];
    o.extend(supported.iter().map(|v| format!("{v} {unit}")));
    let at = match current {
        0 => 0,
        v => supported.iter().position(|&s| s == v).map_or_else(
            || {
                o.push(format!("{v} {unit} {}", tr!("(not supported)")));
                o.len() - 1
            },
            |i| i + 1,
        ),
    };
    (o, at)
}

/// "1/2", "3/4", ... for `pairs` pairs (and `current` past them, marked).
pub(super) fn pair_labels(pairs: usize, current: u8) -> Vec<String> {
    (1..=pairs.max(current as usize))
        .map(|n| {
            if n > pairs {
                tr!("{} (not on this device)", route::pair_name(n as u8))
            } else {
                route::pair_name(n as u8)
            }
        })
        .collect()
}

/// `first` (OFF / AUTO) followed by the pairs: index = pair number.
pub(super) fn numbered(first: &str, pairs: usize, current: u8) -> Vec<String> {
    std::iter::once(first.to_owned())
        .chain(pair_labels(pairs, current))
        .collect()
}

/// Index into [`pair_labels`] of pair `n` (pair numbers start at 1).
fn pair_index(n: u8) -> usize {
    n.max(1) as usize - 1
}

/// The ducking levels ([`TALK_DB`]): OFF, then the dB steps.
fn talk_labels() -> Vec<String> {
    TALK_DB
        .iter()
        .map(|&d| {
            if d == 0 {
                tr!("OFF").to_owned()
            } else {
                db_label(d)
            }
        })
        .collect()
}

fn db_labels(steps: &[i32]) -> Vec<String> {
    steps.iter().map(|&d| db_label(d)).collect()
}

/// `text` when `cond`, else no hint.
fn hint_if(cond: bool, text: impl FnOnce() -> String) -> String {
    cond.then(text).unwrap_or_default()
}

/// The device whose channels the layout rows are about (named in messages).
fn device_name(p: &Prefs, st: &Status) -> String {
    if st.device.is_empty() {
        p.audio.output_device.clone()
    } else {
        st.device.clone()
    }
}

/// The connected controller and the audio output it got.
fn control_device(st: &Status) -> String {
    match audio::controller() {
        None => tr!("no controller using its own audio interface").to_owned(),
        Some(c) if st.source == "controller" => tr!("{} · audio output: {}", c, st.device),
        Some(c) => tr!("{} · audio output unchanged: {}", c, st.device),
    }
}

pub(super) fn automatic_buffer_label(p: &Prefs, st: &Status) -> String {
    if crate::app::prefs::lowpower::on(p)
        || (p.audio.buffer_size == 0 && st.rate > 0 && st.buffer.is_none())
    {
        tr!("AUTO (driver default)").to_owned()
    } else {
        tr!("AUTO ({} frames)", route::DEFAULT_BUFFER_FRAMES)
    }
}

pub fn rows(p: &Prefs) -> Vec<Row> {
    let a = &p.audio;
    let st = audio::status();
    let caps = caps_of(p, &st);
    let (channels, pairs) = pairs_of(caps.as_ref());
    let empty = Caps::default();
    let c = caps.as_ref().unwrap_or(&empty);
    let mut rows = vec![Row::header(tr!("CONFIGURATION"))];

    // "Audio" is the device cards above these rows (`cards`); the status of the stream is on the cards.
    let (o, at) = value_options(&c.rates, a.sample_rate, "Hz");
    rows.push(
        Row::dropdown_of("sample_rate", tr!("SAMPLE RATE"), o, at)
            .hint(&hint_if(st.rate > 0, || tr!("{} Hz now", st.rate))),
    );
    let (mut o, at) = value_options(&c.buffers, a.buffer_size, tr!("frames"));
    o[0] = automatic_buffer_label(p, &st);
    let frames = buffer_in_use(p, &st);
    let latency = device::latency_ms(frames, st.rate);
    rows.push(
        Row::dropdown_of("buffer_size", tr!("BUFFER SIZE"), o, at)
            .hint(&hint_if(frames > 0, || format!("~{latency:.1} ms"))),
    );
    if st.rate > 0 {
        let line = format!(
            "{} · {} Hz · {} ch{}",
            st.device,
            st.rate,
            st.channels,
            hint_if(frames > 0, || tr!(
                " · {} frames = ~{} ms",
                frames,
                format!("{latency:.1}")
            ))
        );
        rows.push(Row::info(tr!("OUTPUT IN USE"), &line));
    }
    for n in &st.notes {
        rows.push(Row::info(tr!("NOTE"), n));
    }
    if let Some(d) = &st.pending {
        rows.push(Row::info(tr!("NOTE"), &tr!("looking for {}…", d)));
    }
    if let Some(e) = &st.error {
        rows.push(Row::info(tr!("PROBLEM"), e));
    }

    rows.push(Row::header(tr!("METRONOME")));
    let tones: Vec<String> = (1..=TONES).map(|t| tr!("TONE {}", t)).collect();
    rows.push(
        Row::chips(
            "metronome_tone",
            tr!("TONE"),
            &tones.iter().map(String::as_str).collect::<Vec<_>>(),
            a.metronome_tone as usize,
        )
        .hint(tr!("grid edit")),
    );
    let vols: Vec<String> = (0..=100 / VOLUME_STEP as usize)
        .map(|i| format!("{}%", i * VOLUME_STEP as usize))
        .collect();
    rows.push(Row::dropdown_of(
        "metronome_volume",
        tr!("VOLUME"),
        vols,
        (a.metronome_volume / VOLUME_STEP).min(100 / VOLUME_STEP) as usize,
    ));
    rows.push(Row::header(tr!("PREVIEW")));
    rows.push(
        Row::dropdown_of(
            "preview_volume",
            tr!("PREVIEW VOLUME"),
            db_labels(&PREVIEW_DB),
            nearest(&PREVIEW_DB, a.preview_volume),
        )
        .hint(tr!("headphones")),
    );
    rows.push(Row::header(tr!("CONTROL DEVICE")));
    rows.push(Row::info(
        tr!("CONTROL DEVICE INFORMATION"),
        &control_device(&st),
    ));

    rows.push(Row::header(tr!("INPUT / OUTPUT")));
    let external = a.mixer_mode == MixerMode::External;
    let offered = external_ok(channels) || external;
    rows.push(
        Row::chips(
            "mixer_mode",
            tr!("MIXER MODE"),
            &[tr!("INTERNAL"), tr!("EXTERNAL")],
            external as usize,
        )
        .enabled(offered)
        .hint(&hint_if(!offered, || {
            tr!("needs 4+ channels, has {}", channels)
        })),
    );
    if !offered {
        rows.push(Row::info(
            tr!("EXTERNAL MIXER MODE"),
            &tr!(
                "unavailable: {} has {} output channels, it needs at least 4",
                device_name(p, &st),
                channels
            ),
        ));
    }
    if external {
        rows.push(Row::header(tr!("OUTPUT CHANNELS · EXTERNAL")));
        for (k, (name, cur)) in EXTERNAL_NAMES.iter().zip(a.externals()).enumerate() {
            rows.push(Row::dropdown_of(
                &format!("ext_{k}"),
                &tr!("OUTPUT {}", name),
                numbered(tr!("OFF"), pairs, cur),
                cur as usize,
            ));
        }
    } else {
        rows.push(Row::header(tr!("OUTPUT CHANNELS · INTERNAL")));
        rows.push(Row::dropdown_of(
            "out_master",
            tr!("MASTER OUTPUT"),
            pair_labels(pairs, a.out_master),
            pair_index(a.out_master),
        ));
        rows.push(Row::dropdown_of(
            "out_phones",
            tr!("HEADPHONES OUTPUT"),
            numbered(tr!("AUTO"), pairs, a.out_phones),
            a.out_phones as usize,
        ));
        rows.push(Row::dropdown_of(
            "out_booth",
            tr!("BOOTH OUTPUT"),
            numbered(tr!("OFF"), pairs, a.out_booth),
            a.out_booth as usize,
        ));
        rows.push(
            Row::dropdown_of(
                "booth_level",
                tr!("BOOTH LEVEL"),
                db_labels(&BOOTH_DB),
                nearest(&BOOTH_DB, a.booth_level),
            )
            .enabled(a.out_booth > 0),
        );
    }

    rows.push(Row::header(tr!("INPUT CHANNELS")));
    let ch = card::choices(&device::cards(true, a.output_show_all, &a.input_device));
    let at = ch
        .iter()
        .position(|(_, n)| *n == a.input_device)
        .unwrap_or(0);
    rows.push(Row::dropdown_of(
        "input_device",
        tr!("INPUT DEVICE"),
        ch.into_iter().map(|(l, _)| l).collect(),
        at,
    ));
    let in_caps = device::input_caps(&a.input_device);
    let (_, in_pairs) = pairs_of(in_caps.as_ref());
    rows.push(
        Row::dropdown_of(
            "input_pair",
            tr!("INPUT CHANNELS"),
            pair_labels(in_pairs, a.input_pair),
            pair_index(a.input_pair),
        )
        .enabled(in_caps.is_some()),
    );
    if in_caps.is_none() {
        rows.push(Row::info(
            tr!("INPUT"),
            tr!("no such input device: nothing to record from"),
        ));
    }
    rows.push(
        Row::chips(
            "rec_source",
            tr!("REC SOURCE"),
            &[tr!("MASTER"), tr!("INPUT")],
            a.rec_source as usize,
        )
        .hint(tr!("recordings")),
    );

    rows.push(Row::header(tr!("MICROPHONE INPUT")));
    rows.push(
        Row::check("mic_on", tr!("Enable microphone input"), a.mic_on)
            .hint(tr!("uses the input device above")),
    );
    rows.push(
        Row::dropdown_of(
            "mic_pair",
            tr!("MIC CHANNELS"),
            pair_labels(in_pairs, a.mic_pair),
            pair_index(a.mic_pair),
        )
        .enabled(a.mic_on && in_caps.is_some()),
    );
    rows.push(
        Row::dropdown_of(
            "mic_level",
            tr!("MIC LEVEL"),
            db_labels(&MIC_DB),
            nearest(&MIC_DB, a.mic_level),
        )
        .enabled(a.mic_on),
    );
    rows.push(
        Row::of("talkover_mode", tr!("TALKOVER MODE"), a.talkover_mode)
            .hint(tr!("advanced: mid range only")),
    );
    rows.push(
        Row::dropdown_of(
            "talkover_level",
            tr!("TALKOVER LEVEL"),
            talk_labels(),
            nearest(&TALK_DB, a.talkover_level),
        )
        .enabled(a.mic_on),
    );
    if let Some(e) = mic::problem() {
        rows.push(Row::info(tr!("MICROPHONE"), &e));
    }

    rows.push(Row::header(tr!("AUX INPUT")));
    rows.push(
        Row::check("aux_on", tr!("Enable AUX input"), a.aux_on)
            .hint(tr!("shows the AUX strip in the mixer")),
    );
    let ch = aux_choices(&a);
    let at = ch.iter().position(|(_, n)| *n == a.aux_device).unwrap_or(0);
    rows.push(
        Row::dropdown_of(
            "aux_device",
            tr!("AUX DEVICE"),
            ch.into_iter().map(|(l, _)| l).collect(),
            at,
        )
        .enabled(a.aux_on),
    );
    let (aux_dev, _) = a.aux_source();
    let aux_caps = device::input_caps(aux_dev);
    let (_, aux_pairs) = pairs_of(aux_caps.as_ref());
    rows.push(
        Row::dropdown_of(
            "aux_pair",
            tr!("AUX CHANNELS"),
            pair_labels(aux_pairs, a.aux_pair),
            pair_index(a.aux_pair),
        )
        .enabled(a.aux_on && aux_caps.is_some()),
    );
    rows.push(
        Row::dropdown_of(
            "aux_duck",
            tr!("AUX DUCK LEVEL"),
            talk_labels(),
            nearest(&TALK_DB, a.aux_duck),
        )
        .hint(tr!("lowers the decks while the AUX plays"))
        .enabled(a.aux_on),
    );
    let ms: Vec<i32> = AUX_RELEASE_MS.iter().map(|&m| m as i32).collect();
    rows.push(
        Row::dropdown_of(
            "aux_release",
            tr!("AUX DUCK RELEASE"),
            ms.iter().map(|m| tr!("{} ms", m)).collect(),
            nearest(&ms, a.aux_release as i32),
        )
        .enabled(a.aux_on && a.aux_duck != 0),
    );
    if let Some(e) = auxin::problem() {
        rows.push(Row::info(tr!("AUX"), &e));
    }
    rows
}
