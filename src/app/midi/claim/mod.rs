//! Controller plugins on the MIDI session (`openfader::plugin::controller`, docs in `docs/plugins.md`). Before the input ports are
//! opened [`Midi::open_claims`] decides, per port, who drives it ([`decide`]): the driver PREFS > CONTROLLER chose for it, or, on
//! AUTO, the controller plugin that recognises it best; no plugin and the port is the host's own (mapping files). A port a plugin
//! drives bypasses our mapping: its messages, whole, go to the plugin ([`Midi::read_claimed`], from the input wake) and the
//! plugin answers through [`Ctl`], the [`Backend`] the SDK's host callbacks come to: [`control`]'s namespace, the output port (the
//! lights), timers and watches ([`Midi::claim_tick`], on the 5 ms input tick). All of it on the UI thread, like our own mapping.
//!
//! The plugin's soft takeover has a table of its own per session, so a plugin's controls never wait on a CC of ours.

mod control;

#[cfg(test)]
mod tests;

use super::pickup::Pickup;
use super::port::{self, OUT_MS, Sink, Wake};
use super::{HeldAction, Midi};
use crate::app::prefs::{
    self,
    controller::{port_key, Controller, Driver},
};
use midir::MidiInputConnection;
use openfader::plugin::controller::{self, Backend, Loaded, Session};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Watched controls are compared this often.
const POLL: Duration = Duration::from_millis(10);
/// Messages one turn of the UI takes from the plugins' ports.
const BATCH: usize = 256;

/// An input port as PREFS lists it, and the plugin that drives it now.
#[derive(Clone, Debug, PartialEq)]
pub struct PortInfo {
    pub name: String,
    pub driver: Option<String>,
}

static PORTS: Mutex<Vec<PortInfo>> = Mutex::new(Vec::new());

/// The input ports of the last connect (PREFS > CONTROLLER > CONTROLLER PLUGINS).
pub fn ports() -> Vec<PortInfo> {
    PORTS.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// One timer a plugin started.
struct Timer {
    id: u64,
    due: Instant,
    every: Option<Duration>,
}

/// What the host keeps for one session: everything [`Ctl`] reads and writes.
struct State {
    /// The controller's output: one complete MIDI message per call (the lights).
    out: RefCell<Option<Box<dyn Fn(&[u8])>>>,
    /// Watched keys and the value last told to the plugin (`None`: not yet).
    watch: RefCell<Vec<(String, Option<f64>)>>,
    timers: RefCell<Vec<Timer>>,
    next_timer: Cell<u64>,
    takeover: RefCell<HashSet<String>>,
    label: RefCell<String>,
    pick: Pickup,
    held: RefCell<HashMap<(usize, i32, i32), HeldAction>>,
    polled: Cell<Instant>,
}

impl State {
    fn new(out: Option<Box<dyn Fn(&[u8])>>) -> State {
        State {
            out: RefCell::new(out),
            watch: RefCell::default(),
            timers: RefCell::default(),
            next_timer: Cell::new(1),
            takeover: RefCell::default(),
            label: RefCell::default(),
            pick: Pickup::new(),
            held: RefCell::default(),
            polled: Cell::new(Instant::now()),
        }
    }

    /// The timers that are due; a repeating one is set for its next turn.
    fn due(&self, now: Instant) -> Vec<u64> {
        let mut due = Vec::new();
        self.timers.borrow_mut().retain_mut(|t| {
            if t.due > now {
                return true;
            }
            due.push(t.id);
            t.every.is_some_and(|e| {
                t.due = now + e;
                true
            })
        });
        due
    }

    /// The watched controls whose value is not the one the plugin was told last.
    fn changes(&self, midi: &Midi) -> Vec<(String, f64)> {
        let mut changed = Vec::new();
        for (key, last) in self.watch.borrow_mut().iter_mut() {
            let v = midi.ctl_get(key);
            if last.is_none_or(|l| !(l == v || l.is_nan() && v.is_nan())) {
                *last = Some(v);
                changed.push((key.clone(), v));
            }
        }
        changed
    }
}

/// One claimed port.
pub(super) struct Claimed {
    port: String,
    plugin: String,
    /// `None` while the plugin opens it, and after it was closed.
    session: Option<Session>,
    rx: Receiver<Vec<u8>>,
    /// Dropping it closes the input port.
    _conn: Option<MidiInputConnection<()>>,
    st: State,
}

/// The claimed ports, and the driver choice (PREFS `controller_plugins`) they were made with.
#[derive(Default)]
pub(super) struct Claims {
    list: Vec<Claimed>,
    applied: String,
}

impl Claims {
    pub(super) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// The host callbacks of one plugin call.
struct Ctl<'a> {
    midi: &'a Midi,
    st: &'a State,
}

impl Backend for Ctl<'_> {
    fn get(&self, key: &str) -> f64 {
        self.midi.ctl_get(key)
    }

    fn set(&self, key: &str, value: f64) -> bool {
        let takeover = self.st.takeover.borrow().contains(key);
        self.midi.ctl_set(self.st, takeover, key, value)
    }

    fn midi_out(&self, msg: &[u8]) {
        if let Some(out) = self.st.out.borrow().as_ref() {
            out(msg);
        }
    }

    fn timer(&self, ms: u32, repeat: bool) -> u64 {
        let id = self.st.next_timer.replace(self.st.next_timer.get() + 1);
        let every = Duration::from_millis(u64::from(ms.max(1)));
        self.st.timers.borrow_mut().push(Timer {
            id,
            due: Instant::now() + every,
            every: repeat.then_some(every),
        });
        id
    }

    fn cancel(&self, id: u64) {
        self.st.timers.borrow_mut().retain(|t| t.id != id);
    }

    fn watch(&self, key: &str, on: bool) {
        let mut w = self.st.watch.borrow_mut();
        w.retain(|(k, _)| k != key);
        if on {
            w.push((key.to_owned(), None));
        }
    }

    fn takeover(&self, key: &str, on: bool) {
        let mut t = self.st.takeover.borrow_mut();
        if on {
            t.insert(key.to_owned());
        } else {
            t.remove(key);
        }
    }

    fn label(&self, text: &str) {
        *self.st.label.borrow_mut() = text.to_owned();
    }
}

/// Who drives which of the input ports `names`: the port, the plugin, and whether the user assigned it (`forced`). AUTO asks every
/// plugin in turn; the best score wins, the first of equals.
fn decide<'a>(
    names: &[String],
    c: &Controller,
    libs: &[&'a Loaded],
) -> Vec<(String, &'a Loaded, bool)> {
    names
        .iter()
        .filter_map(|n| match c.driver(n) {
            Driver::Mapping => None,
            Driver::Plugin(p) => libs
                .iter()
                .find(|l| l.name() == p)
                .map(|l| (n.clone(), *l, true)),
            Driver::Auto => libs
                .iter()
                .map(|l| (l.probe(n), *l))
                .filter(|(score, _)| *score > 0)
                .reduce(|a, b| if b.0 > a.0 { b } else { a })
                .map(|(_, l)| (n.clone(), l, false)),
        })
        .collect()
}

impl Midi {
    /// Opens the ports the controller plugins drive; the input ports that are the host's own are what is left of `names` without
    /// the returned ones. Call after [`Midi::close`], before the other ports are connected.
    pub(super) fn open_claims(&self, wake: &Arc<Wake>, names: &[String]) -> Vec<String> {
        let c = prefs::with(|p| p.controller.clone());
        // Before any plugin runs, so a setting it changes in `open` does not look like a new driver choice.
        self.claimed.borrow_mut().applied = c.controller_plugins.clone();
        let libs: Vec<&Loaded> = controller::enabled().collect();
        let mut claimed = Vec::new();
        let mut list = Vec::new();
        for (name, lib, forced) in decide(names, &c, &libs) {
            let started = port::raw(wake, &name).and_then(|(conn, rx)| {
                let out = port::connect_out(&name)
                    .or_else(|_| port::connect_out(port_key(&name)))
                    .ok()
                    .map(|(conn, _)| Sink::new(conn, Duration::from_millis(OUT_MS)));
                self.start(lib, &name, forced, rx, Some(conn), out)
            });
            match started {
                Ok(c) => {
                    claimed.push(name);
                    list.push(c);
                }
                Err(e) => self.notify(tr!("{}: {}", lib.name(), e)),
            }
        }
        self.claimed.borrow_mut().list = list;
        claimed
    }

    /// The plugin `lib` opens `port`, whose messages arrive on `rx` and whose lights go to `out`.
    fn start(
        &self,
        lib: &Loaded,
        port: &str,
        forced: bool,
        rx: Receiver<Vec<u8>>,
        conn: Option<MidiInputConnection<()>>,
        out: Option<Sink>,
    ) -> Result<Claimed, String> {
        let out = out.map(|sink| -> Box<dyn Fn(&[u8])> {
            Box::new(move |m| match *m {
                [a, b, c] => sink.send([a, b, c]),
                _ => sink.send_sysex(m.to_vec()),
            })
        });
        let mut c = Claimed {
            port: port.to_owned(),
            plugin: lib.name().to_owned(),
            session: None,
            rx,
            _conn: conn,
            st: State::new(out),
        };
        let s = lib.open(port, forced, &Ctl { midi: self, st: &c.st })?;
        c.session = Some(s);
        Ok(c)
    }

    /// Ends every session: each plugin may still send its last lights; dropping the port waits until they are out.
    pub(super) fn close_claims(&self) {
        let list = std::mem::take(&mut self.claimed.borrow_mut().list);
        for mut c in list {
            if let Some(s) = c.session.take() {
                s.close(&Ctl {
                    midi: self,
                    st: &c.st,
                });
            }
            let held = c.st.held.take();
            if let Some(app) = self.app.upgrade() {
                for (_, held) in held {
                    self.release_held(&app, held);
                }
            }
        }
    }

    /// Every message waiting on the plugins' ports goes to its plugin.
    pub(super) fn read_claimed(&self) {
        let claims = self.claimed.borrow();
        let mut left = BATCH;
        for c in &claims.list {
            let Some(s) = &c.session else { continue };
            let ctl = Ctl {
                midi: self,
                st: &c.st,
            };
            while left > 0
                && let Ok(msg) = c.rx.try_recv()
            {
                left -= 1;
                if self.log {
                    eprintln!("midi {} {msg:02X?} -> {}", c.port, c.plugin);
                }
                s.midi(&msg, &ctl);
            }
        }
        if left == 0
            && let Some(p) = self.ports.borrow().as_ref()
        {
            p.wake_input();
        }
    }

    /// The 5 ms tick: due timers, and (every [`POLL`]) the watched controls that changed.
    pub(super) fn claim_tick(&self) {
        let claims = self.claimed.borrow();
        if claims.list.is_empty() {
            return;
        }
        let now = Instant::now();
        for c in &claims.list {
            let Some(s) = &c.session else { continue };
            let ctl = Ctl {
                midi: self,
                st: &c.st,
            };
            for id in c.st.due(now) {
                s.timer(id, &ctl);
            }
            if now.duration_since(c.st.polled.get()) >= POLL {
                c.st.polled.set(now);
                for (key, v) in c.st.changes(self) {
                    s.changed(&key, v, &ctl);
                }
            }
        }
    }

    /// PREFS changed the drivers: the ports are opened again with the new choice.
    pub(super) fn claims_changed(self: &Rc<Self>, p: &prefs::Prefs) {
        if self.claimed.borrow().applied == p.controller.controller_plugins {
            return;
        }
        if let Some(app) = self.app.upgrade() {
            let names = self.names.borrow().clone();
            self.reconnect(&app, names);
        }
    }

    /// Publishes the port list for PREFS, and the claimed ports as "port (plugin)" for the MIDI window.
    pub(super) fn claimed_text(&self, names: &[String]) -> Vec<String> {
        let claims = self.claimed.borrow();
        let driver = |n: &str| {
            claims
                .list
                .iter()
                .find(|c| c.port == n)
                .map(|c| c.plugin.clone())
        };
        *PORTS.lock().unwrap_or_else(PoisonError::into_inner) = names
            .iter()
            .map(|n| PortInfo {
                name: n.clone(),
                driver: driver(n),
            })
            .collect();
        claims
            .list
            .iter()
            .map(|c| format!("{} ({})", c.port, c.plugin))
            .collect()
    }

    /// The CONTROLLER line for the first claimed port: the plugin's own label for the controller, else the port.
    pub(super) fn claimed_label(&self) -> Option<String> {
        let claims = self.claimed.borrow();
        let c = claims.list.first()?;
        let label = c.st.label.borrow();
        Some(if label.is_empty() {
            format!("{}: {}", c.plugin, port_key(&c.port))
        } else {
            format!("{}: {}", c.plugin, label)
        })
    }
}
