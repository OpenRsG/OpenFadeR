//! MIDI ports (midir, ALSA sequencer on Linux). Each connected input port's callback runs on
//! midir's own thread, queues the raw 3-byte message and wakes the UI once for a pending batch.
//! The LED output is one connection to the output port of the mapped controller, written by a [`Sink`] thread.

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{channel, Receiver, Sender},
    Arc,
};
use std::{thread::JoinHandle, time::Duration};

/// Gap between two LED messages in ms when the mapping file has no `<outInterval>`: 2 is the commonest value
/// among the shipped files (1 and 4 occur too).
pub const OUT_MS: u64 = 2;

/// ALSA names every port "<client>:<port> <id>"; ours ("openfader:openfader-out 128:1") are never controllers, and they show up in
/// the input list once our LED output is open.
pub fn is_own(name: &str) -> bool {
    name.starts_with("openfader")
}

pub struct Ports {
    /// Dropping a connection closes its port.
    _conns: Vec<MidiInputConnection<()>>,
    pub rx: Receiver<[u8; 3]>,
    pub names: Vec<String>,
    wake: Arc<Wake>,
}

/// One event-loop wake per pending batch, not per MIDI message.
struct Wake {
    pending: AtomicBool,
    notify: fn() -> Result<(), slint::EventLoopError>,
}

impl Wake {
    fn request(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) && (self.notify)().is_err() {
            self.pending.store(false, Ordering::Release);
        }
    }
}

impl Ports {
    /// Rearm before draining: input arriving during dispatch must schedule another turn.
    pub fn begin_input(&self) {
        self.wake.pending.store(false, Ordering::Release);
    }

    pub fn wake_input(&self) {
        self.wake.request();
    }
}

/// Connects every input port whose name contains `filter` (all when `None`). Only channel
/// messages (3 bytes) are forwarded; sysex, clock and sensing are dropped by midir's default filter.
pub fn connect(
    filter: Option<&str>,
    notify: fn() -> Result<(), slint::EventLoopError>,
) -> Result<Ports, String> {
    // One listing: re-listing per index raced with ports coming and going (hotplug, other clients).
    let lister = MidiInput::new("openfader").map_err(|e| e.to_string())?;
    let ports: Vec<_> = lister
        .ports()
        .into_iter()
        .filter_map(|p| Some((lister.port_name(&p).ok()?, p)))
        .collect();
    let (tx, rx) = channel();
    let wake = Arc::new(Wake {
        pending: AtomicBool::new(false),
        notify,
    });
    let (mut conns, mut names) = (Vec::new(), Vec::new());
    for (name, port) in ports {
        if is_own(&name) || filter.is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let input = MidiInput::new("openfader").map_err(|e| e.to_string())?;
        let tx = tx.clone();
        let wake = wake.clone();
        let on_msg = move |_: u64, m: &[u8], _: &mut ()| {
            if let &[a, b, c] = m {
                if tx.send([a, b, c]).is_ok() {
                    wake.request();
                }
            }
        };
        match input.connect(&port, "openfader-in", on_msg, ()) {
            Ok(c) => {
                conns.push(c);
                names.push(name);
            }
            Err(e) => eprintln!("midi: {name}: {e}"),
        }
    }
    Ok(Ports {
        _conns: conns,
        rx,
        names,
        wake,
    })
}

/// Opens the first output port whose name contains `name` (case-insensitive): the controller's LEDs.
pub fn connect_out(name: &str) -> Result<(MidiOutputConnection, String), String> {
    let out = MidiOutput::new("openfader").map_err(|e| e.to_string())?;
    let name = name.to_lowercase();
    let port = out
        .ports()
        .into_iter()
        .find(|p| {
            out.port_name(p)
                .is_ok_and(|n| n.to_lowercase().contains(&name))
        })
        .ok_or_else(|| tr!("no MIDI output port matching {}", format!("{name:?}")))?;
    let pname = out.port_name(&port).unwrap_or_default();
    out.connect(&port, "openfader-out")
        .map(|c| (c, pname))
        .map_err(|e| e.to_string())
}

/// The LED output on its own thread: [`Sink::send`] queues, the thread writes one message per `gap` (`<outInterval>` of the
/// mapping file), however busy the UI thread is. Written straight from the UI thread the 258-light start-up
/// state left in one 94 µs burst, and a receiver that could not keep up lost most of it with no send error
/// (seen on a virtual ALSA port: 1..5 of 258 arrived).
pub struct Sink {
    tx: Option<Sender<Packet>>,
    /// The thread's last words: why it stopped writing.
    lost: Receiver<String>,
    thread: Option<JoinHandle<()>>,
}

/// One thing the sink writes: a channel message, or a system-exclusive one (a state-dump request).
enum Packet {
    Short([u8; 3]),
    Sysex(Vec<u8>),
}

impl Sink {
    pub fn new(mut conn: MidiOutputConnection, gap: Duration) -> Sink {
        let (tx, rx) = channel::<Packet>();
        let (lost_tx, lost) = channel();
        let thread = std::thread::spawn(move || {
            for p in rx {
                let sent = match p {
                    Packet::Short(m) => conn.send(&m),
                    Packet::Sysex(m) => conn.send(&m),
                };
                if let Err(e) = sent {
                    let _ = lost_tx.send(e.to_string()); // no silent retry loop
                    return;
                }
                std::thread::sleep(gap);
            }
        });
        Sink {
            tx: Some(tx),
            lost,
            thread: Some(thread),
        }
    }

    pub fn send(&self, m: [u8; 3]) {
        self.queue(Packet::Short(m));
    }

    /// A complete system-exclusive message (F0 … F7), written in turn with the lights.
    pub fn send_sysex(&self, m: Vec<u8>) {
        self.queue(Packet::Sysex(m));
    }

    fn queue(&self, p: Packet) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(p); // Err: the thread has stopped, `lost` says why
        }
    }

    /// Why the output stopped (once), if it did.
    pub fn lost(&self) -> Option<String> {
        self.lost.try_recv().ok()
    }
}

impl Drop for Sink {
    /// Closing waits for the queued messages, so the exit-time "all dark" ones are out before the port closes.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use midir::os::unix::{VirtualInput, VirtualOutput};

    /// A virtual ALSA output port ("controller") talks to a connected input; skipped without ALSA seq.
    #[test]
    fn virtual_controller_reaches_the_channel() {
        let Ok(out) = MidiOutput::new("rk-test").map(|o| o.create_virtual("rk-virtual-controller"))
        else {
            return eprintln!("skip: no MIDI backend");
        };
        let Ok(mut out) = out else {
            return eprintln!("skip: no ALSA seq");
        };
        let ports = connect(Some("rk-virtual-controller"), || Ok(())).unwrap();
        assert_eq!(ports.names.len(), 1);
        out.send(&[0xB0, 7, 100]).unwrap();
        out.send(&[0x90, 36, 0]).unwrap();
        assert_eq!(
            ports.rx.recv_timeout(Duration::from_secs(2)),
            Ok([0xB0, 7, 100])
        );
        assert_eq!(
            ports.rx.recv_timeout(Duration::from_secs(2)),
            Ok([0x90, 36, 0])
        );
    }

    /// A receiver that needs 100 µs per message lost an unpaced 258-message burst with no send error (1..5 arrived);
    /// the sink's 1 ms gap lets all of it through. Skipped without ALSA seq.
    #[test]
    fn sink_gap_lets_a_slow_receiver_keep_up() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering::SeqCst},
            Arc,
        };
        let got = Arc::new(AtomicUsize::new(0));
        let g = got.clone();
        let slow = move |_: u64, _: &[u8], _: &mut ()| {
            g.fetch_add(1, SeqCst);
            std::thread::sleep(Duration::from_micros(100));
        };
        let Ok(input) =
            MidiInput::new("rk-sink-test").map(|i| i.create_virtual("rk-sink-slow", slow, ()))
        else {
            return eprintln!("skip: no MIDI backend");
        };
        let Ok(_input) = input else {
            return eprintln!("skip: no ALSA seq");
        };
        let sink = Sink::new(
            connect_out("rk-sink-slow").unwrap().0,
            Duration::from_millis(1),
        );
        (0..258u32).for_each(|i| sink.send([0x90, (i % 128) as u8, 0x7F]));
        assert_eq!(sink.lost(), None);
        drop(sink); // waits for the queue
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(got.load(SeqCst), 258);
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn input_wakes_coalesce_and_rearm_before_dispatch() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let wake = Wake {
            pending: AtomicBool::new(false),
            notify: || {
                CALLS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        };
        for _ in 0..1000 {
            wake.request();
        }
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        wake.pending.store(false, Ordering::Release);
        // A message arriving while the UI dispatches this batch needs its own wake.
        wake.request();
        wake.request();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_input_wake_can_be_retried() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let wake = Wake {
            pending: AtomicBool::new(false),
            notify: || {
                CALLS.fetch_add(1, Ordering::SeqCst);
                Err(slint::EventLoopError::EventLoopTerminated)
            },
        };
        wake.request();
        wake.request();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        assert!(!wake.pending.load(Ordering::Acquire));
    }
}
