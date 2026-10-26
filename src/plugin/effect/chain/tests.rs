use super::*;
use crate::audio::fx::{Ctx, Stage};
use crate::plugin::effect::kit::{RATE, ctx, gain, impulse, off_response, peak_at, run, sine};
use crate::plugin::effect::{Decls, Slot, check, load};
use crate::plugin::{Manifest, Plugin};
use serde_json::json;
use std::path::PathBuf;

fn scope() -> Scope {
    Scope {
        lfos: vec!["lfo".into()],
    }
}

/// knob 0.25, level 0.5, g 1, 1 beat, 48 kHz, 120 BPM (period 0.5 s), `lfo` 0.75.
fn vars() -> Vars {
    let mut v = Vars::default();
    v.slot[KNOB] = 0.25;
    v.slot[LEVEL] = 0.5;
    v.slot[G] = 1.0;
    v.slot[BEATS] = 1.0;
    v.slot[super::RATE] = 48_000.0;
    v.slot[PERIOD] = 0.5;
    v.slot[PERIOD_MS] = 500.0;
    v.slot[BPM] = 120.0;
    v.slot[BUILTINS.len()] = 0.75;
    v
}

fn ev(src: &str) -> f32 {
    Expr::parse(src, &scope()).unwrap().eval(&vars())
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5 * b.abs().max(1.0)
}

#[test]
fn expressions_follow_the_usual_precedence() {
    assert_eq!(ev("1 + 2 * 3"), 7.0);
    assert_eq!(ev("(1 + 2) * 3"), 9.0);
    assert_eq!(ev("1 - 2 - 3"), -4.0);
    assert_eq!(ev("8 / 4 / 2"), 1.0);
    assert_eq!(ev("10 % 4"), 2.0);
    assert_eq!(ev("2 ^ 3 ^ 2"), 512.0, "power is right associative");
    assert_eq!(ev("-2 ^ 2"), -4.0, "and binds tighter than the minus");
    assert_eq!(ev("2 * -3"), -6.0);
}

#[test]
fn comparisons_logic_and_if_give_one_or_zero() {
    assert_eq!(ev("1 < 2"), 1.0);
    assert_eq!(ev("2 <= 1"), 0.0);
    assert_eq!(ev("2 >= 2"), 1.0);
    assert_eq!(ev("1 == 1 && 2 != 3"), 1.0);
    assert_eq!(ev("0 || 0"), 0.0);
    assert_eq!(ev("!0"), 1.0);
    assert_eq!(ev("!(1 < 2)"), 0.0);
    assert_eq!(ev("if(knob < 0.5, 10, 20)"), 10.0);
    assert_eq!(ev("if(knob > 0.5, 10, 20)"), 20.0);
}

#[test]
fn variables_come_from_the_context_and_the_effect_lfos() {
    assert_eq!(ev("knob + level + g + beats"), 2.75);
    assert_eq!(ev("period"), 0.5);
    assert_eq!(ev("period_ms"), 500.0);
    assert_eq!(ev("bpm"), 120.0);
    assert_eq!(ev("rate / 1000"), 48.0);
    assert_eq!(ev("lfo"), 0.75);
    assert!(near(ev("pi"), std::f32::consts::PI) && near(ev("tau"), std::f32::consts::TAU));
}

#[test]
fn numbers_take_exponents_and_the_units_ms_and_k() {
    assert!(near(ev("40ms"), 0.04));
    assert_eq!(ev("3k"), 3000.0);
    assert!(near(ev("1.5k + 2ms"), 1500.002));
    assert!(near(ev("1e-3"), 0.001));
    assert_eq!(ev("2e3"), 2000.0);
    assert_eq!(ev(".5"), 0.5);
}

#[test]
fn the_functions_work() {
    assert_eq!(ev("clamp(5, 0, 1)"), 1.0);
    assert_eq!(ev("clamp(-5, 0, 1)"), 0.0);
    assert_eq!(ev("min(3, 2) + max(3, 2)"), 5.0);
    assert_eq!(ev("abs(-3)"), 3.0);
    assert_eq!(ev("lerp(0, 10, 0.5)"), 5.0);
    assert!(near(ev("expo(100, 10000, 0.5)"), 1000.0));
    assert!(near(ev("semi(12)"), 2.0) && near(ev("semi(-12)"), 0.5));
    assert_eq!(ev("exp2(3)"), 8.0);
    assert_eq!(ev("log2(8)"), 3.0);
    assert_eq!(ev("floor(2.7) + round(2.5)"), 5.0);
    assert_eq!(ev("sqrt(16)"), 4.0);
    assert_eq!(ev("pow(2, 10)"), 1024.0);
    assert!(near(ev("tanh(0)"), 0.0) && near(ev("sin(0)"), 0.0) && near(ev("cos(0)"), 1.0));
}

#[test]
fn a_result_that_is_not_finite_is_zero() {
    assert_eq!(ev("1 / 0"), 0.0);
    assert_eq!(ev("sqrt(-1)"), 0.0);
    assert_eq!(ev("log2(0)"), 0.0);
}

#[test]
fn bad_expressions_say_what_is_wrong() {
    for (src, want) in [
        ("nope", "unknown variable \"nope\""),
        ("nope(1)", "unknown function \"nope\""),
        ("(1", "missing `)`"),
        ("1 2", "unexpected \"2\""),
        ("min(1)", "min takes 2 values"),
        ("clamp(1, 2)", "clamp takes 3 values"),
        ("1 +", "ends where a value is expected"),
        ("3x", "unknown unit \"x\""),
        ("", "ends where a value is expected"),
        ("1 $ 2", "unexpected \"$ 2\""),
        (
            &format!("{}1{}", "1 + (".repeat(20), ")".repeat(20)),
            "nested too deeply",
        ),
    ] {
        let e = Expr::parse(src, &scope()).unwrap_err();
        assert!(e.contains(want), "{src:?}: {e}");
        assert!(e.starts_with("expression "), "{e}");
    }
}

fn decl(chain: serde_json::Value) -> ChainDecl {
    serde_json::from_value(json!({"name": "T", "slot": "beat", "chain": chain})).unwrap()
}

fn stage(chain: serde_json::Value) -> Box<dyn Stage> {
    compile(&decl(chain)).unwrap().stage(RATE)
}

fn mistake(chain: serde_json::Value) -> String {
    compile(&decl(chain)).unwrap_err()
}

fn once(s: &mut dyn Stage, c: &Ctx, x: Frame) -> Frame {
    s.tick(x, c)
}

#[test]
fn gain_route_pan_and_width_shape_the_stereo_signal() {
    let c = ctx(0.5, 1.0, 0.25);
    assert_eq!(
        once(
            &mut *stage(json!([{"block": "gain", "gain": "knob"}])),
            &c,
            [1.0, -1.0]
        ),
        [0.25, -0.25]
    );
    let swap = json!([{"block": "route", "matrix": [[0, 1], [1, 0]]}]);
    assert_eq!(once(&mut *stage(swap), &c, [1.0, 2.0]), [2.0, 1.0]);
    let pan = |p| {
        once(
            &mut *stage(json!([{"block": "pan", "pan": p}])),
            &c,
            [1.0, 1.0],
        )
    };
    assert_eq!(pan(json!(1)), [0.0, 1.0]);
    assert_eq!(pan(json!(-0.5)), [1.0, 0.5]);
    assert_eq!(pan(json!(0)), [1.0, 1.0]);
    let width = |w| {
        once(
            &mut *stage(json!([{"block": "width", "width": w}])),
            &c,
            [1.0, 0.0],
        )
    };
    assert_eq!(width(0), [0.5, 0.5], "mono");
    assert_eq!(width(1), [1.0, 0.0]);
    assert_eq!(width(2), [1.5, -0.5]);
}

#[test]
fn mix_blends_towards_the_effect_or_adds_it_to_the_input() {
    let c = ctx(0.5, 1.0, 0.0);
    let mute = |mode: &str| {
        let chain = json!([{"block": "gain", "gain": 0}, {"block": "mix", "mode": mode}]);
        once(&mut *stage(chain), &c, [1.0, 1.0])
    };
    assert_eq!(mute("blend"), [0.5, 0.5]);
    let chain =
        json!([{"block": "gain", "gain": 3}, {"block": "mix", "mode": "add", "amount": "level"}]);
    assert_eq!(once(&mut *stage(chain), &c, [1.0, 1.0]), [2.5, 2.5]);
    let off = Ctx { g: 0.0, ..c };
    let chain = json!([{"block": "gain", "gain": 3}, {"block": "mix"}]);
    assert_eq!(
        once(&mut *stage(chain), &off, [1.0, 1.0]),
        [1.0, 1.0],
        "g 0: exactly the input"
    );
}

#[test]
fn saturate_and_bitcrush_change_the_signal() {
    let c = ctx(1.0, 1.0, 0.0);
    let soft = once(
        &mut *stage(json!([{"block": "saturate", "drive": 10}])),
        &c,
        [0.5, -0.5],
    );
    assert!(soft[0] > 0.99 && soft[1] < -0.99, "{soft:?}");
    let hard = json!([{"block": "saturate", "drive": 4, "shape": "clip"}]);
    assert_eq!(once(&mut *stage(hard), &c, [0.5, -0.1]), [1.0, -0.4]);
    // 2 bits: steps of 0.5.
    let crush = json!([{"block": "bitcrush", "bits": 2}]);
    assert_eq!(once(&mut *stage(crush), &c, [0.3, -0.3]), [0.5, -0.5]);
    // Held for 4 frames at a quarter of the rate: the first sample is taken on frame 3.
    let mut s = stage(json!([{"block": "bitcrush", "bits": 24, "hz": 12000}]));
    let y: Vec<f32> = (1..=8)
        .map(|i| s.tick([i as f32 * 0.1; 2], &c)[0])
        .collect();
    assert_eq!(&y[..3], &[0.0; 3]);
    assert!(
        y[3..7].iter().all(|v| near(*v, 0.4)) && near(y[7], 0.8),
        "{y:?}"
    );
}

#[test]
fn delay_reads_the_time_late_and_halves_a_time_that_does_not_fit() {
    let c = ctx(1.0, 1.0, 0.0);
    let imp = impulse(40_000, 0, [1.0, 1.0]);
    let y = run(
        &mut *stage(json!([{"block": "delay", "time": "10ms"}])),
        &c,
        &imp,
    );
    assert_eq!(peak_at(&y), 480);
    let y = run(
        &mut *stage(json!([{"block": "delay", "time": 10, "max": 1}])),
        &c,
        &imp,
    );
    assert_eq!(
        peak_at(&y),
        30_000,
        "10 s in a 1 s line: halved until it fits"
    );
}

#[test]
fn feedback_is_limited_so_the_repeats_always_die_away() {
    let y = off_response(
        &mut *stage(json!([{"block": "echo", "time": "50ms", "feedback": 5}])),
        1.0,
        0.5,
        [1.0, 1.0],
        120_000,
    );
    let (first, fifth) = (y[2400][0], y[2400 * 5][0]);
    assert!(near(first, 1.0), "{first}");
    assert!(
        near(fifth, 0.99f32.powi(4)),
        "limited to 0.99 a round trip: {fifth}"
    );
    assert!(y.iter().all(|f| f[0].abs() <= 1.0 + 1e-5));
}

#[test]
fn crossover_sums_back_to_the_input_and_parallel_adds_its_branches() {
    let c = ctx(1.0, 1.0, 0.0);
    let x = sine(440.0, 0.5, 2000);
    let y = run(
        &mut *stage(json!([{"block": "crossover", "freq": 150}])),
        &c,
        &x,
    );
    assert!(x.iter().zip(&y).all(|(a, b)| near(a[0], b[0])));
    let two = json!([{"block": "parallel", "branches": [[{"block": "gain", "gain": 2}], [{"block": "gain", "gain": 3}], []]}]);
    assert_eq!(once(&mut *stage(two), &c, [1.0, 2.0]), [6.0, 12.0]);
}

#[test]
fn lfos_drive_formulas_and_run_at_the_tempo_or_a_rate() {
    let c = ctx(1.0, 1.0, 0.0);
    // A 100 Hz square gate: 480 frames per cycle, open for the first half.
    let gate = json!({"name": "T", "slot": "beat",
        "lfo": [{"name": "gate", "hz": 100, "shape": "square"}],
        "chain": [{"block": "gain", "gain": "gate"}]});
    let d: ChainDecl = serde_json::from_value(gate).unwrap();
    let y = run(
        &mut *compile(&d).unwrap().stage(RATE),
        &c,
        &vec![[1.0, 1.0]; 1000],
    );
    assert_eq!((y[10][0], y[300][0], y[490][0]), (1.0, 0.0, 1.0));
    // A saw with the period of the beat (0.5 s here) is at one half after a quarter second.
    let saw = json!({"name": "T", "slot": "beat",
        "lfo": [{"name": "r", "period": "period", "shape": "saw"}],
        "chain": [{"block": "gain", "gain": "r"}]});
    let d: ChainDecl = serde_json::from_value(saw).unwrap();
    let y = run(
        &mut *compile(&d).unwrap().stage(RATE),
        &c,
        &vec![[1.0, 1.0]; 12_001],
    );
    assert!(near(y[12_000][0], 0.5), "{}", y[12_000][0]);
}

#[test]
fn engage_restarts_the_captures() {
    let c = ctx(1.0, 1.0, 0.0);
    let x: Vec<Frame> = (0..3000).map(|i| [i as f32 * 1e-4; 2]).collect();
    let mut s = stage(json!([{"block": "loop", "time": "0.02"}]));
    let y = run(&mut *s, &c, &x[..2000]);
    assert_eq!(y[0], x[0]);
    assert_eq!(
        y[1000],
        x[1000 - 960],
        "after the 960-frame capture it plays back"
    );
    s.engage();
    let y = run(&mut *s, &c, &x[..960]);
    assert_eq!(y, x[..960], "recording again, the input is heard live");
}

#[test]
fn allpass_keeps_the_level_and_a_two_stage_phaser_notches_its_centre() {
    let c = ctx(1.0, 1.0, 0.0);
    let ap = gain(
        &mut *stage(json!([{"block": "allpass", "time": "3ms"}])),
        &c,
        1000.0,
    );
    assert!((ap - 1.0).abs() < 0.02, "{ap}");
    let ph = |hz| {
        gain(
            &mut *stage(json!([{"block": "phaser", "stages": 2, "hz": 1000}])),
            &c,
            hz,
        )
    };
    assert!(ph(1000.0) < 0.05, "notch: {}", ph(1000.0));
    assert!(ph(50.0) > 0.95, "lows pass: {}", ph(50.0));
}

#[test]
fn pitch_passes_the_input_untouched_at_zero_semitones() {
    let c = ctx(1.0, 1.0, 0.0);
    let x = sine(300.0, 0.5, 3000);
    assert_eq!(run(&mut *stage(json!([{"block": "pitch"}])), &c, &x), x);
}

#[test]
fn a_chain_that_blows_up_is_reset_and_silent_for_that_frame() {
    let c = ctx(1.0, 1.0, 0.0);
    let mut s = stage(json!([{"block": "echo", "time": "10ms"}]));
    assert_eq!(s.tick([f32::NAN, 0.0], &c), [0.0, 0.0]);
    let y = s.tick([0.5, 0.5], &c);
    assert!(y[0].is_finite() && y[1].is_finite());
    for _ in 0..2000 {
        assert!(
            s.tick([0.1, 0.1], &c)[0].is_finite(),
            "the delay line was cleared"
        );
    }
}

#[test]
fn mistakes_name_the_block_the_parameter_and_where_it_is() {
    for (chain, wants) in [
        (
            json!([{"block": "gian"}]),
            vec!["chain[0]", "unknown block \"gian\"", "gain, route"],
        ),
        (
            json!([{"block": "gain", "gian": 1}]),
            vec!["chain[0] (gain)", "unknown parameter \"gian\""],
        ),
        (
            json!([{"block": "gain"}, {"block": "delay", "time": "1 +"}]),
            vec![
                "chain[1] (delay)",
                "\"time\"",
                "ends where a value is expected",
            ],
        ),
        (
            json!([{"block": "feedback", "chain": [{"block": "delay", "tiem": 1}]}]),
            vec!["chain[0].chain[0] (delay)", "\"tiem\""],
        ),
        (
            json!([{"block": "feedback", "chain": [], "loop": [{"block": "filter", "type": "xx"}]}]),
            vec!["chain[0].loop[0] (filter)", "one of lp, hp, bp, notch"],
        ),
        (
            json!([{"gain": 1}]),
            vec!["chain[0]", "needs a \"block\" name"],
        ),
        (json!([3]), vec!["chain[0]", "a block is an object"]),
        (
            json!([{"block": "delay", "max": 100}]),
            vec!["\"max\" must be a number from 0.01 to 30"],
        ),
        (
            json!([{"block": "filter", "type": "notch", "order": 1}]),
            vec!["lp and hp only"],
        ),
        (json!([{"block": "feedback"}]), vec!["needs a \"chain\""]),
        (json!([{"block": "parallel"}]), vec!["needs \"branches\""]),
        (
            json!([{"block": "echo", "tme": 1}]),
            vec!["chain[0] (echo)", "unknown parameter \"tme\""],
        ),
        (
            json!([{"block": "echo", "dry": 3}]),
            vec!["\"dry\" must be true or false"],
        ),
        (
            json!([{"block": "echo", "feedback": "nope"}]),
            vec!["chain[0].echo[1] (delay)", "unknown variable \"nope\""],
        ),
        (
            json!([{"block": "route", "matrix": [[1, 0]]}]),
            vec!["\"matrix\" must be"],
        ),
        (
            json!([{"block": "gain", "gain": true}]),
            vec!["number or a formula string"],
        ),
    ] {
        let e = mistake(chain.clone());
        for w in wants {
            assert!(e.contains(w), "{chain}: {e}");
        }
    }
}

#[test]
fn lfo_declarations_are_checked() {
    let with = |lfo: serde_json::Value| {
        let d: ChainDecl = serde_json::from_value(
            json!({"name": "T", "slot": "beat", "lfo": lfo, "chain": [{"block": "gain"}]}),
        )
        .unwrap();
        compile(&d).err()
    };
    assert!(
        with(json!([{"name": "a", "hz": 1}, {"name": "b", "period": 2, "shape": "tri"}])).is_none()
    );
    for (lfo, want) in [
        (json!([{"name": "1x", "hz": 1}]), "new plain word"),
        (json!([{"name": "knob", "hz": 1}]), "new plain word"),
        (
            json!([{"name": "a", "hz": 1}, {"name": "a", "hz": 1}]),
            "new plain word",
        ),
        (
            json!([{"name": "a", "hz": 1, "shape": "zig"}]),
            "shape must be one of",
        ),
        (json!([{"name": "a"}]), "either \"period\""),
        (
            json!([{"name": "a", "hz": 1, "period": 1}]),
            "either \"period\"",
        ),
        (
            json!([{"name": "a", "period": "b"}]),
            "unknown variable \"b\"",
        ),
        (json!([{"name": "a", "hz": true}]), "number or a formula"),
        (
            json!([{"name": "a", "hz": 1}, {"name": "b", "hz": 1}, {"name": "c", "hz": 1}, {"name": "d", "hz": 1}, {"name": "e", "hz": 1}]),
            "at most 4 lfos",
        ),
    ] {
        let e = with(lfo.clone()).unwrap_or_else(|| panic!("{lfo} accepted"));
        assert!(e.contains(want), "{lfo}: {e}");
    }
}

#[test]
fn effects_decls_read_a_library_path_or_a_list_and_check_names_and_slots() {
    let lib: Decls = serde_json::from_value(json!("libx.so")).unwrap();
    assert!(matches!(lib, Decls::Library(p) if p == PathBuf::from("libx.so")));
    let list: Decls =
        serde_json::from_value(json!([{"name": "A", "slot": "mic", "chain": [{"block": "gain"}]}]))
            .unwrap();
    assert!(check(&list).is_ok());
    let e = serde_json::from_value::<Decls>(
        json!([{"name": "A", "slot": "mic", "chain": [], "chian": []}]),
    )
    .unwrap_err();
    assert!(
        e.to_string().contains("effects[0]") && e.to_string().contains("chian"),
        "{e}"
    );
    assert!(serde_json::from_value::<Decls>(json!(3)).is_err());
    let bad =
        |v: serde_json::Value| check(&serde_json::from_value(json!([v])).unwrap()).unwrap_err();
    assert!(bad(json!({"name": "", "slot": "mic", "chain": [{}]})).contains("1 to 16"));
    assert!(
        bad(json!({"name": "WAY TOO LONG A NAME", "slot": "mic", "chain": [{}]}))
            .contains("1 to 16")
    );
    assert!(
        bad(json!({"name": "A", "slot": "deck", "chain": [{}]})).contains("unknown slot \"deck\"")
    );
    assert!(bad(json!({"name": "A", "slot": "mic", "chain": []})).contains("empty `chain`"));
}

fn plugin(name: &str, effects: serde_json::Value) -> Plugin {
    Plugin {
        dir: PathBuf::new(),
        manifest: Manifest {
            name: name.into(),
            effects: Some(serde_json::from_value(effects).unwrap()),
            ..Manifest::default()
        },
        builtin: None,
    }
}

#[test]
fn load_skips_only_the_effect_with_a_mistake_and_groups_by_slot() {
    let gain = json!([{"block": "gain"}]);
    let plugins = [
        plugin(
            "p",
            json!([
                {"name": "M1", "slot": "mic", "chain": gain},
                {"name": "B1", "slot": "beat", "chain": gain},
                {"name": "BAD", "slot": "send", "chain": [{"block": "gian"}]},
                {"name": "echo", "slot": "beat", "chain": gain},
                {"name": "SAME", "slot": "mic", "chain": gain},
            ]),
        ),
        plugin("q", json!([{"name": "same", "slot": "mic", "chain": gain}])),
    ];
    let mut errors = Vec::new();
    let fx = load(&plugins, &mut errors);
    let got: Vec<(&str, Slot)> = fx.iter().map(|e| (e.name.as_str(), e.slot)).collect();
    assert_eq!(
        got,
        [("B1", Slot::Beat), ("M1", Slot::Mic), ("SAME", Slot::Mic)],
        "grouped by slot, plugin order within it"
    );
    assert_eq!(errors.len(), 3, "{errors:#?}");
    assert!(
        errors[0].starts_with("p: send effect \"BAD\": chain[0]"),
        "{}",
        errors[0]
    );
    assert!(
        errors[1].contains("beat effect \"echo\": the name is already taken"),
        "{}",
        errors[1]
    );
    assert!(
        errors[2].starts_with("q: mic effect \"same\""),
        "{}",
        errors[2]
    );
    assert!(
        fx.iter()
            .all(|e| e.stage(RATE).tick([0.5; 2], &ctx(1.0, 1.0, 0.5)) == [0.5; 2])
    );
}

#[test]
fn a_plugin_folder_with_declared_effects_loads_through_discovery() {
    let d = std::env::temp_dir().join(format!("rk-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for (n, m) in [
        (
            "good",
            json!({"name": "good", "effects": [{"name": "X", "slot": "fader", "chain": [{"block": "gain"}]}]}),
        ),
        (
            "badname",
            json!({"name": "badname", "effects": [{"name": "A.B", "slot": "fader", "chain": [{"block": "gain"}]}]}),
        ),
        (
            "badslot",
            json!({"name": "badslot", "effects": [{"name": "A", "slot": "nowhere", "chain": [{"block": "gain"}]}]}),
        ),
        (
            "typo",
            json!({"name": "typo", "effects": [{"name": "A", "slot": "mic", "chian": []}]}),
        ),
    ] {
        std::fs::create_dir_all(d.join(n)).unwrap();
        std::fs::write(d.join(n).join("plugin.json"), m.to_string()).unwrap();
    }
    let mut r = crate::plugin::discover(&[d]);
    assert_eq!(r.plugins.len(), 1, "{:#?}", r.errors);
    assert_eq!(r.errors.len(), 3, "{:#?}", r.errors);
    let fx = load(&r.plugins, &mut r.errors);
    assert_eq!(fx.len(), 1);
    assert_eq!((fx[0].name.as_str(), fx[0].slot), ("X", Slot::Fader));
}
