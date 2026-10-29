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
    deck::{active, slots, DECKS},
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
        self.close_claims();
        (self.dump.set(None), *self.dump_due.borrow_mut() = None);
        let held = self.held.take();
        if let Some(app) = self.app.upgrade() {
            for (_, held) in held {
                self.release_held(&app, held);
            }
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
        if slots::set_layer(Default::default()) {
            slots::publish(app);
        }
        self.snap_at.set(None);

        // The ports a controller plugin drives are its own; the rest are ours.
        let wake = port::Wake::new(super::wire::wake_input);
        let claimed = self.open_claims(&wake, &self.names.borrow());
        let ports = port::connect(self.filter.as_deref(), &claimed, wake);
        let driven = self.claimed_text(&self.names.borrow());
        state.set_ports(match &ports {
            Ok(p) if p.names.is_empty() && driven.is_empty() => tr!("no MIDI input ports").into(),
            Ok(p) => p
                .names
                .iter()
                .chain(&driven)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
                .into(),
            Err(e) => tr!("MIDI unavailable: {}", e).into(),
        });
        let loaded = self.load_mapping(ports.as_ref().map_or(&[][..], |p| &p.names));
        *self.request.borrow_mut() = loaded.request.clone();
        *self.ports.borrow_mut() = ports.ok();
        *self.leds.borrow_mut() = led::Leds::new(loaded.leds);
        let plugin = self.claimed_label().filter(|_| loaded.controller.is_none());
        state.set_controller(plugin.clone().unwrap_or(loaded.text).into());
        state.set_status(loaded.status.join("; ").into());
        state.set_mapped(self.mapped() as i32);
        crate::app::diag::controller(&self.names.borrow(), loaded.controller.as_deref(), self.file_binds.borrow().len());
        self.restart_lights(loaded.ring_ms.filter(|_| self.out.borrow().is_some()));

        // What the user is told: the old controller left, the new one arrived (the same one again = a port change, not news).
        let changed = before.as_ref().map(|b| &b.controller) != loaded.controller.as_ref();
        if let (Some(b), true) = (&before, changed) {
            self.notify(tr!("{} disconnected", b.controller));
            if b.switched {
                self.restore_audio();
            }
        }
        let Some(name) = loaded.controller else {
            state.set_connected(plugin.unwrap_or_default().into());
            state.set_link("".into());
            // Ports but no mapping: the repositories may have one for them ([`crate::app::catalog`]).
            catalog::unmapped(
                self.names
                    .borrow()
                    .iter()
                    .filter(|n| !claimed.contains(n))
                    .cloned()
                    .collect(),
            );
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

    /// The controller's audio interface becomes the output (PREFS "Use controller audio when connected"). The interface is looked
    /// up on a worker thread (listing ALSA devices takes up to a second): the stream moves when it is found
    /// ([`Midi::audio_settled`]), the UI thread does not wait.
    fn switch_audio(self: &Rc<Self>, app: &App, link: &mut Link) {
        if !prefs_audio() {
            link.audio = tr!("audio unchanged (PREFS: controller audio off)").into();
            return;
        }
        match audio::set_output(Some(&link.controller)) {
            Ok(audio::Switch::Done(dev)) => self.apply_switch(link, Ok(dev)),
            Ok(audio::Switch::Later) => {
                (link.audio, link.switched) = (tr!("audio: looking for {}…", link.controller), true);
                let (weak, controller) = (Rc::downgrade(self), link.controller.clone());
                audio::when_settled(move |r| {
                    if let Some(m) = weak.upgrade() {
                        m.audio_settled(&controller, r);
                    }
                });
            }
            Err(e) => self.apply_switch(link, Err(e)),
        }
        self.show_headphones(app);
    }

    /// What the audio switch did: the log line, the link's status text and the toast.
    fn apply_switch(&self, link: &mut Link, r: Result<String, String>) {
        match r {
            Ok(dev) => {
                eprintln!("midi: audio output: {dev}");
                (link.audio, link.switched) = (tr!("audio: {}", dev), true);
                self.notify(tr!("Audio output: {}", link.controller));
            }
            Err(e) => {
                eprintln!("midi: {}: audio output not switched: {e}", link.controller);
                (link.audio, link.switched) = (tr!("audio unchanged (no output device of its own)").into(), false);
                self.notify(tr!("Audio output: unchanged").into());
            }
        }
    }

    /// The interface of `controller` was looked up and the stream moved (or could not): the same as [`Midi::switch_audio`] does at once
    /// when the device is known. Nothing when that controller left meanwhile.
    fn audio_settled(&self, controller: &str, r: Result<String, String>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let mut link = self.link.borrow_mut();
        let Some(link) = link.as_mut().filter(|l| l.controller == controller) else {
            return;
        };
        self.apply_switch(link, r);
        app.global::<MidiState>().set_link(link.text().into());
        self.show_headphones(&app);
    }

    /// The previous output device comes back (controller unplugged, or PREFS switched off); nothing to do when a device picked in
    /// PREFS already replaced the controller's interface.
    fn restore_audio(self: &Rc<Self>) {
        if audio::controller().is_none() {
            return;
        }
        match audio::set_output(None) {
            Ok(audio::Switch::Later) => {
                let weak = Rc::downgrade(self);
                audio::when_settled(move |r| {
                    if let Some(m) = weak.upgrade() {
                        m.audio_restored(r);
                    }
                });
            }
            Ok(audio::Switch::Done(dev)) => self.audio_restored(Ok(dev)),
            Err(e) => self.audio_restored(Err(e)),
        }
    }

    fn audio_restored(&self, r: Result<String, String>) {
        match r {
            Ok(dev) => {
                eprintln!("midi: audio output restored: {dev}");
                self.notify(tr!("Audio output: restored").into());
            }
            Err(e) => {
                eprintln!("midi: audio output not restored: {e}");
                self.notify(tr!("Audio output: could not restore").into());
            }
        }
        if let Some(app) = self.app.upgrade() {
            self.show_headphones(&app);
        }
    }

    /// PREFS "Use controller audio when connected" changed while a controller is connected.
    pub(super) fn audio_pref_changed(self: &Rc<Self>, app: &App) {
        let Some(mut link) = self.link.borrow_mut().take() else {
            return;
        };
        match (prefs_audio(), link.switched) {
            (true, false) => self.switch_audio(app, &mut link),
            (false, true) => {
                self.restore_audio();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::set_for_test;
    use cpal::traits::{DeviceTrait, HostTrait};
    use openfader::{
        library::{own::Own, Library},
        store::Store,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    static LOOKED_UP: AtomicBool = AtomicBool::new(false);

    /// A sound server's own output device: not the default one, and nothing that plays on real speakers.
    fn sound_server_device() -> Option<String> {
        cpal::default_host()
            .output_devices()
            .ok()?
            .filter_map(|d| d.name().ok())
            .find(|n| n == "pipewire" || n == "pulse")
    }

    /// Connecting a controller must not wait for ALSA to list its devices (up to a second): the switch returns at once, the stream
    /// keeps playing where it is and moves to the controller's interface when the worker has found it. The test thread plays the UI
    /// thread: it runs what the app's waker posts (`audio::settle`).
    #[test]
    fn a_controller_connect_returns_at_once_and_the_stream_lands_on_its_interface_later() {
        let Some(want) = sound_server_device() else {
            return; // no sound server: nothing to find
        };
        audio::set_waker(|| LOOKED_UP.store(true, Ordering::Release));
        crate::app::headless::init();
        let app = App::new().unwrap();
        let lib: Rc<dyn Library> = Rc::new(Own::new(std::sync::Arc::new(Store::in_memory().unwrap())));
        let decks = crate::app::deck::wire(&app, lib, Default::default());
        set_for_test(|p| p.controller_audio = true);
        let guard = crate::app::midi::wire(&app, decks);
        let midi = &guard.midi;
        let before = audio::status().device;
        let mut link = Link {
            controller: want.clone(),
            audio: String::new(),
            dump: String::new(),
            switched: false,
        };

        let t = Instant::now();
        midi.switch_audio(&app, &mut link);
        let took = t.elapsed();
        eprintln!("switch_audio returned after {took:?}");
        assert!(took < Duration::from_millis(5), "the UI thread waited {took:?}");
        assert!(link.switched && link.audio.contains("looking for"), "{}", link.audio);
        assert_eq!(audio::status().device, before, "the stream plays on while the device is looked up");
        assert_eq!(audio::status().pending.as_deref(), Some(want.as_str()));
        *midi.link.borrow_mut() = Some(link);

        let t = Instant::now();
        while !LOOKED_UP.load(Ordering::Acquire) {
            assert!(t.elapsed() < Duration::from_secs(20), "the lookup never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        audio::settle();
        let st = audio::status();
        assert_eq!((st.device.as_str(), st.source, st.pending), (want.as_str(), "controller", None));
        let link = midi.link.borrow();
        assert_eq!(link.as_ref().map(|l| l.audio.as_str()), Some(format!("audio: {want}").as_str()));
    }
}
