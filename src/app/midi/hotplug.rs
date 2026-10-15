//! Controller hot-plug: [`Watcher`] polls midir's input port list on its own thread and reports each change of the list; the UI thread
//! compares it with the ports it knows ([`diff`]) and rebuilds the connection ([`super::session`]).
//!
//! A port is identified by its name without ALSA's trailing " client:port" address ([`key`]): a controller that comes back after a
//! replug keeps its key but gets a new address, which [`diff`] reports as [`Change::Renamed`] (the old connection is dead).
//! ponytail: midir has no hot-plug callback, so the list is polled every [`POLL`]; ALSA's sequencer announce port would push
//! changes instead, if a second of latency ever matters (nothing is lost meanwhile: the old ports just stay open and silent).

use super::port::is_own;
use midir::MidiInput;
use std::{
    fmt,
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    thread::JoinHandle,
    time::Duration,
};

/// How often the port list is read.
const POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Appeared(String),
    Disappeared(String),
    /// Same port (same [`key`]) under another address: unplugged and plugged back in between two reads.
    Renamed {
        from: String,
        to: String,
    },
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Change::Appeared(n) => write!(f, "port appeared: {n}"),
            Change::Disappeared(n) => write!(f, "port disappeared: {n}"),
            Change::Renamed { from, to } => write!(f, "port renamed: {from} -> {to}"),
        }
    }
}

/// "Test Controller:Test Controller MIDI 1 36:0" → "Test Controller:Test Controller MIDI 1": the name without the ALSA "client:port" address, if it has one.
pub fn key(name: &str) -> &str {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match name.rsplit_once(' ') {
        Some((k, addr))
            if addr
                .split_once(':')
                .is_some_and(|(c, p)| digits(c) && digits(p)) =>
        {
            k
        }
        _ => name,
    }
}

/// What changed between two port lists. Ports with the same name in both are unchanged; of the rest, those sharing a [`key`] are
/// paired in order as renamed, the others appeared or disappeared. Disappeared first, then renamed, then appeared, each in list order.
pub fn diff(old: &[String], new: &[String]) -> Vec<Change> {
    let mut gone: Vec<&String> = old.iter().collect();
    let mut fresh: Vec<&String> = Vec::new();
    for n in new {
        match gone.iter().position(|o| *o == n) {
            Some(i) => {
                gone.remove(i);
            }
            None => fresh.push(n),
        }
    }
    let (mut renamed, mut appeared) = (Vec::new(), Vec::new());
    for n in fresh {
        match gone.iter().position(|o| key(o) == key(n)) {
            Some(i) => renamed.push(Change::Renamed {
                from: gone.remove(i).clone(),
                to: n.clone(),
            }),
            None => appeared.push(Change::Appeared(n.clone())),
        }
    }
    let disappeared = gone.into_iter().map(|g| Change::Disappeared(g.clone()));
    disappeared.chain(renamed).chain(appeared).collect()
}

/// The input ports a controller can use: every one whose name contains `filter` (all when `None`), except our own.
fn read(input: &MidiInput, filter: Option<&str>) -> Vec<String> {
    input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .filter(|n| !is_own(n) && filter.is_none_or(|f| n.contains(f)))
        .collect()
}

/// The port list now (read once, on the calling thread).
pub fn snapshot(filter: Option<&str>) -> Result<Vec<String>, String> {
    MidiInput::new("openfader-list")
        .map(|i| read(&i, filter))
        .map_err(|e| e.to_string())
}

/// The polling thread. Dropping it stops the thread (at once: it waits on the stop channel, not on a sleep).
pub struct Watcher {
    rx: Receiver<Vec<String>>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts watching; `known` is the list the caller already has (only a different list is reported).
    pub fn start(filter: Option<String>, known: Vec<String>) -> Watcher {
        Self::start_every(POLL, filter, known)
    }

    fn start_every(every: Duration, filter: Option<String>, mut known: Vec<String>) -> Watcher {
        let (tx, rx) = channel();
        let (stop, stop_rx) = channel::<()>();
        let thread = std::thread::Builder::new()
            .name("midi-hotplug".into())
            .spawn(move || {
                let Ok(input) = MidiInput::new("openfader-watch") else {
                    return eprintln!("midi: hot-plug disabled: no MIDI backend");
                };
                while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(every) {
                    let now = read(&input, filter.as_deref());
                    if now != known {
                        known = now.clone();
                        if tx.send(now).is_err() {
                            return;
                        }
                    }
                }
            });
        Watcher {
            rx,
            stop: Some(stop),
            thread: thread.ok(),
        }
    }

    /// The newest port list reported since the last call, if the ports changed.
    pub fn latest(&self) -> Option<Vec<String>> {
        self.rx.try_iter().last()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop = None; // the thread's recv sees the disconnect and returns
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use midir::{MidiOutput, os::unix::VirtualOutput};

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    const DECK4: &str = "Test Controller:Test Controller MIDI 1 36:0";

    #[test]
    fn key_drops_only_the_alsa_address() {
        assert_eq!(key(DECK4), "Test Controller:Test Controller MIDI 1");
        assert_eq!(
            key("Midi Through:Midi Through Port-0 14:0"),
            "Midi Through:Midi Through Port-0"
        );
        assert_eq!(
            key("Test Controller MIDI 1"),
            "Test Controller MIDI 1",
            "a name without an address is its own key (other platforms)"
        );
        assert_eq!(key("USB MIDI 2:1x"), "USB MIDI 2:1x");
    }

    #[test]
    fn appeared_and_disappeared() {
        let through = "Midi Through:Midi Through Port-0 14:0";
        assert_eq!(diff(&list(&[through]), &list(&[through])), vec![]);
        assert_eq!(
            diff(&list(&[through]), &list(&[through, DECK4])),
            vec![Change::Appeared(DECK4.into())]
        );
        assert_eq!(
            diff(&list(&[through, DECK4]), &list(&[through])),
            vec![Change::Disappeared(DECK4.into())]
        );
        assert_eq!(
            diff(&[], &list(&[DECK4])),
            vec![Change::Appeared(DECK4.into())]
        );
        assert_eq!(
            diff(&list(&[DECK4]), &[]),
            vec![Change::Disappeared(DECK4.into())]
        );
    }

    #[test]
    fn a_new_address_is_a_rename_not_two_events() {
        let back = "Test Controller:Test Controller MIDI 1 40:0";
        assert_eq!(
            diff(&list(&[DECK4]), &list(&[back])),
            vec![Change::Renamed {
                from: DECK4.into(),
                to: back.into()
            }]
        );
        // a different port of the same device is another port
        let other = "Test Controller:Test Controller MIDI 2 36:1";
        assert_eq!(
            diff(&list(&[DECK4]), &list(&[DECK4, other])),
            vec![Change::Appeared(other.into())]
        );
    }

    #[test]
    fn two_identical_controllers_pair_by_name_first() {
        let (a, b, c) = (
            "Pad Box:Pad Box MIDI 1 20:0",
            "Pad Box:Pad Box MIDI 1 24:0",
            "Pad Box:Pad Box MIDI 1 28:0",
        );
        // one of two leaves
        assert_eq!(
            diff(&list(&[a, b]), &list(&[b])),
            vec![Change::Disappeared(a.into())]
        );
        // one leaves and one arrives under the same key: a rename of the one that left
        assert_eq!(
            diff(&list(&[a, b]), &list(&[b, c])),
            vec![Change::Renamed {
                from: a.into(),
                to: c.into()
            }]
        );
        // unrelated devices swap: gone, then appeared
        assert_eq!(
            diff(&list(&[DECK4]), &list(&[a])),
            vec![Change::Disappeared(DECK4.into()), Change::Appeared(a.into())]
        );
    }

    /// A virtual ALSA port named like a controller shows up in the snapshot and in the watcher's report, and goes away again.
    /// Skipped without ALSA seq. The real hot-plug is the same list changing.
    #[test]
    fn watcher_reports_a_virtual_port_coming_and_going() {
        const NAME: &str = "rk-hotplug-test";
        let Ok(out) = MidiOutput::new("rk-hotplug-src") else {
            return eprintln!("skip: no MIDI backend");
        };
        let before = snapshot(Some(NAME)).unwrap();
        assert!(before.is_empty(), "{before:?}");
        let w = Watcher::start_every(Duration::from_millis(50), Some(NAME.into()), before.clone());
        let Ok(port) = out.create_virtual(NAME) else {
            return eprintln!("skip: no ALSA seq");
        };
        let wait = |want_empty: bool| {
            for _ in 0..100 {
                if let Some(now) = w.latest().filter(|n| n.is_empty() == want_empty) {
                    return now;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("no report (want empty: {want_empty})");
        };
        let now = wait(false);
        assert_eq!(diff(&before, &now).len(), 1, "{now:?}");
        assert!(matches!(&diff(&before, &now)[0], Change::Appeared(n) if n.contains(NAME)));
        drop(port);
        let gone = wait(true);
        assert!(matches!(&diff(&now, &gone)[0], Change::Disappeared(n) if n.contains(NAME)));
        drop(w); // joins the thread promptly
    }
}
