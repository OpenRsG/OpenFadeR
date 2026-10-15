//! Set-up: the shared [`Midi`] state, the first connection, the PREFS listener and the 5 ms input timer.

use super::jog::Jog;
use super::led::Leds;
use super::Ev;
use super::{decode, hotplug, layer, learn, lights, prefs_audio, usermap, Guard, Midi};
use crate::app::deck::{Decks, DECKS};
use crate::app::{App, MidiState};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

impl Midi {
    /// The next decodable message from the input ports, without collecting the channel into a list first.
    fn next_event(&self) -> Option<Ev> {
        loop {
            let raw = self.ports.borrow().as_ref()?.rx.try_recv().ok()?;
            if let Some(ev) = decode(raw) {
                return Some(ev);
            }
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

    // PREFS: the jog speed, and "Use controller audio when connected" switched while a controller is connected.
    let (weak_midi, weak_app, was) = (
        Rc::downgrade(&midi),
        app.as_weak(),
        Cell::new(prefs_audio()),
    );
    crate::app::prefs::on_change(move |p| {
        let Some(m) = weak_midi.upgrade() else { return };
        m.jog_factor.set(p.controller.jog_factor());
        if p.controller_audio != was.replace(p.controller_audio) {
            if let Some(app) = weak_app.upgrade() {
                m.audio_pref_changed(&app);
            }
        }
    });

    // 5 ms while MIDI input ports are open (jog and controls feel immediate); 100 ms without, which still sees a controller
    // arrive (the hot-plug thread reads the port list once a second) without waking the app 200 times a second for nothing.
    let (m, input) = (midi.clone(), Rc::new(Timer::default()));
    let weak_input = Rc::downgrade(&input);
    input.start(TimerMode::Repeated, Duration::from_millis(5), move || {
        m.start_dump();
        while let Some(ev) = m.next_event() {
            m.handle(ev);
        }
        m.tick();
        m.dump_tick();
        m.poll_ports();
        m.flush_toasts();
        let open = m
            .ports
            .borrow()
            .as_ref()
            .is_some_and(|p| !p.names.is_empty());
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
