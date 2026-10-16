use super::*;

fn bind(s: &str) -> Bind {
    parse_bind(s).unwrap().unwrap()
}

fn row<'a>(rows: &'a [Row], id: &str) -> &'a Row {
    rows.iter().find(|r| r.id == id).unwrap()
}

#[test]
fn default_table_is_empty_text_with_unique_ids_and_no_conflicts() {
    assert_eq!(Keymap::default().to_text(), "");
    let rows = Keymap::parse("").0.rows();
    assert_eq!(rows.len(), TABLE.len());
    let ids: std::collections::HashSet<_> = rows.iter().map(|r| &r.id).collect();
    assert_eq!(ids.len(), rows.len(), "ids are unique");
    for r in &rows {
        assert!(
            conflicts(&rows, &r.id, r.bind.as_ref().unwrap()).is_empty(),
            "{} clashes",
            r.id
        );
    }
    assert_eq!(row(&rows, "deck.play_pause.A").bind, Some(bind("space")));
    assert_eq!(row(&rows, "app.help").bind, Some(bind("?")));
}

#[test]
fn overrides_apply_unknown_ids_stay_and_bad_bindings_are_reported() {
    let (km, notes) = Keymap::parse(
        "deck.cue.A=q; deck.pad.B=shift+a-h; app.help=; future.thing=ctrl+9; deck.slip.A=shift+_; nonsense; deck.brake.A=ctrl+f+g; preset=weird",
    );
    assert_eq!(notes.len(), 4, "{notes:?}");
    let rows = km.rows();
    assert_eq!(row(&rows, "deck.cue.A").bind, Some(bind("q")));
    assert_eq!(row(&rows, "deck.pad.B").bind, Some(bind("shift+a-h")));
    assert_eq!(row(&rows, "app.help").bind, None, "empty = cleared");
    assert_eq!(
        row(&rows, "deck.slip.A").bind,
        Some(bind("v")),
        "a bad binding leaves the default"
    );
    assert!(
        km.to_text().contains("future.thing=ctrl+9"),
        "unknown ids are preserved"
    );
    assert!(km.is_custom(row(&rows, "deck.cue.A")) && !km.is_custom(row(&rows, "deck.cue.B")));
}

#[test]
fn text_round_trips() {
    let mut km = Keymap::default();
    km.use_preset(Preset::LeftHanded);
    km.set("deck.cue.A", Some(bind("ctrl+shift+space")));
    km.set("deck.key_up.B", Some(bind("ctrl+plus")));
    km.set("app.tag_track", Some(bind("semicolon")));
    km.set("app.help", None);
    km.set("deck.pad.A", Some(bind("a-h")));
    let text = km.to_text();
    assert!(text.starts_with("preset=left-handed;"), "{text}");
    let (back, notes) = Keymap::parse(&text);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(back, km);
    assert_eq!(back.to_text(), text);
}

#[test]
fn setting_the_preset_default_removes_the_override() {
    let mut km = Keymap::default();
    km.set("deck.cue.A", Some(bind("q")));
    assert_eq!(km.to_text(), "deck.cue.A=q");
    km.set("deck.cue.A", Some(bind("z")));
    assert_eq!(km.to_text(), "");
    km.set("app.mystery", Some(bind("9")));
    km.use_preset(Preset::LeftHanded);
    assert_eq!(
        km.to_text(),
        "preset=left-handed;app.mystery=9",
        "reset keeps unknown ids"
    );
}

#[test]
fn left_handed_mirrors_letters_only() {
    let (km, _) = Keymap::parse("preset=left-handed");
    let rows = km.rows();
    assert_eq!(row(&rows, "deck.auto_loop.A").bind, Some(bind("a")));
    assert_eq!(row(&rows, "deck.cue.A").bind, Some(bind("m")));
    assert_eq!(row(&rows, "deck.auto_loop.B").bind, Some(bind("shift+a")));
    assert_eq!(
        row(&rows, "app.focus_search").bind,
        Some(bind("ctrl+f")),
        "Ctrl chords stay"
    );
    assert_eq!(row(&rows, "deck.play_pause.A").bind, Some(bind("space")));
    assert_eq!(row(&rows, "deck.pad.A").bind, Some(bind("1-8")));
    for r in &rows {
        assert!(
            conflicts(&rows, &r.id, r.bind.as_ref().unwrap()).is_empty(),
            "{} clashes",
            r.id
        );
    }
}

#[test]
fn conflicts_see_ranges_named_keys_and_modifiers() {
    let rows = Keymap::default().rows();
    let hits = |id: &str, b: &str| {
        conflicts(&rows, id, &bind(b))
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(hits("deck.cue.A", "space"), ["deck.play_pause.A"]);
    assert_eq!(
        hits("deck.cue.A", "5"),
        ["deck.pad.A"],
        "inside the 1-8 range"
    );
    assert!(hits("deck.cue.A", "9").is_empty());
    assert!(hits("deck.cue.A", "ctrl+q").is_empty());
    assert_eq!(hits("deck.cue.A", "ctrl+z"), ["app.undo"], "Ctrl+Z is undo");
    assert_eq!(
        hits("deck.cue.A", "s"),
        ["deck.sync.A"],
        "a single letter is not a name prefix"
    );
    assert!(
        hits("deck.cue.A", "z").is_empty(),
        "its own key is no conflict"
    );
}

#[test]
fn capture_reads_assign_clear_cancel_and_ignores_modifiers() {
    assert_eq!(capture("q", false, false), Capture::Assign(bind("q")));
    assert_eq!(capture("Q", true, false), Capture::Assign(bind("shift+q")));
    assert_eq!(capture("!", true, false), Capture::Assign(bind("shift+1")));
    assert_eq!(capture("_", true, false), Capture::Assign(bind("_")));
    assert_eq!(
        capture("\u{5}", false, true),
        Capture::Assign(bind("ctrl+e")),
        "Ctrl+E as a control character"
    );
    assert_eq!(
        capture(" ", true, true),
        Capture::Assign(bind("ctrl+shift+space"))
    );
    assert_eq!(
        capture("\u{f703}", true, false),
        Capture::Assign(bind("right"))
    );
    assert_eq!(capture("\u{8}", false, false), Capture::Clear);
    assert_eq!(capture("\u{7f}", false, false), Capture::Clear);
    assert_eq!(capture("\u{1b}", false, false), Capture::Cancel);
    for ignored in ["", "\u{10}", "\u{11}", "\u{f700}", "\t", "ab"] {
        assert_eq!(
            capture(ignored, false, false),
            Capture::Ignore,
            "{ignored:?}"
        );
    }
}

#[test]
fn pad_rows_take_the_pressed_key_as_the_start_of_the_range() {
    assert_eq!(fit(Act::Pad, bind("1")).unwrap().keys, "1-8");
    assert_eq!(fit(Act::Pad, bind("shift+a")).unwrap(), bind("shift+a-h"));
    assert!(
        fit(Act::Pad, bind("9")).is_err()
            && fit(Act::Pad, bind("z")).is_err()
            && fit(Act::Pad, bind("space")).is_err()
    );
    assert_eq!(fit(Act::Cue, bind("9")).unwrap(), bind("9"));
}

#[test]
fn bindings_a_press_can_never_match_are_refused_and_labels_read_well() {
    for bad in [
        "shift+_",
        "!",
        "shift+left",
        "ctrl+",
        "z-a",
        "hyper+z",
        "ctrl+f+g",
        "up",
        "tab",
    ] {
        assert!(parse_bind(bad).is_err(), "{bad}");
    }
    assert_eq!(bind("A-H"), bind("a-h"), "upper case is folded");
    assert_eq!(format_bind(&bind("ctrl+semicolon")), "ctrl+semicolon");
    assert_eq!(
        super::super::label(&bind("ctrl+shift+space")),
        "Ctrl+Shift+Space"
    );
    assert_eq!(super::super::label(&bind("1-8")), "1-8");
}
