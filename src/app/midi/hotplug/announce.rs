//! ALSA's sequencer announces every client and port that starts, changes or exits on its `System:Announce` port (what `aseqdump -p
//! 0:1` prints). [`Announce`] subscribes a port of its own to it, so the hot-plug thread sleeps until a port really comes or goes
//! instead of reading the list every second. Linux only; without the sequencer [`open`] says no and the list is polled.

use alsa::{
    Direction,
    poll::Descriptors,
    seq::{Addr, EventType, PortCap, PortSubscribe, PortType, Seq},
};
use openfader::device::watch::Settle;
use std::{
    io::{PipeReader, PipeWriter},
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

/// How the sleep ended.
pub enum Wake {
    /// The ports may have changed (an announcement settled), or the fallback interval is over: read the list.
    Read,
    /// The [`PipeWriter`] of [`open`] was dropped.
    Stop,
}

pub struct Announce {
    seq: Seq,
    /// The sequencer's descriptors, then the read end of the stop pipe.
    fds: Vec<libc::pollfd>,
    stop: PipeReader,
}

/// The announcement events that mean the port list changed. Subscriptions between ports (also ours) are announced too and do not.
fn is_topology(t: EventType) -> bool {
    matches!(
        t,
        EventType::PortStart
            | EventType::PortExit
            | EventType::PortChange
            | EventType::ClientStart
            | EventType::ClientExit
            | EventType::ClientChange
    )
}

/// Subscribes to the announcements. Dropping the returned writer wakes [`Announce::wait`] with [`Wake::Stop`].
pub fn open() -> Option<(Announce, PipeWriter)> {
    let seq = Seq::open(None, Some(Direction::Capture), true).ok()?;
    seq.set_client_name(c"openfader-announce").ok()?;
    // write-only and not exported: no other program, and no port list of ours (midir lists SUBS_WRITE ports), shows it
    let port = seq
        .create_simple_port(
            c"announce",
            PortCap::WRITE | PortCap::NO_EXPORT,
            PortType::MIDI_GENERIC | PortType::APPLICATION,
        )
        .ok()?;
    let sub = PortSubscribe::empty().ok()?;
    sub.set_sender(Addr::system_announce());
    sub.set_dest(Addr {
        client: seq.client_id().ok()?,
        port,
    });
    seq.subscribe_port(&sub).ok()?;
    let fds = (&seq, Some(Direction::Capture)).get().ok()?;
    let (stop, wake) = std::io::pipe().ok()?;
    Some((Announce { seq, fds, stop }, wake))
}

impl Announce {
    /// Reads every queued announcement; whether one changed the ports (an overrun counts: events are lost).
    fn drain(&self) -> bool {
        let mut input = self.seq.input();
        let mut hit = false;
        while input.event_input_pending(true).unwrap_or(0) > 0 {
            match input.event_input() {
                Ok(e) => hit |= is_topology(e.get_type()),
                Err(_) => return true,
            }
        }
        hit
    }

    /// Blocks until the ports changed (a plug raises several announcements: the wait goes on until they settle), `idle` is over, or
    /// the stop pipe closes.
    pub fn wait(&self, idle: Duration) -> Wake {
        let end = Instant::now() + idle;
        let mut settle = Settle::new(Duration::from_millis(150), Duration::from_secs(1));
        let mut fds = self.fds.clone();
        fds.push(libc::pollfd {
            fd: self.stop.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        loop {
            let now = Instant::now();
            let left = end.saturating_duration_since(now);
            let ms = settle.wait(now, left).as_millis().min(i32::MAX as u128 - 1) as i32 + 1;
            // SAFETY: `fds` is valid for its length.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
            let now = Instant::now();
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                std::thread::sleep(Duration::from_secs(1)); // not a spin; the caller reads and asks again
                return Wake::Read;
            }
            if fds.last().is_some_and(|f| f.revents != 0) {
                return Wake::Stop;
            }
            if n > 0 && self.drain() {
                settle.hit(now);
            }
            if settle.fire(now) || now >= end {
                return Wake::Read;
            }
        }
    }
}
