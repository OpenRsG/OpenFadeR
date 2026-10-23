//! The controller session: what happens when the set of MIDI input ports changes (at launch it is just the first change).
//!
//! [`Midi::reconnect`] closes the old session (releases what is held, turns the lights off while the controller is still there),
//! opens the input ports and the LED output of the new one, loads our mapping file that matches ([`mapfile`]), starts the lights and sends the
//! state-dump request ([`dump`]) and plays the connect light show ([`led::Leds::intro`]); when a mapped controller appeared it also
//! moves the audio output to the controller's own interface (`audio::set_output`; PREFS "Use controller audio when connected", on by
//! default; `OPENFADER_OUTPUT_DEVICE` still wins; a device picked in PREFS > AUDIO afterwards wins until the next connect), and when
//! it went away it puts the previous output back. Each step is announced with a toast. One mapped controller per run
//! (the file with the longest port name found in a port name), the others work through `midi.map` only.

use super::{dump, hotplug, led, mapfile, port, prefs_audio, App, Ev, LedRow, Midi, Outcome};
use crate::app::{
    catalog,
    deck::{active, DECKS},
    toast, MidiState, MixerState,
};
use openfader::audio;
use slint::ComponentHandle;
use std::{
    rc::Rc,
    time::{Duration, Instant},
};

/// The connected controller that has a mapping file.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The mapping file's controller name.
    pub controller: String,
    /// What the audio switch did, for the status line ("audio: Test Controller", "audio unchanged (PREFS off)", "audio: <why not>").
    pub audio: String,
    /// What the state dump did.
    pub dump: String,
    /// The output device the app moved to for this controller: put back when it goes.
    switched: bool,
}

impl Link {
    fn text(&self) -> String {
        tr!("connected · {} · {}", self.audio, self.dump)
    }
}

/// What the mapping file for the connected ports gave.
#[derive(Default)]
struct Loaded {
    /// The CONTROLLER line.
    text: String,
    /// Problems for the status line.
    status: Vec<String>,
    leds: Vec<LedRow>,
    /// The jog ring's refresh period, when a mapped controller is connected.
    ring_ms: Option<u64>,
    /// The mapping file's state request (empty = none).
    request: Vec<u8>,
    controller: Option<String>,
}

impl Midi {
    /// Queues a toast: shown on the next 5 ms tick, so one raised before the event loop runs (launch) appears when the window does.
    pub(super) fn notify(&self, text: String) {
        self.pending.borrow_mut().push(text);
    }

    pub(super) fn flush_toasts(&self) {
        if self.pending.borrow().is_empty() {
            return;
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        for t in self.pending.take() {
            toast::show(&app, &t);
        }
    }

    /// The input port list changed (the hot-plug thread saw it): rebuild when it differs from what is connected.
    pub(super) fn poll_ports(self: &Rc<Self>) {
        let Some(names) = self.watcher.borrow().as_ref().and_then(|w| w.latest()) else {
            return;
        };
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let changes = hotplug::diff(&self.names.borrow(), &names);
        if self.log {
            changes.iter().for_each(|c| eprintln!("midi: {c}"));
        }
        if !changes.is_empty() {
            self.reconnect(&app, names);
        }
    }

    /// Closes the old session: buttons still held are released, the platter lets go, the lights go dark when the controller is still
    /// there to see it, the ports close.
    pub(super) fn close(&self, controller_present: bool) {
        (self.dump.set(None), *self.dump_due.borrow_mut() = None);
        let held: Vec<_> = self.held.borrow_mut().drain().collect();
        for (status, d1) in held {
            self.handle(Ev {
                status: status & 0x8F,
                d1,
                d2: 0,
            });
        }
        for d in 0..DECKS {
            self.touch(d, false);
        }
        if controller_present {
            self.leds_off();
        } else {
            self.out.borrow_mut().take(); // the port is gone: nothing to tell it, and no waiting for the queue
        }
        self.ports.borrow_mut().take();
    }

    /// Opens the session for the input ports `names` (what the hot-plug thread / the launch snapshot saw), replacing the old one.
    pub(super) fn reconnect(self: &Rc<Self>, app: &App, names: Vec<String>) {
        let state = app.global::<MidiState>();
        let before = self.link.borrow_mut().take();
        let still_there = before
            .as_ref()
            .is_some_and(|b| names.iter().any(|n| n.contains(&b.controller)));
        self.close(still_there);
        *self.names.borrow_mut() = names;
        self.file_binds.borrow_mut().clear();
        self.file_names.borrow_mut().clear();
        self.msb.borrow_mut().clear();
        self.layer.set(Default::default());
        self.layer34_noted.set(false);
        self.snap_at.set(None);

        let ports = port::connect(self.filter.as_deref(), super::wire::wake_input);
        state.set_ports(match &ports {
            Ok(p) if p.names.is_empty() => tr!("no MIDI input ports").into(),
            Ok(p) => p.names.join(", ").into(),
            Err(e) => tr!("MIDI unavailable: {}", e).into(),
        });
        let loaded = self.load_mapping(ports.as_ref().map_or(&[][..], |p| &p.names));
        *self.request.borrow_mut() = loaded.request.clone();
        *self.ports.borrow_mut() = ports.ok();
        *self.leds.borrow_mut() = led::Leds::new(loaded.leds);
        state.set_controller(loaded.text.into());
        state.set_status(loaded.status.join("; ").into());
        state.set_mapped(self.mapped() as i32);
        self.restart_lights(loaded.ring_ms.filter(|_| self.out.borrow().is_some()));

        // What the user is told: the old controller left, the new one arrived (the same one again = a port change, not news).
        let changed = before.as_ref().map(|b| &b.controller) != loaded.controller.as_ref();
        if let (Some(b), true) = (&before, changed) {
            self.notify(tr!("{} disconnected", b.controller));
            if b.switched {
                self.restore_audio(app);
            }
        }
        let Some(name) = loaded.controller else {
            state.set_connected("".into());
            state.set_link("".into());
            // Ports but no mapping: the repositories may have one for them ([`crate::app::catalog`]).
            catalog::unmapped(self.names.borrow().clone());
            return;
        };
        catalog::mapped();
        let mut link = Link {
            controller: name.clone(),
            audio: tr!("audio unchanged").into(),
            dump: tr!("no state dump").into(),
            switched: false,
        };
        if let (Some(b), false) = (before, changed) {
            (link.audio, link.switched) = (b.audio, b.switched);
        } else {
            self.notify(tr!("{} connected", name));
            self.leds.borrow_mut().intro();
            self.switch_audio(app, &mut link);
        }
        self.request_dump(&mut link, loaded.request);
        state.set_connected(link.controller.clone().into());
        state.set_link(link.text().into());
        *self.link.borrow_mut() = Some(link);
    }

    /// Loads the mapping file that matches the input ports `in_names`, opens the controller's LED output and installs the
    /// bindings (the lights and the status text are the caller's).
    fn load_mapping(&self, in_names: &[String]) -> Loaded {
        let mut status = Vec::new();
        match mapfile::load(in_names) {
            Ok(Some(m)) => {
                let mut text = tr!(
                    "{}: {} bindings, {} LEDs",
                    m.controller,
                    m.binds.len(),
                    m.leds.len()
                );
                // The output port carries the lights and the state-dump request.
                match port::connect_out(self.filter.as_deref().unwrap_or(&m.controller)) {
                    Ok((conn, name)) => {
                        text += &format!(" → {name}");
                        *self.out.borrow_mut() = Some(port::Sink::new(
                            conn,
                            Duration::from_millis(m.out_ms.unwrap_or(port::OUT_MS)),
                        ));
                    }
                    Err(e) => status.push(tr!("no LED output: {}", e)),
                }
                eprintln!("midi: {text}");
                status.extend(m.bad.iter().map(|b| format!("{}: {b}", m.controller)));
                *self.file_names.borrow_mut() = m.names;
                *self.file_binds.borrow_mut() =
                    m.binds.into_iter().map(|b| ((b.status, b.d1), b)).collect();
                Loaded {
                    text,
                    status,
                    leds: m.leds,
                    ring_ms: Some(m.ring_ms.unwrap_or(led::RING_MS)),
                    request: m.request,
                    controller: Some(m.controller),
                }
            }
            Ok(None) => Loaded {
                text: tr!("no controller mapping for these ports (mappings/*.xml)").to_owned(),
                ..Loaded::default()
            },
            Err(e) => Loaded {
                text: tr!("controller mapping failed").to_owned(),
                status: vec![e],
                ..Loaded::default()
            },
        }
    }

    /// The controller's audio interface becomes the output (PREFS "Use controller audio when connected").
    fn switch_audio(&self, app: &App, link: &mut Link) {
        if !prefs_audio() {
            link.audio = tr!("audio unchanged (PREFS: controller audio off)").into();
            return;
        }
        match audio::set_output(Some(&link.controller)) {
            Ok(dev) => {
                eprintln!("midi: audio output: {dev}");
                (link.audio, link.switched) = (tr!("audio: {}", dev), true);
                self.notify(tr!("Audio output: {}", link.controller));
            }
            Err(e) => {
                eprintln!("midi: {}: audio output not switched: {e}", link.controller);
                link.audio = tr!("audio unchanged (no output device of its own)").into();
                self.notify(tr!("Audio output: unchanged").into());
            }
        }
        self.show_headphones(app);
    }

    /// The previous output device comes back (controller unplugged, or PREFS switched off); nothing to do when a device picked in
    /// PREFS already replaced the controller's interface.
    fn restore_audio(&self, app: &App) {
        if audio::controller().is_none() {
            return;
        }
        match audio::set_output(None) {
            Ok(dev) => {
                eprintln!("midi: audio output restored: {dev}");
                self.notify(tr!("Audio output: restored").into());
            }
            Err(e) => {
                eprintln!("midi: audio output not restored: {e}");
                self.notify(tr!("Audio output: could not restore").into());
            }
        }
        self.show_headphones(app);
    }

    /// PREFS "Use controller audio when connected" changed while a controller is connected.
    pub(super) fn audio_pref_changed(&self, app: &App) {
        let Some(mut link) = self.link.borrow_mut().take() else {
            return;
        };
        match (prefs_audio(), link.switched) {
            (true, false) => self.switch_audio(app, &mut link),
            (false, true) => {
                self.restore_audio(app);
                (link.audio, link.switched) = (
                    tr!("audio unchanged (PREFS: controller audio off)").into(),
                    false,
                );
            }
            _ => {}
        }
        app.global::<MidiState>().set_link(link.text().into());
        *self.link.borrow_mut() = Some(link);
    }

    /// The mixer's status line names the headphone route; it changes with the device.
    fn show_headphones(&self, app: &App) {
        let Some(bus) = self.decks.iter().find_map(|d| d.with_player(|p| p.bus())) else {
            return;
        };
        let text = match bus.headphones() {
            audio::Headphones::Device(n) => tr!("Headphones: {}", n),
            audio::Headphones::Failed(why) => why,
            audio::Headphones::Split => String::new(),
        };
        let mx = app.global::<MixerState>();
        let mut b = mx.get_bus();
        b.status = text.into();
        mx.set_bus(b);
    }

    /// Asks the controller for its state ([`dump`]) once the event loop runs ([`Midi::start_dump`]); the replies are applied while the window is open.
    fn request_dump(&self, link: &mut Link, req: Vec<u8>) {
        if req.is_empty() {
            return;
        }
        *self.dump_due.borrow_mut() = Some(req);
        link.dump = tr!("state dump requested").into();
    }

    /// Asks the controller for its state again (the layout widened: the controls of the decks it shows were ignored so far).
    pub(super) fn request_state_again(&self) {
        let req = self.request.borrow().clone();
        if !req.is_empty() {
            *self.dump_due.borrow_mut() = Some(req);
        }
    }

    /// Sends the pending request and opens the reply window. Runs from the 5 ms input tick and ahead of every light update, so the
    /// request leaves the sink before the start-up lights, and the window starts when the loop does (not during the slow part of
    /// launch, where the replies would otherwise wait in the channel until it expired).
    pub(super) fn start_dump(&self) {
        let Some(req) = self.dump_due.borrow_mut().take() else {
            return;
        };
        let out = self.out.borrow();
        let Some(sink) = out.as_ref() else { return };
        sink.send_sysex(req.clone());
        self.dump.set(Some(dump::Window::new(Instant::now())));
        if self.log {
            eprintln!("midi: state dump requested: {req:02X?}");
        }
    }

    /// A reply is over: say what it did.
    pub(super) fn dump_tick(&self) {
        let Some(w) = self.dump.get() else { return };
        if w.open(Instant::now()) {
            return;
        }
        self.dump.set(None);
        let note = if w.seen == 0 {
            tr!("no reply to the state dump: controls sync on their first move").to_owned()
        } else {
            tr!("state dump: {} of {} reports applied", w.applied, w.seen)
        };
        eprintln!("midi: {note}");
        let (Some(app), Some(link)) = (
            self.app.upgrade(),
            self.link.borrow_mut().as_mut().map(|l| {
                l.dump = note;
                l.text()
            }),
        ) else {
            return;
        };
        app.global::<MidiState>().set_link(link.into());
    }

    /// One report of the dump window: [`dump::plan`] says what it may do. `None` outside the window.
    pub(super) fn dump_step(&self, app: &App, ev: Ev) -> Option<Outcome> {
        use dump::Step;
        let mut w = self.dump.get().filter(|w| w.open(Instant::now()))?;
        let step = dump::plan(|k| self.find(k), ev, active());
        let outcome = match step {
            Step::Knob => self.dispatch(app, ev),
            Step::Layer(d) => {
                self.select_layer(app, d);
                Outcome::Dump("deck layer")
            }
            Step::Vinyl(d, on) => {
                self.set_vinyl(d, on);
                Outcome::Dump("vinyl mode")
            }
            Step::Cutter(d, on) => {
                self.set_cutter(d, on);
                Outcome::Dump("cutter mode")
            }
            Step::Skip(why) => Outcome::Dump(why),
        };
        w.saw(Instant::now(), !matches!(step, Step::Skip(_)));
        self.dump.set(Some(w));
        Some(outcome)
    }
}
