use super::*;

fn hit(text: &str, shift: bool, ctrl: bool) -> Option<(Act, Option<i32>, usize)> {
    lookup(text, shift, ctrl).map(|(r, n)| (r.act, r.deck, n))
}

#[test]
fn modifiers_pick_the_deck() {
    assert_eq!(hit(" ", false, false), Some((Act::PlayPause, Some(0), 0)));
    assert_eq!(hit(" ", true, false), Some((Act::PlayPause, Some(1), 0)));
    assert_eq!(hit("\n", false, false), Some((Act::Load, Some(0), 0)));
    assert_eq!(hit("\r", true, false), Some((Act::Load, Some(1), 0)));
}

#[test]
fn ctrl_picks_decks_c_and_d_and_d_switches_the_layout() {
    assert_eq!(hit(" ", false, true), Some((Act::PlayPause, Some(2), 0)));
    assert_eq!(hit(" ", true, true), Some((Act::PlayPause, Some(3), 0)));
    assert_eq!(
        hit("\u{18}", false, true),
        Some((Act::Cue, Some(2), 0)),
        "Ctrl+X arrives as a control character"
    );
    assert_eq!(hit("X", true, true), Some((Act::Cue, Some(3), 0)));
    assert_eq!(hit("\u{13}", false, true), Some((Act::Sync, Some(2), 0)));
    assert_eq!(hit("\n", true, true), Some((Act::Load, Some(3), 0)));
    assert_eq!(hit("d", false, false), Some((Act::DeckLayout, None, 0)));
    let (r, _) = lookup("x", true, true).unwrap();
    assert!(released(&r), "CUE on C / D previews while held");
}

#[test]
fn letters_ignore_case() {
    assert_eq!(hit("z", false, false), Some((Act::Cue, Some(0), 0)));
    assert_eq!(hit("X", false, false), Some((Act::Cue, Some(1), 0)));
    assert_eq!(hit("l", false, false), Some((Act::AutoLoop, Some(0), 0)));
    assert_eq!(hit("L", true, false), Some((Act::AutoLoop, Some(1), 0)));
    assert_eq!(hit("s", false, false), Some((Act::Sync, Some(0), 0)));
    assert_eq!(hit("S", true, false), Some((Act::Sync, Some(1), 0)));
}

#[test]
fn ctrl_e_exports_the_loop_of_deck_a_and_ctrl_shift_e_of_deck_b() {
    assert_eq!(hit("e", false, true), Some((Act::ExportLoop, Some(0), 0)));
    assert_eq!(
        hit("\u{5}", false, true),
        Some((Act::ExportLoop, Some(0), 0))
    );
    assert_eq!(hit("E", true, true), Some((Act::ExportLoop, Some(1), 0)));
    assert_eq!(
        hit("e", false, false),
        Some((Act::Reverse, Some(0), 0)),
        "plain E is still reverse"
    );
}

#[test]
fn digits_are_pads_and_shifted_digits_are_deck_b() {
    assert_eq!(hit("1", false, false), Some((Act::Pad, Some(0), 0)));
    assert_eq!(hit("8", false, false), Some((Act::Pad, Some(0), 7)));
    assert_eq!(hit("!", true, false), Some((Act::Pad, Some(1), 0)));
    assert_eq!(hit("*", true, false), Some((Act::Pad, Some(1), 7)));
    assert_eq!(hit("9", false, false), None);
}

#[test]
fn ctrl_f_arrives_as_letter_or_control_char() {
    assert_eq!(hit("f", false, true), Some((Act::FocusSearch, None, 0)));
    assert_eq!(hit("\u{6}", false, true), Some((Act::FocusSearch, None, 0)));
    assert_eq!(
        hit("z", false, true),
        Some((Act::Undo, None, 0)),
        "Ctrl+Z is not CUE"
    );
    assert_eq!(hit("\u{1a}", true, true), Some((Act::Redo, None, 0)));
    assert_eq!(hit("\u{19}", false, true), Some((Act::RedoAlt, None, 0)));
}

#[test]
fn question_mark_ignores_shift() {
    assert_eq!(hit("?", true, false), Some((Act::Help, None, 0)));
    assert_eq!(hit("?", false, false), Some((Act::Help, None, 0)));
}

#[test]
fn unknown_or_multi_char_keys_are_ignored() {
    assert_eq!(hit("j", false, false), None);
    assert_eq!(hit("", false, false), None);
    assert_eq!(hit("ab", false, false), None);
    assert_eq!(
        hit("\u{f700}", false, false),
        None,
        "up / down belong to the table"
    );
}

#[test]
fn every_binding_is_unique_and_labelled() {
    let rows = Keymap::default().rows();
    let mut seen = std::collections::HashSet::new();
    for r in &rows {
        let b = r.bind.as_ref().unwrap();
        assert!(
            seen.insert((b.keys.clone(), b.shift, b.ctrl)),
            "duplicate {}",
            label(b)
        );
    }
    let lines: Vec<_> = rows
        .iter()
        .map(|r| (help_line(r).keys.to_string(), help_line(r).what.to_string()))
        .collect();
    assert_eq!(lines[0], ("Space".into(), "play / pause — deck A".into()));
    assert_eq!(lines[1].0, "Shift+Space");
    assert_eq!(
        lines[lines.len() - 2],
        ("Ctrl+F".into(), "focus the search box".into())
    );
}

#[test]
fn loop_and_jump_keys() {
    assert_eq!(hit("i", false, false), Some((Act::LoopIn, Some(0), 0)));
    assert_eq!(hit("O", true, false), Some((Act::LoopOut, Some(1), 0)));
    assert_eq!(hit("r", false, false), Some((Act::Reloop, Some(0), 0)));
    assert_eq!(
        hit("[", false, false),
        Some((Act::BeatJump(-4), Some(0), 0))
    );
    assert_eq!(hit("{", true, false), Some((Act::BeatJump(-4), Some(1), 0)));
    assert_eq!(hit("}", true, false), Some((Act::BeatJump(4), Some(1), 0)));
}

#[test]
fn key_slip_and_pad_mode_keys() {
    assert_eq!(hit("k", false, false), Some((Act::KeySync, Some(0), 0)));
    assert_eq!(hit("K", true, false), Some((Act::KeySync, Some(1), 0)));
    // Shift+- / Shift+= arrive as `_` / `+`, Shift+0 as `)`: deck B
    assert_eq!(hit("-", false, false), Some((Act::KeyStep(-1), Some(0), 0)));
    assert_eq!(hit("=", false, false), Some((Act::KeyStep(1), Some(0), 0)));
    assert_eq!(hit("_", true, false), Some((Act::KeyStep(-1), Some(1), 0)));
    assert_eq!(hit("+", true, false), Some((Act::KeyStep(1), Some(1), 0)));
    assert_eq!(hit("0", false, false), Some((Act::KeyReset, Some(0), 0)));
    assert_eq!(hit(")", true, false), Some((Act::KeyReset, Some(1), 0)));
    assert_eq!(hit("v", false, false), Some((Act::Slip, Some(0), 0)));
    assert_eq!(hit("B", true, false), Some((Act::Brake, Some(1), 0)));
    assert_eq!(hit("e", false, false), Some((Act::Reverse, Some(0), 0)));
    assert_eq!(hit("M", true, false), Some((Act::PadMode, Some(1), 0)));
    assert_eq!(hit("t", false, false), Some((Act::TagTrack, None, 0)));
    assert_eq!(hit("f", false, false), Some((Act::FxToggle, None, 0)));
    assert_eq!(hit("w", false, false), Some((Act::BrowseView, None, 0)));
    assert_eq!(hit("W", true, false), Some((Act::BrowseArea, None, 0)));
    assert_eq!(hit("p", false, false), Some((Act::SamplerToggle, None, 0)));
    assert_eq!(hit("a", false, false), Some((Act::AutomixToggle, None, 0)));
    assert_eq!(hit("A", true, false), Some((Act::AutomixSkip, None, 0)));
    assert_eq!(
        hit("\u{f702}", false, false),
        Some((Act::FxBeat(-1), None, 0))
    );
    assert_eq!(
        hit("\u{f703}", true, false),
        Some((Act::FxBeat(1), None, 0)),
        "Shift does not matter for arrows"
    );
}

#[test]
fn grid_keys() {
    assert_eq!(hit("g", false, false), Some((Act::GridToggle, Some(0), 0)));
    assert_eq!(hit("G", true, false), Some((Act::GridToggle, Some(1), 0)));
    assert_eq!(hit("y", false, false), Some((Act::GridTap, Some(0), 0)));
    assert_eq!(hit("N", true, false), Some((Act::GridBeat1, Some(1), 0)));
    assert_eq!(
        hit(",", false, false),
        Some((Act::GridShift(-1), Some(0), 0))
    );
    assert_eq!(
        hit(".", false, false),
        Some((Act::GridShift(1), Some(0), 0))
    );
    assert_eq!(
        hit("<", true, false),
        Some((Act::GridShift(-1), Some(1), 0))
    );
    assert_eq!(hit(">", true, false), Some((Act::GridShift(1), Some(1), 0)));
}

#[test]
fn only_pad_and_cue_keys_have_a_release_action() {
    let held = |t, s| lookup(t, s, false).is_some_and(|(r, _)| released(&r));
    assert!(
        held("1", false)
            && held("!", true)
            && held("[", false)
            && held("z", false)
            && held("x", false)
    );
    assert!(!held("k", false) && !held(" ", false) && !held("q", false));
}

#[test]
fn shifted_key_released_after_shift_still_returns_deck_b() {
    let mut held = Held::new();
    for (down, up) in [("!", "1"), ("{", "["), ("*", "8"), ("}", "]")] {
        let (r, _) = lookup(down, true, false).unwrap();
        hold(&mut held, down, &r);
        // Shift is already up: the plain lookup would hit deck A (or nothing)
        let got = release(&mut held, up, false, false).expect(down);
        assert_eq!((got.deck, got.act), (r.deck, r.act));
        assert_eq!(got.deck, Some(1));
    }
    assert!(held.is_empty());
    assert!(release(&mut held, "k", false, false).is_none());
}

#[test]
fn master_keys() {
    assert_eq!(hit("q", false, false), Some((Act::Master, Some(0), 0)));
    assert_eq!(hit("Q", true, false), Some((Act::Master, Some(1), 0)));
}

#[test]
fn a_user_keymap_rebinds_clears_and_keeps_the_rest() {
    let rows = Keymap::parse("deck.cue.A=q;deck.master.A=;deck.pad.A=a-h")
        .0
        .rows();
    let go = |t, s| find(&rows, t, s, false).map(|(r, n)| (r.act, r.deck, n));
    assert_eq!(go("q", false), Some((Act::Cue, Some(0), 0)));
    assert_eq!(go("z", false), None, "the old key is free");
    assert_eq!(go("h", false), Some((Act::Pad, Some(0), 7)));
    assert_eq!(go("1", false), None);
    assert_eq!(
        go("Q", true),
        Some((Act::Master, Some(1), 0)),
        "deck B's master key is untouched"
    );
}
