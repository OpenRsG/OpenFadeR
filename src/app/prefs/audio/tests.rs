use super::page::{numbered, pair_labels, value_options};
use super::*;

#[test]
fn settings_carry_every_field_to_the_engine() {
    let a = Audio {
        output_device: "X".into(),
        sample_rate: 96_000,
        buffer_size: 256,
        metronome_tone: 2,
        metronome_volume: 30,
        preview_volume: -6,
        mixer_mode: MixerMode::External,
        out_master: 2,
        out_phones: 3,
        out_booth: 4,
        booth_level: -3,
        out_deck1: 6,
        out_preview: 0,
        ..Audio::default()
    };
    let s = a.settings();
    assert_eq!(
        (s.device.as_str(), s.rate, s.buffer, s.mixer),
        ("X", 96_000, 256, Mixer::External)
    );
    assert_eq!(
        (s.master, s.phones, s.booth, s.external),
        (2, 3, 4, [6, 2, 3, 4, 5, 0])
    );
    assert_eq!((s.metronome_tone, s.metronome_volume), (2, 0.3));
    assert!((s.preview_gain - 0.501).abs() < 0.001 && (s.booth_gain - 0.708).abs() < 0.001);
}

#[test]
fn the_microphone_reaches_the_engine_only_when_on() {
    let mut p = Prefs::default();
    assert!(
        (
            p.audio.settings().mic_gain,
            p.audio.settings().talkover_gain
        ) == (0.0, 1.0)
    );
    assert!(
        choose(&mut p, "mic_on", 1)
            && choose(&mut p, "mic_level", 0)
            && choose(&mut p, "talkover_level", 2)
    );
    assert!(choose(&mut p, "talkover_mode", 1) && choose(&mut p, "mic_pair", 1));
    let s = p.audio.settings();
    assert!(
        (s.mic_gain - 0.063).abs() < 0.001 && (s.talkover_gain - 0.251).abs() < 0.001,
        "{s:?}"
    );
    assert!(!s.talkover_advanced && p.audio.mic_pair == 2);
}

#[test]
fn the_aux_ducking_and_input_reach_the_engine_and_the_pair_follows_the_input_device() {
    let mut p = Prefs::default();
    let s = p.audio.settings();
    assert_eq!(
        (s.aux_duck, s.aux_release),
        (1.0, 0.4),
        "no ducking until asked"
    );
    assert!(choose(&mut p, "aux_duck", 2) && choose(&mut p, "aux_release", 3));
    let s = p.audio.settings();
    assert!(
        (s.aux_duck - 0.251).abs() < 0.001 && s.aux_release == 1.5,
        "{s:?}"
    );
    p.audio.input_device = "interface".into();
    assert!(choose(&mut p, "aux_pair", 1));
    assert_eq!(
        p.audio.aux_source(),
        ("interface", 2),
        "no AUX device of its own: the input device"
    );
    p.audio.aux_device = "line".into();
    assert_eq!(p.audio.aux_source(), ("line", 2));
    assert!(
        choose(&mut p, "aux_device", 0),
        "the first entry is the input device again"
    );
    assert_eq!(p.audio.aux_source(), ("interface", 2));
}

#[test]
fn db_steps_convert_and_round_trip_through_the_labels() {
    assert_eq!((db_gain(0), db_gain(-99)), (1.0, 0.0));
    assert!((db_gain(6) - 1.995).abs() < 0.001);
    assert_eq!(
        (db_label(-99), db_label(0), db_label(3), db_label(-6)),
        ("MUTE".into(), "0 dB".into(), "+3 dB".into(), "-6 dB".into())
    );
    assert_eq!(
        (
            nearest(&PREVIEW_DB, 0),
            nearest(&PREVIEW_DB, -7),
            nearest(&PREVIEW_DB, -200)
        ),
        (7, 5, 0)
    );
}

#[test]
fn value_lists_show_only_supported_values_and_never_hide_a_saved_one() {
    let (o, at) = value_options(&[44_100, 48_000], 0, "Hz");
    assert_eq!(
        (o.as_slice(), at),
        (
            &["AUTO".to_owned(), "44100 Hz".into(), "48000 Hz".into()][..],
            0
        )
    );
    assert_eq!(value_options(&[44_100, 48_000], 48_000, "Hz").1, 2);
    let (o, at) = value_options(&[44_100], 96_000, "Hz");
    assert_eq!((o[at].as_str(), at), ("96000 Hz (not supported)", 2));
    assert_eq!(
        (
            value_at(&[44_100, 48_000], 0),
            value_at(&[44_100, 48_000], 2),
            value_at(&[44_100], 2)
        ),
        (Some(0), Some(48_000), None)
    );
    assert_eq!(
        value_at(&[], 1),
        None,
        "a device that reports nothing offers only AUTO"
    );
}

#[test]
fn pair_lists_follow_the_channel_count_and_mark_a_saved_pair_the_device_lacks() {
    assert_eq!(pair_labels(2, 1), ["1/2", "3/4"]);
    assert_eq!(
        pair_labels(1, 3),
        [
            "1/2",
            "3/4 (not on this device)",
            "5/6 (not on this device)"
        ]
    );
    let n = numbered("OFF", 2, 0);
    assert_eq!(
        (n[0].as_str(), n[2].as_str()),
        ("OFF", "3/4"),
        "the index is the pair number"
    );
    assert!(!external_ok(2) && external_ok(4));
}

#[test]
fn clicks_set_numbers_pairs_and_keep_the_mixer_mode_honest() {
    let mut p = Prefs::default();
    for (key, i) in [
        ("metronome_tone", 1),
        ("metronome_volume", 8),
        ("preview_volume", 4),
        ("booth_level", 6),
        ("out_master", 1),
        ("out_phones", 2),
        ("out_booth", 3),
        ("input_pair", 1),
        ("ext_2", 0),
        ("ext_5", 4),
    ] {
        assert!(choose(&mut p, key, i), "{key}");
    }
    let a = &p.audio;
    assert_eq!(
        (
            a.metronome_tone,
            a.metronome_volume,
            a.preview_volume,
            a.booth_level
        ),
        (1, 80, -9, 6)
    );
    assert_eq!(
        (
            a.out_master,
            a.out_phones,
            a.out_booth,
            a.input_pair,
            a.out_deck3,
            a.out_preview
        ),
        (2, 2, 3, 2, 0, 4)
    );
    assert!(
        choose(&mut p, "metronome_tone", 99) && p.audio.metronome_tone == 2,
        "clamped"
    );
    assert!(choose(&mut p, "rec_source", 1) && p.audio.rec_input());
    assert!(!choose(&mut p, "nonsense", 0) && !choose(&mut p, "ext_9", 0));
    assert!(choose(&mut p, "mixer_mode", 0) && p.audio.mixer_mode == MixerMode::Internal);
}

#[test]
fn automatic_buffer_follows_low_power_and_manual_values_win() {
    use crate::app::prefs::lowpower::LowPower;
    let mut p = Prefs::default();
    p.advanced.low_power = LowPower::Off;
    assert_eq!(
        effective_settings(&p).requested_buffer(),
        Some(openfader::audio::route::DEFAULT_BUFFER_FRAMES)
    );
    p.advanced.low_power = LowPower::On;
    assert_eq!(effective_settings(&p).requested_buffer(), None);
    p.audio.buffer_size = 256;
    assert_eq!(effective_settings(&p).requested_buffer(), Some(256));
}
