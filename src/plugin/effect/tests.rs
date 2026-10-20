use super::native::{from_vtable, read};
use super::*;
use crate::audio::fx::{Effect as Selected, Settings, Unit};
use crate::plugin::Manifest;
use openfader_plugin as sdk;
use openfader_plugin::{EffectVTable, PluginInfo};
use std::cell::Cell;
use std::ffi::{CStr, c_void};
use std::fs;
use std::path::{Path, PathBuf};

thread_local! {
    /// Live `Gain` instances created on this thread (tests run on their own threads).
    static LIVE: Cell<i32> = const { Cell::new(0) };
}

/// Doubles the signal.
struct Gain;

impl sdk::Effect for Gain {
    const NAME: &'static CStr = c"GAIN";
    fn new(_: u32) -> Gain {
        LIVE.with(|l| l.set(l.get() + 1));
        Gain
    }
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
        c.mix(x, [x[0] * 2.0, x[1] * 2.0])
    }
    fn reset(&mut self) {}
}

impl Drop for Gain {
    fn drop(&mut self) {
        LIVE.with(|l| l.set(l.get() - 1));
    }
}

struct Boom;

impl sdk::Effect for Boom {
    const NAME: &'static CStr = c"BOOM";
    fn new(_: u32) -> Boom {
        Boom
    }
    fn tick(&mut self, _: Frame, _: &Ctx) -> Frame {
        panic!("effect bug");
    }
    fn reset(&mut self) {}
}

/// The entry point a library built with the SDK exports, here in this binary.
mod fake {
    openfader_plugin::export_effects!(super::Gain, super::Boom);
}

pub(crate) fn fake_effects() -> Vec<Effect> {
    unsafe { read(fake::openfader_plugin_v1(), "fake") }.unwrap()
}

const CTX: Ctx = Ctx {
    rate: 48_000.0,
    period: 24_000.0,
    beats: 1.0,
    level: 1.0,
    g: 1.0,
    knob: 0.5,
};

#[test]
fn a_sdk_table_runs_through_the_host_stage_and_is_destroyed_on_drop() {
    let fx = fake_effects();
    assert_eq!(
        fx.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["GAIN", "BOOM"]
    );
    assert!(fx.iter().all(|e| e.plugin == "fake"));
    let base = LIVE.with(Cell::get);
    let mut s = fx[0].stage(48_000);
    assert_eq!(LIVE.with(Cell::get), base + 1);
    assert_eq!(s.tick([0.25, -0.5], &CTX), [0.5, -1.0]);
    assert_eq!(
        s.tick([0.25, -0.5], &Ctx { g: 0.0, ..CTX }),
        [0.25, -0.5],
        "the context reaches the effect"
    );
    s.reset();
    s.engage();
    drop(s);
    assert_eq!(LIVE.with(Cell::get), base);
}

#[test]
fn a_unit_constructs_only_selected_plugins_and_recreates_them_after_switching() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    static CREATED: AtomicUsize = AtomicUsize::new(0);
    static DESTROYED: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl sdk::Effect for Counted {
        const NAME: &'static CStr = c"COUNTED";
        fn new(_: u32) -> Self {
            CREATED.fetch_add(1, SeqCst);
            Self
        }
        fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {
            c.mix(x, [2.0 * x[0], 2.0 * x[1]])
        }
        fn reset(&mut self) {}
    }
    impl Drop for Counted {
        fn drop(&mut self) {
            DESTROYED.fetch_add(1, SeqCst);
        }
    }
    let plugin = unsafe { from_vtable("selected-only", &sdk::vtable::<Counted>()) }.unwrap();
    let created = CREATED.load(SeqCst);
    let destroyed = DESTROYED.load(SeqCst);
    let mut u = Unit::with(48_000, &[plugin]);
    assert_eq!(
        CREATED.load(SeqCst),
        created,
        "unselected plugins are not instantiated"
    );
    let s = Settings {
        effect: Selected::Plugin(0).index(),
        beats: 1.0,
        level: 1.0,
        knob: 0.5,
        on: true,
        bpm: 120.0,
    };
    for selected in 1..=2 {
        u.prepare(s.effect);
        u.set(s);
        let y = (0..500).map(|_| u.process([0.25, -0.5])).last().unwrap();
        assert_eq!(y, [0.5, -1.0]);
        assert_eq!(CREATED.load(SeqCst), created + selected);
        u.prepare(Selected::Trans.index());
        u.set(Settings {
            effect: Selected::Trans.index(),
            ..s
        });
    }
    drop(u);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while DESTROYED.load(SeqCst) < destroyed + 2 {
        assert!(std::time::Instant::now() < deadline, "retirement timed out");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(DESTROYED.load(SeqCst), destroyed + 2);
}

#[test]
fn a_panicking_effect_passes_the_signal_through_in_a_unit() {
    let fx = fake_effects();
    let mut u = Unit::with(48_000, &fx);
    let set = |effect: Selected| Settings {
        effect: effect.index(),
        beats: 1.0,
        level: 1.0,
        knob: 0.5,
        on: true,
        bpm: 120.0,
    };
    u.prepare(Selected::Plugin(1).index());
    u.set(set(Selected::Plugin(1)));
    for _ in 0..500 {
        u.process([0.0; 2]); // engage ramp done
    }
    assert_eq!(u.process([0.5, -0.5]), [0.5, -0.5]);
    u.prepare(Selected::Plugin(0).index());
    u.set(set(Selected::Plugin(0)));
    let y = (0..500).map(|_| u.process([0.5, -0.5])).last().unwrap();
    assert_eq!(y, [1.0, -1.0], "the next effect still works");
}

#[test]
fn a_unit_without_the_plugin_falls_back_instead_of_indexing_out_of_range() {
    let on = |effect| Settings {
        effect,
        beats: 1.0,
        level: 1.0,
        knob: 0.5,
        on: true,
        bpm: 120.0,
    };
    let mut u = Unit::with(48_000, &[]);
    u.prepare(Selected::Plugin(3).index());
    u.set(on(Selected::Plugin(3).index()));
    assert!(u.process([0.1; 2])[0].is_finite());
    let mut rack = Unit::rack_with(48_000, &[]);
    rack.set(on(2));
    assert_eq!(
        rack.process([0.1, 0.2]),
        [0.1, 0.2],
        "an empty rack passes through"
    );
    assert!(rack.idle());
}

#[test]
fn a_rack_runs_only_its_slot_effects_selected_by_position() {
    let fx = fake_effects();
    let mut rack = Unit::rack_with(48_000, &fx[..1]); // GAIN alone
    rack.set(Settings {
        effect: 0,
        beats: 1.0,
        level: 1.0,
        knob: 0.5,
        on: true,
        bpm: 120.0,
    });
    let y = (0..500)
        .map(|_| rack.process([0.25, -0.25]))
        .last()
        .unwrap();
    assert_eq!(
        y,
        [0.5, -0.5],
        "position 0 is the plugin, no built-in before it"
    );
}

/// A table copied from `Gain`'s with `edit` applied.
fn table(edit: impl FnOnce(&mut EffectVTable)) -> Result<Effect, String> {
    let mut vt = sdk::vtable::<Gain>();
    edit(&mut vt);
    unsafe { from_vtable("t", &vt) }
}

#[test]
fn tables_are_rejected_for_bad_names_missing_functions_and_a_failing_create() {
    unsafe extern "C" fn no_instance(_: u32) -> *mut c_void {
        std::ptr::null_mut()
    }
    let named = |n: &'static CStr| move |vt: &mut EffectVTable| vt.name = n.as_ptr();
    for (name, want) in [
        (c"", "must be 1 to 16"),
        (c"MORE THAN SIXTEEN CHARS", "must be 1 to 16"),
        (c"A.B", "must be 1 to 16"),
        (c"M\xc3\x9cLL", "must be 1 to 16"),
    ] {
        let e = table(named(name)).err().expect("rejected");
        assert!(e.contains(want), "{name:?}: {e}");
    }
    assert!(table(named(c"OK name_1-2")).is_ok());
    assert!(
        table(|vt| vt.name = std::ptr::null())
            .unwrap_err()
            .contains("no name")
    );
    for edit in [
        (|vt: &mut EffectVTable| vt.create = None) as fn(&mut EffectVTable),
        |vt| vt.tick = None,
        |vt| vt.destroy = None,
    ] {
        assert!(table(edit).unwrap_err().contains("required"));
    }
    assert!(
        table(|vt| vt.create = Some(no_instance))
            .unwrap_err()
            .contains("create failed")
    );
    let e = table(|vt| vt.slot = Slot::ALL.len() as u32).unwrap_err();
    assert!(e.contains("unknown slot"), "{e}");
    assert_eq!(table(|vt| vt.slot = 4).unwrap().slot, Slot::Lever);
    let optional = table(|vt| (vt.reset, vt.engage) = (None, None)).unwrap();
    let mut s = optional.stage(48_000);
    (s.reset(), s.engage()); // missing reset / engage are no-ops
    assert_eq!(s.tick([0.5; 2], &CTX), [1.0; 2]);
}

#[test]
fn an_abi_mismatch_or_empty_table_is_an_error() {
    let vt = sdk::vtable::<Gain>();
    let info = |abi, count, effects| PluginInfo {
        abi,
        count,
        effects,
    };
    let e = unsafe { read(&info(sdk::ABI + 1, 1, &vt), "p") }
        .err()
        .unwrap();
    assert!(e.contains(&format!("ABI {}", sdk::ABI + 1)), "{e}");
    let e = unsafe { read(&info(sdk::ABI, 0, &vt), "p") }.err().unwrap();
    assert!(e.contains("no effects"), "{e}");
    let e = unsafe { read(&info(sdk::ABI, 1, std::ptr::null()), "p") }
        .err()
        .unwrap();
    assert!(e.contains("no effects"), "{e}");
    let e = unsafe { read(std::ptr::null(), "p") }.err().unwrap();
    assert!(e.contains("null"), "{e}");
}

fn with_names(slot: Slot, names: &[&str]) -> Vec<Effect> {
    let g = fake_effects().remove(0);
    names
        .iter()
        .map(|n| Effect {
            name: n.to_string(),
            plugin: String::new(),
            slot,
            imp: g.imp.clone(),
        })
        .collect()
}

#[test]
fn names_are_unique_within_a_slot_across_builtins_and_plugins_ignoring_case() {
    let one = |slot, n: &str| with_names(slot, &[n]).remove(0);
    let loaded = with_names(Slot::Beat, &["BITCRUSH"]);
    assert!(admit(&loaded, &one(Slot::Beat, "RINGMOD")).is_ok());
    for (slot, taken) in [
        (Slot::Beat, "bitcrush"),
        (Slot::Beat, "Echo"),
        (Slot::Beat, "TRANS"),
        (Slot::Color, "dub echo"),
        (Slot::Fader, "Linear"),
    ] {
        let e = admit(&loaded, &one(slot, taken)).unwrap_err();
        assert!(e.contains("already taken"), "{taken}: {e}");
    }
    for (slot, free) in [
        (Slot::Send, "ECHO"),
        (Slot::Mic, "BITCRUSH"),
        (Slot::Color, "TRANS"),
    ] {
        assert!(
            admit(&loaded, &one(slot, free)).is_ok(),
            "{free} in {slot:?}"
        );
    }
    let many = with_names(Slot::Mic, &vec!["X"; MAX]);
    assert!(
        admit(&many, &one(Slot::Mic, "Y"))
            .unwrap_err()
            .contains("at most")
    );
    assert!(
        admit(&many, &one(Slot::Send, "Y")).is_ok(),
        "another slot has room"
    );
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rk-effect-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// `cc -shared` of `src` into `dir/out`; `None` when there is no C compiler.
fn cc(dir: &Path, src: &Path, out: &str) -> Option<()> {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk");
    let s = std::process::Command::new("cc")
        .args(["-O2", "-shared", "-fPIC", "-I"])
        .arg(&sdk)
        .arg(src)
        .arg("-o")
        .arg(dir.join(out))
        .arg("-lm")
        .status()
        .ok()?;
    assert!(s.success(), "cc failed on {}", src.display());
    Some(())
}

fn plugin(dir: &Path, name: &str, lib: &str) -> Plugin {
    let manifest = Manifest {
        name: name.into(),
        effects: Some(Decls::Library(lib.into())),
        ..Manifest::default()
    };
    Plugin {
        dir: dir.into(),
        manifest,
    }
}

#[test]
fn load_opens_real_libraries_and_reports_the_ones_that_do_not_work() {
    let dir = temp("load");
    let ringmod = Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk/examples/ringmod/ringmod.c");
    fs::write(dir.join("empty.c"), "int unrelated;").unwrap();
    let (Some(()), Some(())) = (
        cc(&dir, &ringmod, "libringmod.so"),
        cc(&dir, &dir.join("empty.c"), "libempty.so"),
    ) else {
        eprintln!("no C compiler: skipped");
        return;
    };
    fs::write(dir.join("notlib.so"), "not an object").unwrap();
    let plugins = [
        plugin(&dir, "ring", "libringmod.so"),
        plugin(&dir, "nosym", "libempty.so"),
        plugin(&dir, "junk", "notlib.so"),
        plugin(&dir, "gone", "missing.so"),
        plugin(&dir, "again", "libringmod.so"),
    ];
    let mut errors = Vec::new();
    let fx = load(&plugins, &mut errors);
    assert_eq!(
        fx.iter()
            .map(|e| (&*e.name, &*e.plugin))
            .collect::<Vec<_>>(),
        [("RINGMOD", "ring")]
    );
    assert_eq!(errors.len(), 4, "{errors:#?}");
    for (e, want) in errors.iter().zip([
        "nosym: ",
        "junk: cannot load",
        "gone: cannot load",
        "again: beat effect \"RINGMOD\"",
    ]) {
        assert!(e.starts_with(want), "{e}");
    }
    assert!(errors[0].contains("openfader_plugin_v1"), "{}", errors[0]);

    // The C effect, through the header's ABI: a ring modulator changes the signal and is exact while off.
    let mut s = fx[0].stage(48_000);
    let sine = |i: usize| (i as f32 * 0.05).sin();
    let moved = (1..200).any(|i| {
        let y = s.tick([sine(i); 2], &CTX);
        (y[0] - sine(i)).abs() > 0.05 && y[0] == y[1]
    });
    assert!(moved, "RINGMOD changes a sine");
    s.engage();
    assert_eq!(s.tick([0.3, -0.3], &Ctx { g: 0.0, ..CTX }), [0.3, -0.3]);
}
