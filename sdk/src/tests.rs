use super::*;

struct Boom {
    calls: u32,
}

impl Effect for Boom {
    const NAME: &'static CStr = c"BOOM";
    fn new(_: u32) -> Boom {
        Boom { calls: 0 }
    }
    /// Doubles the signal, panics on its third call.
    fn tick(&mut self, x: Frame, _: &Ctx) -> Frame {
        self.calls += 1;
        assert!(self.calls < 3, "third frame");
        [x[0] * 2.0, x[1] * 2.0]
    }
    fn reset(&mut self) {
        self.calls = 0;
    }
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
fn every_slot_round_trips_through_its_raw_value_and_unknown_ones_are_refused() {
    for s in Slot::ALL {
        assert_eq!(Slot::from_raw(s as u32), Some(s));
    }
    assert_eq!(Slot::from_raw(Slot::ALL.len() as u32), None);
}

#[test]
fn a_panicking_effect_passes_its_input_through_until_reset() {
    let vt = vtable::<Boom>();
    let (create, tick) = (vt.create.unwrap(), vt.tick.unwrap());
    let (reset, destroy) = (vt.reset.unwrap(), vt.destroy.unwrap());
    let run = |inst| {
        let mut f = [0.5, -0.25];
        unsafe { tick(inst, &mut f, &CTX) };
        f
    };
    let inst = unsafe { create(48_000) };
    assert_eq!(run(inst), [1.0, -0.5]);
    assert_eq!(run(inst), [1.0, -0.5]);
    assert_eq!(run(inst), [0.5, -0.25], "the panic leaves the input alone");
    assert_eq!(run(inst), [0.5, -0.25], "and the effect stays bypassed");
    unsafe { reset(inst) };
    assert_eq!(run(inst), [1.0, -0.5], "reset revives it");
    unsafe { destroy(inst) };
}

#[test]
fn mix_is_exactly_the_input_while_off_and_the_effect_at_full_level() {
    let (x, w) = ([0.3, -0.3], [1.0, 1.0]);
    assert_eq!(Ctx { g: 0.0, ..CTX }.mix(x, w), x);
    let full = CTX.mix(x, w);
    assert!(
        (full[0] - w[0]).abs() < 1e-6 && (full[1] - w[1]).abs() < 1e-6,
        "{full:?}"
    );
    let half = Ctx { level: 0.5, ..CTX }.mix(x, w);
    assert!(
        (half[0] - 0.65).abs() < 1e-6 && (half[1] - 0.35).abs() < 1e-6,
        "{half:?}"
    );
}
