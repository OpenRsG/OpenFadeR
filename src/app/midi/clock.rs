//! MIDI clock output on its own port and thread (`OPENFADER_MIDI_CLOCK`).
//!
//! Set `OPENFADER_MIDI_CLOCK` to a substring of an output port name (the way
//! [`super::port::connect_out`] does for the LED port, and `OPENFADER_MIDI_PORT`
//! for the inputs) and every time the app plays it sends a MIDI realtime clock: 24
//! pulses per quarter note at the master deck's effective tempo, START (0xFA)
//! when a deck starts and STOP (0xFC) when the last one stops. That lets external
//! gear follow the mix: a eurorack sequencer, a drum machine, MIDI-synced lighting,
//! a hardware daisy chain. The master deck is the tempo reference
//! ([`crate::app::deck::sync`]); when no deck is master, the first playing deck's
//! tempo is used.
//!
//! The clock lives on its own `MidiOutputConnection` and thread rather than the
//! controller's LED [`super::port::Sink`] for two reasons: the sink paces one
//! message every `<outInterval>` ms (that gap would turn a 20.8 ms pulse train at
//! 120 BPM into a jittery mess), and the sink is shared by all LED output. Realtime
//! clock needs a clean, dedicated pipe.
//!
//! The UI thread only writes two atomics from its tick (tempo in BPM as f64 bits,
//! and whether the master deck is playing); the clock thread reads them on every
//! pulse and sleeps until the next one, so a busy UI thread never delays a pulse.
//! Tempo changes are picked up on the next pulse; the phase is kept by advancing a
//! deadline by the pulse interval, never by sleeping a fresh duration each time
//! (that would accumulate slew).
//!
//! Hot-plug: when the port goes away the pulse thread marks the clock [`Snap::dead`]
//! and closes. The UI tick sees that, and throttled to one attempt a second, reopens
//! the port by name once it comes back ([`Midi::clock_tick`]). Unplugging a MIDI
//! interface mid-set and plugging it in again re-syncs without restarting the app.

use super::port;
use midir::MidiOutputConnection;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// MIDI realtime messages (1 byte, no channel).
const CLOCK: u8 = 0xF8;
const START: u8 = 0xFA;
// CONTINUE (0xFB) is deliberately not used: MIDI clock treats a pause as a STOP.
const STOP: u8 = 0xFC;
/// Pulses per quarter note (the MIDI clock resolution).
const PPQN: u64 = 24;
/// The idle gap when nothing plays, and the floor for a sane pulse period.
const IDLE_MICROS: u64 = 20_000;

/// The tempo and transport the clock thread reads (written on the UI thread).
struct Snap {
    /// Effective master tempo in BPM, as f64 bits; 0 = none / not playing.
    bpm: AtomicU64,
    /// The master (or fallback) deck is playing.
    running: AtomicBool,
    /// Drop-set: make the pulse loop return so we can join.
    quit: AtomicBool,
    /// Set by the pulse loop when its connection dies: the port is gone, and the UI
    /// tick reopens it by name once it is back. Sticky, where a channel is one-shot.
    dead: AtomicBool,
}

impl Default for Snap {
    fn default() -> Self {
        Self {
            bpm: AtomicU64::new(0),
            running: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            dead: AtomicBool::new(false),
        }
    }
}

/// A dedicated MIDI clock output: its own connection and pulse thread.
pub struct Clock {
    /// The port name substring this clock is bound to (the `OPENFADER_MIDI_CLOCK` value),
    /// kept so the UI tick can reopen the same target after a replug.
    name: String,
    thread: Option<JoinHandle<()>>,
    snap: Arc<Snap>,
}

impl Clock {
    /// Opens the first output port whose name contains `name` and starts the pulse
    /// thread there. `Err` when no such port exists or midir cannot open it.
    pub fn start(name: &str) -> Result<Clock, String> {
        let (conn, _pname) = port::connect_out(name)?;
        Ok(Clock::from_connection(name, conn))
    }

    /// Builds a clock on an already-open output connection.
    fn from_connection(name: &str, conn: MidiOutputConnection) -> Clock {
        let snap = Arc::new(Snap::default());
        let snap_t = snap.clone();
        let thread = std::thread::spawn(move || pulse(conn, snap_t));
        Clock {
            name: name.to_owned(),
            thread: Some(thread),
            snap,
        }
    }

    /// The output port this clock writes to.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The UI tick's snapshot of the master deck: tempo in BPM and whether it plays.
    /// 0 BPM / stopped sends no pulses and (on a running→stopped edge) a STOP.
    pub fn set(&self, bpm: f64, running: bool) {
        self.snap.bpm.store(bpm.to_bits(), Ordering::Relaxed);
        self.snap.running.store(running, Ordering::Relaxed);
    }

    /// Whether the pulse thread is still alive on a working connection. `false` once
    /// the output port is unplugged (the loop closed itself) or after [`Drop`].
    pub fn alive(&self) -> bool {
        !self.snap.dead.load(Ordering::Relaxed)
    }
}

/// The UI tick's `clock_tick`: resolve the master deck (fallback: the first playing
/// deck) and push its tempo + transport to the clock. Runs on the UI thread.
///
/// When the clock is gone (the port was unplugged, or it never opened because the
/// port was absent at launch) this reopens the target port by name — at most once a
/// second, so a missing interface is polled gently and a replug picks the clock back
/// up in under a second.
impl super::Midi {
    pub(super) fn clock_tick(&self) {
        let target = match self.clock_target.borrow().as_ref() {
            Some(t) => t.clone(),
            None => return, // MIDI clock output not enabled
        };

        // A healthy clock just needs its tempo/transport fed.
        if let Some(clock) = self.clock.borrow().as_ref() {
            if clock.alive() {
                self.push_clock(clock);
                return;
            }
            // Dead: fall through to the throttled reopen below.
        }

        let now = Instant::now();
        if self
            .clock_next_try
            .get()
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(1))
        {
            return; // throttled: one reopen attempt per second
        }

        let ok = match Clock::start(&target) {
            Ok(clock) => {
                eprintln!("midi: MIDI clock on {}", clock.name());
                self.push_clock(&clock);
                *self.clock.borrow_mut() = Some(clock);
                true
            }
            // Port not back yet: stay quiet, the 1-s throttle paces the retry.
            Err(_) => false,
        };
        if ok {
            self.clock_next_try.set(None);
        } else {
            self.clock_next_try.set(Some(now)); // pace the next attempt
        }
    }

    /// Push the master deck's tempo and transport into `clock`.
    fn push_clock(&self, clock: &Clock) {
        let master = self
            .decks
            .iter()
            .find(|d| d.is_master())
            .or_else(|| self.decks.iter().find(|d| d.playing()));
        let (bpm, running) = match master {
            Some(d) => (d.effective_bpm(), d.playing()),
            None => (0.0, false),
        };
        clock.set(bpm, running);
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        self.snap.quit.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The pulse loop. Owns the connection, so a midir failure that drops it also ends the
/// loop: a send error marks the clock [`Snap::dead`] and returns, and the UI tick
/// reopens the port when it comes back.
fn pulse(mut conn: MidiOutputConnection, snap: Arc<Snap>) {
    let mut running = false;
    // The phase: the next CLOCK is exactly one pulse period after the previous one,
    // so tempo nudges never accumulate into slew across long sets.
    let mut next = Instant::now();
    loop {
        if snap.quit.load(Ordering::Relaxed) {
            return;
        }
        let bpm = f64::from_bits(snap.bpm.load(Ordering::Relaxed));
        let want = snap.running.load(Ordering::Relaxed);
        if want != running {
            running = want;
            let status = match want {
                // A pause in MIDI clock is a STOP; nothing tricky about CONTINUE.
                true => START,
                false => STOP,
            };
            if conn.send(&[status]).is_err() {
                snap.dead.store(true, Ordering::Relaxed);
                return;
            }
            next = Instant::now();
        }
        if !running {
            // No pulses while stopped; poll the value change without spinning.
            std::thread::sleep(Duration::from_micros(IDLE_MICROS));
            continue;
        }
        let period = Duration::from_micros(safe_micros(bpm));
        let wait = next.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            std::thread::sleep(wait); // wakes a little early; see below
        }
        if conn.send(&[CLOCK]).is_err() {
            snap.dead.store(true, Ordering::Relaxed);
            return;
        }
        next += period;
        // The OS timer wakes a bit late; schedule the next pulse from the instant the
        // deadline would have been, tolerating the small overshoot.
        if next < Instant::now() {
            next = Instant::now();
        }
    }
}

/// Microseconds between two clock pulses at `bpm` (24 PPQN), clamped to sane bounds:
/// a BPM of 0 or less than ~10 would otherwise ask for an absurd (or infinite) wait.
fn safe_micros(bpm: f64) -> u64 {
    if !bpm.is_finite() || bpm < 10.0 {
        return IDLE_MICROS; // idle-ish
    }
    let us = 60_000_000.0 / (bpm * PPQN as f64);
    // Fast BPM still gets a real pulse (a 100 µs floor, ~140 BPM at 24 PPQN ~ 18 ms is well above it).
    (us.round() as u64).clamp(100, 5_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_period_matches_midi_clock_resolution() {
        // 120 BPM -> one CLOCK every 20.833 ms (24 PPQN).
        assert_eq!(safe_micros(120.0), 20_833);
        // 140 BPM -> every 17.857 ms.
        assert_eq!(safe_micros(140.0), 17_857);
    }

    #[test]
    fn missing_or_absurd_bpm_idles_instead_of_spinning() {
        assert_eq!(safe_micros(0.0), IDLE_MICROS);
        assert_eq!(safe_micros(f64::NAN), IDLE_MICROS);
        assert!(safe_micros(1.0) >= IDLE_MICROS, "clamped to a sane idle");
    }
}
