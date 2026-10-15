//! The AUDIO page rows (see the parent doc).

use super::{
    BOOTH_DB, MIC_DB, MixerMode, PREVIEW_DB, TALK_DB, VOLUME_STEP, buffer_in_use, caps_of,
    db_label, external_ok, nearest, pairs_of,
};
use crate::app::prefs::{Prefs, Row};
use openfader::audio::{
    self, Status,
    device::{self, Caps, card},
    metronome::TONES,
    mic,
    route::{self, EXTERNAL_NAMES},
};

/// AUTO plus the supported values, and the selected index. A saved value the device does not support is listed
/// as such, so the row never lies.
pub(super) fn value_options(supported: &[u32], current: u32, unit: &str) -> (Vec<String>, usize) {
    let mut o = vec!["AUTO".to_owned()];
    o.extend(supported.iter().map(|v| format!("{v} {unit}")));
    let at = match current {
        0 => 0,
        v => supported.iter().position(|&s| s == v).map_or_else(
            || {
                o.push(format!("{v} {unit} (not supported)"));
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
                format!("{} (not on this device)", route::pair_name(n as u8))
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
        None => "no controller using its own audio interface".to_owned(),
        Some(c) if st.source == "controller" => format!("{c} · audio output: {}", st.device),
        Some(c) => format!("{c} · audio output unchanged: {}", st.device),
    }
}

pub fn rows(p: &Prefs) -> Vec<Row> {
    let a = &p.audio;
    let st = audio::status();
    let caps = caps_of(p, &st);
    let (channels, pairs) = pairs_of(caps.as_ref());
    let empty = Caps::default();
    let c = caps.as_ref().unwrap_or(&empty);
    let mut rows = vec![Row::header("CONFIGURATION")];

    // "Audio" is the device cards above these rows (`cards`); the status of the stream is on the cards.
    let (o, at) = value_options(&c.rates, a.sample_rate, "Hz");
    rows.push(
        Row::dropdown_of("sample_rate", "SAMPLE RATE", o, at)
            .hint(&hint_if(st.rate > 0, || format!("{} Hz now", st.rate))),
    );
    let (o, at) = value_options(&c.buffers, a.buffer_size, "frames");
    let frames = buffer_in_use(p, &st);
    let latency = device::latency_ms(frames, st.rate);
    rows.push(
        Row::dropdown_of("buffer_size", "BUFFER SIZE", o, at)
            .hint(&hint_if(frames > 0, || format!("~{latency:.1} ms"))),
    );
    if st.rate > 0 {
        let line = format!(
            "{} · {} Hz · {} ch{}",
            st.device,
            st.rate,
            st.channels,
            hint_if(frames > 0, || format!(
                " · {frames} frames = ~{latency:.1} ms"
            ))
        );
        rows.push(Row::info("OUTPUT IN USE", &line));
    }
    for n in &st.notes {
        rows.push(Row::info("NOTE", n));
    }
    if let Some(e) = &st.error {
        rows.push(Row::info("PROBLEM", e));
    }

    rows.push(Row::header("METRONOME"));
    let tones: Vec<String> = (1..=TONES).map(|t| format!("TONE {t}")).collect();
    rows.push(
        Row::chips(
            "metronome_tone",
            "TONE",
            &tones.iter().map(String::as_str).collect::<Vec<_>>(),
            a.metronome_tone as usize,
        )
        .hint("grid edit"),
    );
    let vols: Vec<String> = (0..=100 / VOLUME_STEP as usize)
        .map(|i| format!("{}%", i * VOLUME_STEP as usize))
        .collect();
    rows.push(Row::dropdown_of(
        "metronome_volume",
        "VOLUME",
        vols,
        (a.metronome_volume / VOLUME_STEP).min(100 / VOLUME_STEP) as usize,
    ));
    rows.push(Row::header("PREVIEW"));
    rows.push(
        Row::dropdown_of(
            "preview_volume",
            "PREVIEW VOLUME",
            db_labels(&PREVIEW_DB),
            nearest(&PREVIEW_DB, a.preview_volume),
        )
        .hint("headphones"),
    );
    rows.push(Row::header("CONTROL DEVICE"));
    rows.push(Row::info(
        "CONTROL DEVICE INFORMATION",
        &control_device(&st),
    ));

    rows.push(Row::header("INPUT / OUTPUT"));
    let external = a.mixer_mode == MixerMode::External;
    let offered = external_ok(channels) || external;
    rows.push(
        Row::chips(
            "mixer_mode",
            "MIXER MODE",
            &["INTERNAL", "EXTERNAL"],
            external as usize,
        )
        .enabled(offered)
        .hint(&hint_if(!offered, || {
            format!("needs 4+ channels, has {channels}")
        })),
    );
    if !offered {
        rows.push(Row::info(
            "EXTERNAL MIXER MODE",
            &format!(
                "unavailable: {} has {channels} output channels, it needs at least 4",
                device_name(p, &st)
            ),
        ));
    }
    if external {
        rows.push(Row::header("OUTPUT CHANNELS · EXTERNAL"));
        for (k, (name, cur)) in EXTERNAL_NAMES.iter().zip(a.externals()).enumerate() {
            rows.push(Row::dropdown_of(
                &format!("ext_{k}"),
                &format!("OUTPUT {name}"),
                numbered("OFF", pairs, cur),
                cur as usize,
            ));
        }
    } else {
        rows.push(Row::header("OUTPUT CHANNELS · INTERNAL"));
        rows.push(Row::dropdown_of(
            "out_master",
            "MASTER OUTPUT",
            pair_labels(pairs, a.out_master),
            pair_index(a.out_master),
        ));
        rows.push(Row::dropdown_of(
            "out_phones",
            "HEADPHONES OUTPUT",
            numbered("AUTO", pairs, a.out_phones),
            a.out_phones as usize,
        ));
        rows.push(Row::dropdown_of(
            "out_booth",
            "BOOTH OUTPUT",
            numbered("OFF", pairs, a.out_booth),
            a.out_booth as usize,
        ));
        rows.push(
            Row::dropdown_of(
                "booth_level",
                "BOOTH LEVEL",
                db_labels(&BOOTH_DB),
                nearest(&BOOTH_DB, a.booth_level),
            )
            .enabled(a.out_booth > 0),
        );
    }

    rows.push(Row::header("INPUT CHANNELS"));
    let ch = card::choices(&device::cards(true, a.output_show_all, &a.input_device));
    let at = ch
        .iter()
        .position(|(_, n)| *n == a.input_device)
        .unwrap_or(0);
    rows.push(Row::dropdown_of(
        "input_device",
        "INPUT DEVICE",
        ch.into_iter().map(|(l, _)| l).collect(),
        at,
    ));
    let in_caps = device::input_caps(&a.input_device);
    let (_, in_pairs) = pairs_of(in_caps.as_ref());
    rows.push(
        Row::dropdown_of(
            "input_pair",
            "INPUT CHANNELS",
            pair_labels(in_pairs, a.input_pair),
            pair_index(a.input_pair),
        )
        .enabled(in_caps.is_some()),
    );
    if in_caps.is_none() {
        rows.push(Row::info(
            "INPUT",
            "no such input device: nothing to record from",
        ));
    }
    rows.push(
        Row::chips(
            "rec_source",
            "REC SOURCE",
            &["MASTER", "INPUT"],
            a.rec_source as usize,
        )
        .hint("recordings"),
    );

    rows.push(Row::header("MICROPHONE INPUT"));
    rows.push(
        Row::check("mic_on", "Enable microphone input", a.mic_on)
            .hint("uses the input device above"),
    );
    rows.push(
        Row::dropdown_of(
            "mic_pair",
            "MIC CHANNELS",
            pair_labels(in_pairs, a.mic_pair),
            pair_index(a.mic_pair),
        )
        .enabled(a.mic_on && in_caps.is_some()),
    );
    rows.push(
        Row::dropdown_of(
            "mic_level",
            "MIC LEVEL",
            db_labels(&MIC_DB),
            nearest(&MIC_DB, a.mic_level),
        )
        .enabled(a.mic_on),
    );
    rows.push(
        Row::of("talkover_mode", "TALKOVER MODE", a.talkover_mode).hint("advanced: mid range only"),
    );
    let labels = TALK_DB
        .iter()
        .map(|&d| {
            if d == 0 {
                "OFF".to_owned()
            } else {
                db_label(d)
            }
        })
        .collect();
    rows.push(
        Row::dropdown_of(
            "talkover_level",
            "TALKOVER LEVEL",
            labels,
            nearest(&TALK_DB, a.talkover_level),
        )
        .enabled(a.mic_on),
    );
    if let Some(e) = mic::problem() {
        rows.push(Row::info("MICROPHONE", &e));
    }
    rows
}
