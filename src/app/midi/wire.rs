//! Set-up: the shared [`Midi`] state, connections, event-driven input and the jog/housekeeping timer.

use super::jog::Jog;
use super::led::Leds;
use super::{Guard, Midi, decode, hotplug, layer, learn, lights, prefs_audio, usermap};
use crate::app::deck::{DECKS, Decks};
use crate::app::{App, MidiState};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

/// Bounds each UI turn, including unsupported messages, without discarding or reordering input.
const INPUT_BATCH: usize = 256;

pub(super) fn wake_input() -> Result<(), slint::EventLoopError> {
    slint::invoke_from_event_loop(super::get::input_ready)
}

impl Midi {
    pub(super) fn read_input(&self) {
        self.start_dump();
        {
            let ports = self.ports.borrow();
            let Some(p) = ports.as_ref() else { return };
            p.begin_input();
        }
        for _ in 0..INPUT_BATCH {
            let raw = self
                .ports
                .borrow()
                .as_ref()
                .and_then(|p| p.rx.try_recv().ok());
            let Some(raw) = raw else { return };
            if let Some(ev) = decode(raw) {
                self.handle(ev);
            }
        }
        if let Some(p) = self.ports.borrow().as_ref() {
            p.wake_input();
        }
    }
}

pub fn wire(app: &App, decks: Decks) -> Guard {
    let state = app.global::<MidiState>();
    learn::offer_actions(&state);

    let filter = std::env::var("OPENFADER_MIDI_PORT")
        .ok()
        .filter(|s| !s.is_empty());
    let ticks = std::env::var("OPENFADER_JOG_TICKS")
        .ok()
        .and_then(|v| v.parse().ok());
    let midi = Rc::new(Midi {
        app: app.as_weak(),
        decks,
        user: RefCell::new(usermap::load()),
        file_binds: RefCell::default(),
        file_names: RefCell::default(),
        captured: Cell::new(None),
        msb: RefCell::default(),
        jog: RefCell::new(std::array::from_fn(|_| Jog::new(ticks))),
        jog_factor: Cell::new(crate::app::prefs::with(|p| p.controller.jog_factor())),
        vinyl: Cell::new([true; DECKS]),
        cutter: Cell::new([false; DECKS]),
        layer: Cell::new(layer::Layer::default()),
        dual: Cell::default(),
        started: Instant::now(),
        log: std::env::var_os("OPENFADER_MIDI_LOG").is_some_and(|v| !v.is_empty()),
        layer34_noted: Cell::new(false),
        out: RefCell::new(None),
        leds: RefCell::new(Leds::new(Vec::new())),
        clock: RefCell::new(None),
        snap: RefCell::default(),
        snap_at: Cell::new(None),
        filter: filter.clone(),
        ports: RefCell::new(None),
        names: RefCell::default(),
        watcher: RefCell::new(None),
        link: RefCell::new(None),
        dump: Cell::new(None),
        dump_due: RefCell::default(),
        held: RefCell::default(),
        pending: RefCell::default(),
        lights: Timer::default(),
    });
    super::get::register(&midi);
    let m = midi.clone();
    state.on_assign(move |act, deck| m.assign(act as usize, deck));
    lights::quit_on_signals();

    // The first connection is the same as every later one; then the port list is watched.
    let names = hotplug::snapshot(filter.as_deref()).unwrap_or_else(|e| {
        state.set_ports(format!("MIDI unavailable: {e}").into());
        Vec::new()
    });
    midi.reconnect(app, names.clone());
    *midi.watcher.borrow_mut() = Some(hotplug::Watcher::start(filter, names));

    // MIDI clock output (PREFS > CONTROLLER > MIDI CLOCK OUT); its thread opens the port, now and after a replug.
    crate::app::prefs::with(|p| midi.clock_port(&p.controller.midi_clock_port));

    // PREFS: the jog speed, and "Use controller audio when connected" switched while a controller is connected.
    let (weak_midi, weak_app, was) = (
        Rc::downgrade(&midi),
        app.as_weak(),
        Cell::new(prefs_audio()),
    );
    crate::app::prefs::on_change(move |p| {
        let Some(m) = weak_midi.upgrade() else { return };
        m.jog_factor.set(p.controller.jog_factor());
        m.clock_port(&p.controller.midi_clock_port);
        if p.controller_audio != was.replace(p.controller_audio) {
            if let Some(app) = weak_app.upgrade() {
                m.audio_pref_changed(&app);
            }
        }
    });

    // Input wakes the event loop directly; this timer retains jog decay, session housekeeping and the MIDI clock's view of
    // the decks. Without input ports or clock, 100 ms still picks up the hot-plug worker's once-a-second port snapshots.
    let (m, input) = (midi.clone(), Rc::new(Timer::default()));
    let weak_input = Rc::downgrade(&input);
    input.start(TimerMode::Repeated, Duration::from_millis(5), move || {
        m.read_input(); // fallback if an event-loop wake could not be queued
        m.tick();
        m.clock_tick();
        m.dump_tick();
        m.poll_ports();
        m.flush_toasts();
        let open = m
            .ports
            .borrow()
            .as_ref()
            .is_some_and(|p| !p.names.is_empty())
            || m.clock.borrow().is_some();
        let every = Duration::from_millis(if open { 5 } else { 100 });
        if let Some(t) = weak_input.upgrade().filter(|t| t.interval() != every) {
            t.set_interval(every);
        }
    });
    Guard {
        midi,
        _input: input,
    }
}
