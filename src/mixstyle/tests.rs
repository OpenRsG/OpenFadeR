use super::*;

fn style(name: &str) -> Recipe {
    builtin()
        .iter()
        .find(|r| r.name == name)
        .cloned()
        .expect(name)
}

/// The style's timeline over `bars` bars (its own length when it fixes one).
fn timeline(name: &str, bars: u32) -> Timeline {
    let r = style(name);
    r.timeline(r.bars.unwrap_or(bars) as f64 * 4.0)
}

fn near(a: Option<f32>, b: f32) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-4)
}

#[test]
fn every_builtin_is_valid_named_once_and_ends_with_the_incoming_deck_flat_on_its_side() {
    let all = builtin();
    assert_eq!(all[0].name, "SMOOTH", "today's style stays the first");
    assert!(all.len() >= 10);
    for (i, r) in all.iter().enumerate() {
        r.validate().unwrap();
        assert!(
            !all[..i].iter().any(|o| o.name == r.name),
            "{} twice",
            r.name
        );
        let t = r.timeline(r.bars.unwrap_or(16) as f64 * 4.0);
        let end = t.at(t.len());
        assert_eq!(end.crossfader, 1.0, "{} ends on the incoming deck", r.name);
        for (c, n) in Ctl::ALL.iter().zip(Ctl::NEUTRAL) {
            assert!(
                end.get(Side::In, *c).is_none_or(|v| v == n),
                "{} leaves the incoming {c:?} off neutral",
                r.name
            );
        }
    }
}

#[test]
fn smooth_runs_the_crossfader_across_and_swaps_the_bass_at_the_middle() {
    let t = timeline("SMOOTH", 16); // 64 beats
    let s = t.at(0.0);
    assert_eq!((s.crossfader, s.get(Side::In, Ctl::Low)), (0.0, Some(0.0)));
    assert_eq!(
        s.get(Side::Out, Ctl::Low),
        Some(0.5),
        "the outgoing bass plays until the middle"
    );
    assert!((t.at(16.0).crossfader - 0.25).abs() < 1e-6);
    let before = t.at(31.9);
    assert_eq!(
        (
            before.get(Side::In, Ctl::Low),
            before.get(Side::Out, Ctl::Low)
        ),
        (Some(0.0), Some(0.5))
    );
    let mid = t.at(32.0);
    assert_eq!(
        (mid.get(Side::In, Ctl::Low), mid.get(Side::Out, Ctl::Low)),
        (Some(0.5), Some(0.0))
    );
    assert_eq!(t.at(64.0).crossfader, 1.0);
    assert_eq!(
        (s.get(Side::In, Ctl::Fader), s.get(Side::In, Ctl::Filter)),
        (None, None),
        "the DJ's faders and filters stay put"
    );
    assert!(t.events().is_empty());
}

#[test]
fn bass_swap_holds_both_decks_up_while_the_bass_changes_hands() {
    let t = timeline("BASS SWAP", 16);
    assert!((t.at(4.0).crossfader - 0.25).abs() < 1e-6); // 12.5% = 8 beats up to the middle
    assert_eq!(t.at(8.0).crossfader, 0.5);
    assert_eq!(t.at(40.0).crossfader, 0.5, "both decks stay up in between");
    assert_eq!(t.at(31.0).get(Side::In, Ctl::Low), Some(0.0));
    let s = t.at(33.0);
    assert_eq!(
        (s.get(Side::In, Ctl::Low), s.get(Side::Out, Ctl::Low)),
        (Some(0.5), Some(0.0))
    );
    assert_eq!(t.at(56.0).crossfader, 0.5);
    assert!((t.at(60.0).crossfader - 0.75).abs() < 1e-6);
    assert_eq!(t.at(64.0).crossfader, 1.0);
}

#[test]
fn filter_sweeps_the_outgoing_deck_into_a_high_pass_while_the_fader_follows() {
    let t = timeline("FILTER", 16);
    assert_eq!(t.at(0.0).get(Side::Out, Ctl::Filter), Some(0.0));
    assert!(near(t.at(24.0).get(Side::Out, Ctl::Filter), 0.5)); // 75% = 48 beats
    assert!(near(t.at(48.0).get(Side::Out, Ctl::Filter), 1.0));
    assert!(near(t.at(64.0).get(Side::Out, Ctl::Filter), 1.0));
    assert_eq!(t.at(16.0).crossfader, 0.0, "the crossfader waits a quarter");
    assert!((t.at(40.0).crossfader - 0.5).abs() < 1e-6);
    assert_eq!(t.at(31.0).get(Side::In, Ctl::Low), Some(0.0));
    assert_eq!(t.at(33.0).get(Side::In, Ctl::Low), Some(0.5));
}

#[test]
fn filter_in_opens_a_low_pass_on_the_incoming_deck() {
    let t = timeline("FILTER IN", 16);
    assert!(near(t.at(0.0).get(Side::In, Ctl::Filter), -1.0));
    assert!(near(t.at(22.4).get(Side::In, Ctl::Filter), -0.5)); // opens over 70% = 44.8 beats
    assert!(near(t.at(44.8).get(Side::In, Ctl::Filter), 0.0));
    assert!((t.at(32.0).crossfader - 0.5).abs() < 1e-6);
    assert_eq!(t.at(33.0).get(Side::Out, Ctl::Low), Some(0.0));
}

#[test]
fn echo_out_throws_the_outgoing_deck_into_the_echo_then_cuts_it_and_lets_the_tail_ring() {
    let t = timeline("ECHO OUT", 16); // fixed at 4 bars = 16 beats
    assert_eq!(t.len(), 16.0);
    let ev = t.events();
    assert_eq!(ev.len(), 2);
    assert_eq!((ev[0].0, ev[0].1), (0.0, Side::Out));
    assert_eq!(
        ev[0].2,
        Event::Fx(FxStep {
            effect: Some("echo".into()),
            on: true,
            level: Some(0.6),
            beat: Some(0.5)
        })
    );
    assert_eq!((ev[1].0, ev[1].1), (12.0, Side::Out));
    assert_eq!(
        ev[1].2,
        Event::Fx(FxStep {
            effect: None,
            on: false,
            level: None,
            beat: None
        })
    );
    assert_eq!(t.at(0.4).crossfader, 0.0);
    assert_eq!(
        t.at(0.5).crossfader,
        1.0,
        "the dry outgoing deck is cut after half a beat"
    );
    assert_eq!(
        (
            t.at(3.9).get(Side::In, Ctl::Low),
            t.at(4.0).get(Side::In, Ctl::Low)
        ),
        (Some(0.0), Some(0.5))
    );
}

#[test]
fn reverb_out_cuts_the_outgoing_deck_after_a_beat_of_reverb() {
    let t = timeline("REVERB OUT", 16);
    assert_eq!(t.len(), 16.0);
    assert!(
        matches!(&t.events()[0].2, Event::Fx(f) if f.effect.as_deref() == Some("reverb") && f.on)
    );
    assert_eq!((t.at(0.9).crossfader, t.at(1.0).crossfader), (0.0, 1.0));
    assert!(
        matches!(t.events()[1], (b, Side::Out, Event::Fx(FxStep { on: false, .. })) if b == 12.0)
    );
}

#[test]
fn cut_is_an_instant_switch_on_the_downbeat() {
    let t = timeline("CUT", 16);
    assert_eq!(t.len(), 4.0, "one bar");
    assert_eq!((t.at(0.0).crossfader, t.at(4.0).crossfader), (1.0, 1.0));
    assert!(t.events().is_empty());
    let s = t.at(0.0);
    assert!(
        s.deck.iter().flatten().all(Option::is_none),
        "no EQ, fader or filter moves"
    );
}

#[test]
fn brake_stops_the_outgoing_deck_over_three_beats_as_the_incoming_one_fades_in() {
    let t = timeline("BRAKE", 16);
    assert_eq!(t.len(), 8.0);
    assert_eq!(t.events(), &[(0.0, Side::Out, Event::Brake(3.0))]);
    assert_eq!(t.at(1.0).crossfader, 0.0);
    assert!((t.at(2.5).crossfader - 0.5).abs() < 1e-6);
    assert_eq!(t.at(4.0).crossfader, 1.0);
    assert_eq!(
        (
            t.at(3.9).get(Side::In, Ctl::Low),
            t.at(4.0).get(Side::In, Ctl::Low)
        ),
        (Some(0.0), Some(0.5))
    );
}

#[test]
fn loop_out_loops_a_bar_of_the_outgoing_deck_and_dulls_it_while_it_fades() {
    let t = timeline("LOOP OUT", 16);
    assert_eq!(t.events(), &[(0.0, Side::Out, Event::Loop(4.0))]);
    assert!(near(t.at(32.0).get(Side::Out, Ctl::Filter), -0.35));
    assert!(near(t.at(64.0).get(Side::Out, Ctl::Filter), -0.7));
    assert!((t.at(32.0).crossfader - 0.5).abs() < 1e-6);
    assert_eq!(
        (
            t.at(31.0).get(Side::Out, Ctl::Low),
            t.at(33.0).get(Side::Out, Ctl::Low)
        ),
        (Some(0.5), Some(0.0))
    );
}

#[test]
fn eq_blend_brings_the_incoming_mids_and_highs_in_first_then_swaps_the_bass() {
    let t = timeline("EQ BLEND", 16);
    assert_eq!(t.at(0.0).get(Side::In, Ctl::Mid), Some(0.0));
    assert_eq!(t.at(0.0).get(Side::In, Ctl::Low), Some(0.0));
    assert!((t.at(1.6).crossfader - 0.25).abs() < 1e-6);
    assert!((t.at(3.2).crossfader - 0.5).abs() < 1e-6);
    assert!(near(t.at(9.6).get(Side::In, Ctl::Mid), 0.25)); // 30% = 19.2 beats
    assert!(near(t.at(19.2).get(Side::In, Ctl::High), 0.5));
    assert!(
        near(t.at(19.2).get(Side::Out, Ctl::Mid), 0.5),
        "the outgoing mids stay until the incoming ones are in"
    );
    assert!(near(t.at(28.8).get(Side::Out, Ctl::High), 0.25));
    assert!(near(t.at(38.4).get(Side::Out, Ctl::Mid), 0.0));
    assert_eq!(
        t.at(44.7).get(Side::In, Ctl::Low),
        Some(0.0),
        "bass still on the outgoing deck"
    );
    let s = t.at(44.8);
    assert_eq!(
        (s.get(Side::In, Ctl::Low), s.get(Side::Out, Ctl::Low)),
        (Some(0.5), Some(0.0))
    );
    assert!((t.at(60.8).crossfader - 0.75).abs() < 1e-6);
}

fn recipe(json: &str) -> Recipe {
    serde_json::from_str(json).unwrap()
}

#[test]
fn positions_are_beats_percents_or_from_the_end_and_a_ramp_starts_from_the_previous_target() {
    let r = recipe(
        r#"{"name": "T", "steps": [
            {"at": 4, "deck": "out", "fader": 0.5},
            {"at": -4, "over": 2, "deck": "out", "fader": 0},
            {"at": "50%", "over": "25%", "deck": "in", "high": 0},
            {"at": "50%", "deck": "in", "high": 1}]}"#,
    );
    r.validate().unwrap();
    let t = r.timeline(16.0);
    let f = |b| t.at(b).get(Side::Out, Ctl::Fader);
    assert_eq!((f(3.9), f(4.0), f(11.9)), (Some(1.0), Some(0.5), Some(0.5)));
    assert!(near(f(13.0), 0.25), "from 0.5 to 0 over beats 12 to 14");
    assert_eq!(f(14.0), Some(0.0));
    assert_eq!(
        t.at(9.0).get(Side::In, Ctl::High),
        Some(1.0),
        "the later step of the same beat wins"
    );
}

#[test]
fn a_bad_recipe_is_refused_with_the_step_named() {
    let bad = |steps: &str| {
        recipe(&format!(r#"{{"name": "T", "steps": [{steps}]}}"#))
            .validate()
            .unwrap_err()
    };
    assert!(
        bad(r#"{"at": 0, "low": 0}"#).contains("step 1")
            && bad(r#"{"at": 0, "low": 0}"#).contains("deck")
    );
    assert!(bad(r#"{"at": 0}"#).contains("moves nothing"));
    assert!(bad(r#"{"at": "soon", "crossfader": 1}"#).contains("`at`"));
    assert!(bad(r#"{"at": 0, "over": -1, "crossfader": 1}"#).contains("`over`"));
    assert!(
        bad(r#"{"at": 0, "crossfader": 1}, {"at": 0, "deck": "in", "filter": 2}"#)
            .contains("step 2")
    );
    assert!(bad(r#"{"at": 0, "deck": "out", "brake": 0}"#).contains("brake"));
    assert!(
        recipe(r#"{"name": "", "steps": [{"at": 0, "crossfader": 1}]}"#)
            .validate()
            .is_err()
    );
    assert!(
        recipe(r#"{"name": "A,B", "steps": [{"at": 0, "crossfader": 1}]}"#)
            .validate()
            .is_err()
    );
    assert!(recipe(r#"{"name": "T", "steps": []}"#).validate().is_err());
    assert!(
        serde_json::from_str::<Recipe>(r#"{"name": "T", "steps": [{"at": 0, "crossfade": 1}]}"#)
            .is_err(),
        "a typo is an error"
    );
}

#[test]
fn plugin_styles_may_not_take_a_built_in_name_or_each_others() {
    let r = |n: &str| {
        recipe(&format!(
            r#"{{"name": "{n}", "steps": [{{"at": 0, "crossfader": 1}}]}}"#
        ))
    };
    assert!(validate_all(&[r("SLAM"), r("DROP")]).is_ok());
    assert!(validate_all(&[r("smooth")]).unwrap_err().contains("taken"));
    assert!(validate_all(&[r("SLAM"), r("Slam")]).is_err());
}

#[test]
fn random_never_repeats_and_stays_inside_the_enabled_set() {
    let enabled = [2, 5, 7];
    let mut last = None;
    let mut seen = [false; 3];
    let mut r = 12345u64;
    for _ in 0..300 {
        r = r
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let p = pick(&enabled, last, r >> 33).unwrap();
        assert!(
            enabled.contains(&p) && Some(p) != last,
            "{p} after {last:?}"
        );
        seen[enabled.iter().position(|&e| e == p).unwrap()] = true;
        last = Some(p);
    }
    assert_eq!(seen, [true; 3], "every enabled style comes up");
    assert_eq!(
        pick(&[4], Some(4), 99),
        Some(4),
        "one style left: it has to repeat"
    );
    assert_eq!(pick(&[], None, 1), None);
    assert_ne!(random_u64(), random_u64());
}

#[test]
fn a_fixed_length_style_spans_its_bars_of_the_grids_meter() {
    let r = style("CUT");
    let per = 3.0; // a 3/4 grid: the caller passes bars × the grid's beats per bar
    let t = r.timeline(r.bars.unwrap() as f64 * per);
    assert_eq!(t.len(), 3.0, "one bar of three beats");
    assert_eq!(t.at(t.len()).crossfader, 1.0);
}
