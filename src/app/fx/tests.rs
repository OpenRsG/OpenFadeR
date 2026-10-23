use super::*;

#[test]
fn follows_the_master_deck_while_it_plays() {
    let d = |a: [(bool, f32); 2]| [a[0], a[1], (false, 0.0), (false, 0.0)];
    assert_eq!(tempo(1, d([(true, 128.0), (true, 140.0)])), Some(140.0));
    assert_eq!(tempo(0, d([(true, 128.0), (true, 140.0)])), Some(128.0));
}

#[test]
fn falls_back_to_the_other_playing_deck_then_to_nothing() {
    let d = |a: [(bool, f32); 2]| [a[0], a[1], (false, 0.0), (false, 0.0)];
    assert_eq!(
        tempo(1, d([(true, 128.0), (false, 140.0)])),
        Some(128.0),
        "master stopped"
    );
    assert_eq!(
        tempo(0, d([(true, 0.0), (true, 140.0)])),
        Some(140.0),
        "master has no tempo"
    );
    assert_eq!(
        tempo(0, d([(false, 128.0), (false, 140.0)])),
        None,
        "nothing playing"
    );
    assert_eq!(tempo(-1, d([(true, 0.0), (false, 140.0)])), None);
}

#[test]
fn the_last_tempo_is_kept_while_no_deck_plays() {
    let last = Cell::new(None);
    assert_eq!(held_tempo(&last, None), None, "nothing has played yet");
    assert_eq!(held_tempo(&last, Some(128.0)), Some(128.0));
    assert_eq!(
        held_tempo(&last, None),
        Some(128.0),
        "the last deck paused: the tail rings on at 128, not at 120"
    );
    assert_eq!(held_tempo(&last, Some(140.0)), Some(140.0));
}

#[test]
fn decks_c_and_d_lead_and_follow_too() {
    let on = [(true, 120.0), (true, 124.0), (true, 128.0), (true, 132.0)];
    assert_eq!(
        [2, 3, 1, 0].map(|m| tempo(m, on)),
        [Some(128.0), Some(132.0), Some(124.0), Some(120.0)]
    );
    let only_d = [
        (false, 120.0),
        (false, 124.0),
        (false, 128.0),
        (true, 132.0),
    ];
    assert_eq!(
        tempo(0, only_d),
        Some(132.0),
        "a lone playing deck D is the tempo"
    );
    assert_eq!(
        tempo(9, only_d),
        Some(132.0),
        "an out-of-range master is ignored"
    );
}

#[test]
fn assignment_offers_only_the_shown_channels_and_master() {
    assert_eq!(
        [0, 1, 2, 3, 4, 5, 6, -1].map(|i| assignment(i, 4, false)),
        [
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            None,
            None,
            None
        ]
    );
    assert_eq!(
        [0, 1, 2, 3, 4].map(|i| assignment(i, 2, false)),
        [Some(0), Some(1), None, None, Some(4)],
        "CH3 / CH4 need the 4-deck layout"
    );
    assert_eq!(
        [4, 5].map(|i| assignment(i, 2, true)),
        [Some(4), Some(5)],
        "AUX needs the AUX input"
    );
}

#[test]
fn a_unit_follows_its_channel_deck_in_ch_the_master_in_mst_and_tap_overrides_both() {
    // deck 1 at 128, deck 2 at 120 and started last (master = deck 1, index 0 asked first)
    let decks = [(true, 128.0), (true, 120.0), (false, 0.0), (false, 0.0)];
    let master = tempo(0, decks);
    let on_ch2 = |master_tempo, over| unit_bpm(over, follow(1, master_tempo, master, decks));
    assert_eq!(on_ch2(false, None), (120.0, false), "CH: deck 2");
    assert_eq!(on_ch2(true, None), (128.0, false), "MST: the master deck");
    assert_eq!(on_ch2(false, Some(90.0)), (90.0, false), "TAP beats CH");
    assert_eq!(on_ch2(true, Some(90.0)), (90.0, false), "and MST");
    assert_eq!(
        follow(4, false, master, decks),
        Some(128.0),
        "CH on MASTER is MST"
    );
    // a CH deck that does not play, or has no tempo, gives none: the unit keeps its last (held_tempo)
    let stopped = [(true, 128.0), (false, 120.0), (false, 0.0), (false, 0.0)];
    assert_eq!(follow(1, false, tempo(0, stopped), stopped), None);
    assert_eq!(follow(0, false, None, [(true, 0.0); 4]), None);
    assert_eq!(unit_bpm(None, None), (FALLBACK_BPM, true));
    let last = Cell::new(None);
    assert_eq!(
        held_tempo(&last, follow(1, false, master, decks)),
        Some(120.0)
    );
    assert_eq!(
        held_tempo(&last, follow(1, false, tempo(0, stopped), stopped)),
        Some(120.0),
        "deck 2 paused: the tail keeps 120, not deck 1's 128"
    );
}

#[test]
fn release_is_a_momentary_echo_on_slot_1_that_silences_the_other_slots() {
    let slot = |effect: Effect, on| FxSlot {
        effect: effect.index() as i32,
        level: 0.2,
        on,
        ..FxSlot::default()
    };
    let mut u = FxUnit {
        slots: ModelRc::new(VecModel::from(vec![
            slot(Effect::Phaser, false),
            slot(Effect::Trans, true),
            slot(Effect::Roll, false),
        ])),
        ..FxUnit::default()
    };
    release(&mut u, true);
    let s: Vec<FxSlot> = u.slots.iter().collect();
    assert!(
        u.releasing && s[0].on && s[0].effect == Effect::Echo.index() as i32 && s[0].level == 1.0
    );
    assert!(!s[1].on && !s[2].on, "the other slots stop");
    release(&mut u, false);
    assert!(!u.releasing && u.slots.iter().all(|s| !s.on));
}

#[test]
fn release_units_1_sends_a_held_release_to_the_master_and_2_keeps_the_assignment() {
    assert_eq!(
        [(0, true, 1), (1, true, 1), (4, true, 1)].map(|(a, r, n)| effective_assign(a, r, n)),
        [4, 4, 4],
        "RELEASE FX UNITS 1 = master"
    );
    assert_eq!(
        [(0, true, 2), (1, true, 2)].map(|(a, r, n)| effective_assign(a, r, n)),
        [0, 1],
        "2 = each unit's own assign"
    );
    assert_eq!(
        [(0, false, 1), (2, false, 2)].map(|(a, r, n)| effective_assign(a, r, n)),
        [0, 2],
        "not held: always the assignment"
    );
}

#[test]
fn only_units_that_are_releasing_silence_color_fx_and_only_when_asked() {
    let u = |releasing| FxUnit {
        releasing,
        ..FxUnit::default()
    };
    assert!(release_cfx_off([u(false), u(true)], true));
    assert!(!release_cfx_off([u(false), u(true)], false));
    assert!(!release_cfx_off([u(false), u(false)], true));
}

#[test]
fn merge_select_up_and_down_cycle_the_four_presets() {
    let mut u = FxUnit::default();
    let names: Vec<String> = (0..5)
        .map(|_| {
            merge_step(&mut u, 1);
            u.merge_name.to_string()
        })
        .collect();
    assert_eq!(
        names,
        [
            PRESETS[1].name,
            PRESETS[2].name,
            PRESETS[3].name,
            PRESETS[0].name,
            PRESETS[1].name
        ]
    );
    let mut u = FxUnit::default();
    merge_step(&mut u, -1);
    assert_eq!(
        (u.merge_preset, u.merge_name.as_str()),
        (3, PRESETS[3].name),
        "down from the first wraps to the last"
    );
}

#[test]
fn part_masks_cover_the_stems_and_three_stems_fold_bass_into_inst() {
    assert_eq!(
        [0, 1, 2, 3, 4].map(|p| part_mask(p, true)),
        [0, 0b0001, 0b0010, 0b0100, 0b1000]
    );
    assert_eq!(
        [2, 3].map(|p| part_mask(p, false)),
        [0b0110, 0],
        "INST carries bass, no BASS part"
    );
    assert_eq!(part_mask(9, true), 0);
}

#[test]
fn the_part_chip_cycles_and_skips_bass_with_three_stems() {
    let cycle = |four| {
        std::iter::successors(Some(0), |&p| Some(next_part(p, four)))
            .take(6)
            .collect::<Vec<_>>()
    };
    assert_eq!(cycle(true), [0, 1, 2, 3, 4, 0]);
    assert_eq!(cycle(false), [0, 1, 2, 4, 0, 1]);
}

#[test]
fn a_controller_part_button_selects_and_pressed_again_clears() {
    assert_eq!(
        [
            toggle_part(0, 1, true),
            toggle_part(1, 1, true),
            toggle_part(1, 4, true)
        ],
        [1, 0, 4]
    );
    assert_eq!(
        [toggle_part(0, 3, false), toggle_part(0, 9, true)],
        [0, 0],
        "no BASS with 3 stems, no such part"
    );
}
