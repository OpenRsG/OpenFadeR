use super::dialog::PAGES;
use super::file::{load_from, path_from, save_to};
use super::live::update_with;
use super::*;
use std::{cell::RefCell, rc::Rc};

#[test]
fn round_trips_every_field_and_keeps_unknown_keys() {
    let mut p = Prefs {
        key_notation: KeyNotation::OpenKey,
        waveform_colors: WaveColors::Blue,
        tempo_range_default: 3,
        quantize_default: false,
        key_lock_default: true,
        crossfader_curve: CrossfaderCurve::Sharp,
        window_w: 1280,
        window_h: 1000,
        fx_effect: 4,
        fx_beat: 8,
        fx_tempo_master: true,
        decks: 4,
        stems: true,
        stems_four: false,
        active_part_solo: true,
        stems_reset: false,
        stems_fast: true,
        stem_wave: false,
        controller_audio: false,
        unknown: vec![("future_key".into(), "a = b".into())],
        ..Default::default()
    };
    p.set_extra("skin", "light");
    p.view.wave_order = 4321;
    p.view.draw_rate = view::DrawRate::Fps30;
    p.view.tempo_decimals = view::TempoDecimals::One;
    p.view.tempo_reverse = true;
    p.controller.tempo_invert = true;
    let (back, notes) = Prefs::parse(&p.to_text());
    assert_eq!(back, p);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        Prefs::parse(&Prefs::default().to_text()),
        (Prefs::default(), vec![])
    );
}

#[test]
fn bad_input_resets_that_value_and_notes_it() {
    let text = "# comment\nkey_notation = klingon\nwaveform_colors = RGB\ntempo_range_default = 9\n\
                quantize_default = maybe\nwindow_w = -5\nfx_effect = ECHO\nfx_beat = 3/5\n\
                crossfader_curve=smooth\nnot a setting\nkey_notation = camelot\nwindow_h = x\nwindow_h = 800\n";
    let (p, notes) = Prefs::parse(text);
    let d = Prefs::default();
    assert_eq!(
        (p.waveform_colors, p.crossfader_curve, p.key_notation),
        (
            WaveColors::Rgb,
            CrossfaderCurve::Smooth,
            KeyNotation::Camelot
        )
    );
    assert_eq!(
        (
            p.tempo_range_default,
            p.quantize_default,
            p.window_w,
            p.fx_beat
        ),
        (0, d.quantize_default, 0, d.fx_beat)
    );
    assert_eq!(
        (p.fx_effect, p.window_h),
        (0, 800),
        "a later valid line wins over an earlier bad one"
    );
    assert!(
        notes.iter().any(|n| n.starts_with("key_notation")),
        "{notes:?}"
    );
    assert_eq!(notes.len(), 7, "{notes:?}"); // 6 bad values + the `not a setting` line
    assert!(p.unknown.is_empty());
    assert_eq!(Prefs::parse(""), (d, vec![]));
}

#[test]
fn section_values_parse_and_a_bad_one_is_noted() {
    let (p, notes) =
        Prefs::parse("skin = light\nline_space = huge\ntooltips = false\nwave_order = 2143");
    assert_eq!(
        (view::skin_name(&p), p.view.line_space, p.view.tooltips),
        ("light", view::LineSpace::Medium, false)
    );
    assert_eq!(
        (p.view.wave_order(), notes.len()),
        ([1, 0, 3, 2], 1),
        "{notes:?}"
    );
    let mut v = Prefs::default().view;
    v.wave_order = 1123; // not a permutation
    assert_eq!(v.wave_order(), [0, 1, 2, 3]);
}

#[test]
fn extra_settings_are_stored_in_the_file() {
    let mut p = Prefs::default();
    p.set_extra("rec_dir", "/tmp/x");
    p.set_extra("rec_on", true);
    p.set_extra("rec_dir", "/tmp/y");
    let (back, notes) = Prefs::parse(&p.to_text());
    assert_eq!(
        (back.extra("rec_dir"), back.extra_bool("rec_on", false)),
        (Some("/tmp/y"), true),
        "{notes:?}"
    );
}

#[test]
fn every_row_of_every_page_is_clickable() {
    let p = Prefs::default();
    for (rows, _) in PAGES {
        for r in rows(&p).iter().filter(|r| {
            matches!(
                r.kind,
                rows::Kind::Chips | rows::Kind::Dropdown | rows::Kind::Check
            )
        }) {
            assert!(
                PAGES.iter().any(|(_, c)| c(&mut p.clone(), &r.key, 0))
                    || p.clone().choose(&r.key, 0),
                "no handler for row {:?}",
                r.key
            );
        }
    }
}

#[test]
fn save_and_load_use_the_file() {
    let dir = std::env::temp_dir().join(format!("openfader-prefs-{}", std::process::id()));
    let file = path_from(|k| (k == "XDG_CONFIG_HOME").then(|| dir.display().to_string())).unwrap();
    assert_eq!(file, dir.join("openfader").join("settings.conf"));
    assert_eq!(load_from(&file), (Prefs::default(), vec![]));
    let p = Prefs {
        key_notation: KeyNotation::Classic,
        ..Default::default()
    };
    save_to(&file, &p).unwrap();
    assert_eq!(load_from(&file), (p, vec![]));
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        path_from(|k| Some(String::new()).filter(|_| k != "HOME")),
        None
    );
}

#[test]
fn dialog_choices_edit_one_field_and_listeners_hear_real_changes() {
    let mut p = Prefs::default();
    p.choose("key", 1);
    p.choose("wave", 2);
    p.choose("curve", 0);
    p.choose("range", 2);
    p.choose("quantize", 1);
    p.choose("keylock", 0);
    p.choose("key", 7); // out of range
    p.choose("range", 4);
    assert!(!p.choose("nonsense", 0));
    p.choose("decks", 1);
    assert_eq!(p.decks, 4);
    p.choose("decks", 0);
    assert_eq!(p.decks, 2);
    assert_eq!(
        (p.key_notation, p.waveform_colors, p.crossfader_curve),
        (
            KeyNotation::Classic,
            WaveColors::Blue,
            CrossfaderCurve::Smooth
        )
    );
    assert_eq!(
        (
            p.tempo_range_default,
            p.quantize_default,
            p.key_lock_default
        ),
        (2, false, true)
    );

    let heard = Rc::new(RefCell::new(Vec::new()));
    let h = heard.clone();
    on_change(move |p| h.borrow_mut().push(p.key_notation));
    let saved = RefCell::new(0);
    update_with(
        |p| p.key_notation = KeyNotation::OpenKey,
        |_| *saved.borrow_mut() += 1,
    );
    update_with(
        |p| p.key_notation = KeyNotation::OpenKey,
        |_| *saved.borrow_mut() += 1,
    ); // no change
    assert_eq!(
        (heard.borrow().clone(), *saved.borrow()),
        (vec![KeyNotation::OpenKey], 1)
    );
    assert_eq!(get().key_notation, KeyNotation::OpenKey);
}

#[test]
fn deck_count_is_two_or_four() {
    assert_eq!(Prefs::parse("decks = 4").0.decks, 4);
    let (p, notes) = Prefs::parse("decks = 3");
    assert_eq!((p.decks, notes.len()), (2, 1), "{notes:?}");
}

#[test]
fn stems_settings_parse_choose_and_reject_bad_values() {
    let (p, notes) = Prefs::parse(
        "stems = true\nstems_mode = 3\nactive_part = SOLO\nstems_reset_on_load = false",
    );
    assert_eq!(
        (
            p.stems,
            p.stems_four,
            p.active_part_solo,
            p.stems_reset,
            notes.len()
        ),
        (true, false, true, false, 0)
    );
    let (p, notes) = Prefs::parse("stems_mode = 5\nactive_part = both\nstems = maybe");
    assert_eq!(
        (p.stems, p.stems_four, p.active_part_solo, notes.len()),
        (false, true, false, 3),
        "{notes:?}"
    );
    let mut p = Prefs::default();
    p.choose("stems", 1);
    p.choose("stems_mode", 0);
    p.choose("active_part", 1);
    p.choose("stems_reset", 1);
    assert_eq!(
        (p.stems, p.stems_four, p.active_part_solo, p.stems_reset),
        (true, false, true, false)
    );
}

#[test]
fn fader_curve_and_color_fx_knob_are_stored_by_name_and_unknown_names_read_as_the_defaults() {
    use controller::{Fader, KnobMode};
    let mut p = Prefs::default();
    assert!(controller::choose(&mut p, "fader_curve", 2)); // GRADUAL, the last built-in
    let text = p.to_text();
    assert!(
        text.contains("fader_curve = gradual\n") && text.contains("cfx_knob = normal\n"),
        "{text}"
    );
    assert_eq!(Prefs::parse(&text), (p.clone(), vec![]));
    // The file may spell a name in any case; one that no list has is noted and reads as the default.
    let (q, notes) = Prefs::parse("fader_curve = STEEP\ncfx_knob = NORMAL");
    assert_eq!(
        (q.controller.fader_curve, q.controller.cfx_knob, notes.len()),
        (Fader(0), KnobMode::NORMAL, 0)
    );
    let (q, notes) = Prefs::parse("fader_curve = gone\ncfx_knob = vanished");
    assert_eq!(
        (q.controller.fader_curve, q.controller.cfx_knob),
        (Fader::LINEAR, KnobMode::NORMAL)
    );
    assert_eq!(notes.len(), 2, "{notes:?}");
    // A click past the list changes nothing.
    assert!(
        controller::choose(&mut p, "cfx_knob", 99) && p.controller.cfx_knob == KnobMode::NORMAL
    );
}
