//! MIDI clock output on its own port and thread (PREFS > CONTROLLER > OTHERS > MIDI CLOCK OUT: part of an output port's name).
//!
//! External gear (a drum machine, a sequencer, MIDI-synced lighting) follows the mix: 24 pulses per quarter note (CLOCK, 0xF8)
//! locked to the lead deck's beat grid, START (0xFA) on that deck's next bar line, STOP (0xFC) when no deck plays any more. The
//! lead is the master deck while it plays, else the first playing deck ([`crate::app::deck::lead`]); a deck only previewing
//! its cue (CUE held) does not count, and a deck without a grid gives no tempo, so no pulses (and a STOP).
//!
//! Phase: the UI tick hands the thread the lead deck's beat at an instant ([`Pulse`]: the latency-compensated playhead on its
//! grid) and the thread schedules each pulse at the instant that beat reaches the pulse's place, so nudges, pitch bends, a
//! master hand-over and tempo changes are followed. A pulse comes 0.75 to 1.5 periods after the previous one, so a jump
//! of the playhead is caught up gradually instead of in a burst. ponytail: a jump by a number of beats that is not a whole
//! number of bars moves the gear's bar line; it keeps its count from START.
//!
//! The clock has its own `MidiOutputConnection` and thread rather than the controller's LED [`super::port::Sink`]: the sink
//! paces one message per `<outInterval>` ms, which would make a 20 ms pulse train jitter. The thread also opens the port,
//! and opens it again once a second while it is missing (absent at start, or unplugged), so the UI thread never enumerates
//! ports.

use super::port;
use crate::app::deck::Pulse;
use midir::MidiOutputConnection;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// MIDI realtime messages (1 byte, no channel).
const CLOCK: u8 = 0xF8;
const START: u8 = 0xFA;
// CONTINUE (0xFB) is not used: a pause is a STOP, the next play a START on a bar line.
const STOP: u8 = 0xFC;
/// Pulses per quarter note (the MIDI clock resolution).
const PPQN: f64 = 24.0;
/// How often the thread looks at the lead deck while it waits (no pulse due, or for the bar line to START on).
const POLL: Duration = Duration::from_millis(5);
/// A play this close after a bar line (beats) STARTs at once instead of waiting a whole bar.
const START_SLACK: f64 = 0.05;
/// Tempo range (BPM) the clock runs at; outside it the lead deck counts as having none.
const BPM: std::ops::RangeInclusive<f64> = 20.0..=400.0;
/// Spacing of two pulses as a share of the period: how fast the clock catches up with or waits for the lead deck's beat.
const SPACING: [f64; 2] = [0.75, 1.5];
/// How long a missing port waits before the next try.
const RETRY: Duration = Duration::from_secs(1);

/// What one step of [`Sync`] sends.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Out {
    Nothing,
    Stop,
    /// START, then the bar line's first CLOCK.
    Start,
    Clock,
}

impl Out {
    fn bytes(self) -> &'static [u8] {
        match self {
            Out::Nothing => &[],
            Out::Stop => &[STOP],
            Out::Start => &[START, CLOCK],
            Out::Clock => &[CLOCK],
        }
    }
}

/// The clock's transport and phase: which pulse is next and where it belongs on the lead deck's beat.
#[derive(Default)]
struct Sync {
    running: bool,
    /// Pulses sent since START.
    pulses: u64,
    /// The lead deck's beat that START stood for (a bar line), moved by whole beats when the playhead jumps.
    base: f64,
    /// When the last pulse went out.
    last: Option<Instant>,
}

impl Sync {
    /// What to send at `now` for `lead` (the lead deck's [`Pulse`] read at an instant, `None` when no deck plays on a grid),
    /// and when to look again.
    fn step(&mut self, lead: Option<(Pulse, Instant)>, now: Instant) -> (Out, Instant) {
        let lead = lead.filter(|(p, _)| BPM.contains(&p.bpm) && p.beat.is_finite());
        let Some((p, at)) = lead else {
            let out = if self.running {
                Out::Stop
            } else {
                Out::Nothing
            };
            *self = Sync::default();
            return (out, now + POLL);
        };
        let beat_s = 60.0 / p.bpm;
        let beat = p.beat + now.saturating_duration_since(at).as_secs_f64() / beat_s;
        if !self.running {
            let bar = f64::from(p.per_bar.max(1));
            let into = beat.rem_euclid(bar);
            if into > START_SLACK {
                let wait = Duration::from_secs_f64((bar - into) * beat_s);
                return (Out::Nothing, now + wait.min(POLL));
            }
            *self = Sync {
                running: true,
                pulses: 1,
                base: beat - into,
                last: Some(now),
            };
            return (Out::Start, now + Duration::from_secs_f64(beat_s / PPQN));
        }
        // Beats until the next pulse's place; a jump of the playhead moves `base` by whole beats instead of sending (or
        // holding back) a beat's worth of pulses.
        let mut ahead = self.base + self.pulses as f64 / PPQN - beat;
        if ahead.abs() > 0.5 {
            self.base -= ahead.round();
            ahead -= ahead.round();
        }
        let period = beat_s / PPQN;
        let last = self.last.unwrap_or(now);
        let [soon, late] = SPACING.map(|s| last + Duration::from_secs_f64(s * period));
        let due = (now + Duration::from_secs_f64(ahead.max(0.0) * beat_s)).clamp(soon, late);
        if due > now {
            return (Out::Nothing, due.min(now + POLL));
        }
        self.pulses += 1;
        self.last = Some(now);
        (
            Out::Clock,
            now + Duration::from_secs_f64(SPACING[0] * period).min(POLL),
        )
    }
}

/// What the UI tick shares with the clock thread.
#[derive(Default)]
struct Shared {
    lead: Mutex<Option<(Pulse, Instant)>>,
    quit: AtomicBool,
}

/// A dedicated MIDI clock output: its own connection and pulse thread. Dropping it sends STOP (when running) and ends the
/// thread.
pub struct Clock {
    port: String,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Clock {
    /// Starts the clock thread for the first output port whose name contains `port` (case-insensitive); it opens the port
    /// itself and keeps trying while it is missing.
    pub fn start(port: String) -> Clock {
        let shared = Arc::new(Shared::default());
        let (name, s) = (port.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("midi-clock".into())
            .spawn(move || run(&name, &s))
            .ok();
        Clock {
            port,
            shared,
            thread,
        }
    }

    /// The port setting this clock was started for.
    pub fn port(&self) -> &str {
        &self.port
    }

    /// The UI tick's reading of the lead deck (now), `None` when no deck plays on a grid.
    pub fn set(&self, lead: Option<Pulse>) {
        if let Ok(mut l) = self.shared.lead.lock() {
            *l = lead.map(|p| (p, Instant::now()));
        }
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl super::Midi {
    /// PREFS > CONTROLLER > MIDI CLOCK OUT changed (or at start): a clock for `port`, none when it is empty. The same port
    /// keeps its running clock.
    pub(super) fn clock_port(&self, port: &str) {
        let port = port.trim();
        if self.clock.borrow().as_ref().map_or("", Clock::port) == port {
            return;
        }
        // The old clock (if any) sends its STOP and ends before the new one opens the port.
        *self.clock.borrow_mut() = None;
        if !port.is_empty() {
            *self.clock.borrow_mut() = Some(Clock::start(port.to_owned()));
        }
    }

    /// The UI tick: hands the clock the lead deck's beat and tempo.
    pub(super) fn clock_tick(&self) {
        if let Some(c) = self.clock.borrow().as_ref() {
            let master = self.decks.iter().position(|d| d.is_master());
            c.set(crate::app::deck::lead(&self.decks, master, |d| {
                d.pulse_input()
            }));
        }
    }
}

/// The clock thread: opens `port`, steps [`Sync`] and sends what it says; a failed send drops the connection, which is
/// opened again once the port is back.
fn run(port: &str, shared: &Shared) {
    let (mut conn, mut sync): (Option<MidiOutputConnection>, Sync) = (None, Sync::default());
    let mut retry = Instant::now();
    while !shared.quit.load(Ordering::Relaxed) {
        let now = Instant::now();
        let Some(c) = conn.as_mut() else {
            if now >= retry {
                match port::connect_out(port) {
                    Ok((c, name)) => {
                        eprintln!("midi: MIDI clock on {name}");
                        conn = Some(c);
                    }
                    Err(_) => retry = now + RETRY,
                }
            }
            std::thread::sleep(POLL);
            continue;
        };
        let lead = shared.lead.lock().ok().and_then(|l| *l);
        let (out, wake) = sync.step(lead, now);
        // One message per realtime byte: some backends take a multi-byte send as one message and drop all but the first.
        if out.bytes().iter().any(|&b| c.send(&[b]).is_err()) {
            eprintln!("midi: MIDI clock port lost, retrying");
            (conn, sync, retry) = (None, Sync::default(), now + RETRY);
            continue;
        }
        std::thread::sleep(wake.saturating_duration_since(Instant::now()));
    }
    if let Some(c) = conn.as_mut().filter(|_| sync.running) {
        let _ = c.send(&[STOP]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: u16 = 4;

    fn at(bpm: f64, beat: f64, t: Instant) -> Option<(Pulse, Instant)> {
        Some((
            Pulse {
                beat,
                bpm,
                per_bar: BAR,
            },
            t,
        ))
    }

    /// Runs `sync` from `t0` for `secs` against a lead deck playing from `beat` at `bpm` (steady: the reading taken at `t0`),
    /// waking when it asks; returns what went out and when.
    fn run_for(
        sync: &mut Sync,
        lead: Option<(Pulse, Instant)>,
        t0: Instant,
        secs: f64,
    ) -> Vec<(Out, Instant)> {
        let (mut now, end, mut sent) = (t0, t0 + Duration::from_secs_f64(secs), Vec::new());
        while now < end {
            let (out, wake) = sync.step(lead, now);
            if out != Out::Nothing {
                sent.push((out, now));
            }
            assert!(wake > now, "the clock always waits a little");
            now = wake;
        }
        sent
    }

    #[test]
    fn start_waits_for_the_next_bar_line_then_pulses_24_per_beat_on_the_beat() {
        let (t0, mut s) = (Instant::now(), Sync::default());
        // 120 BPM, playhead at beat 2.5 of a 4-beat bar: the bar line is 1.5 beats = 0.75 s away.
        let sent = run_for(&mut s, at(120.0, 2.5, t0), t0, 0.75 + 2.0);
        assert_eq!(sent[0].0, Out::Start);
        let start = (sent[0].1 - t0).as_secs_f64();
        assert!((start - 0.75).abs() < 0.006, "START at {start} s");
        // 2 s at 120 BPM after START = 4 beats = 96 pulses (the START's own included), each 20.833 ms apart.
        assert!(sent[1..].iter().all(|o| o.0 == Out::Clock));
        assert_eq!(sent.len(), 96);
        for w in sent.windows(2) {
            let gap = (w[1].1 - w[0].1).as_secs_f64() * 1000.0;
            assert!((gap - 20.833).abs() < 0.01, "pulse gap {gap} ms");
        }
    }

    #[test]
    fn a_play_just_after_a_bar_line_starts_at_once() {
        let (t0, mut s) = (Instant::now(), Sync::default());
        assert_eq!(s.step(at(128.0, 8.02, t0), t0).0, Out::Start);
    }

    #[test]
    fn no_tempo_means_no_pulses_and_a_stop_when_it_was_running() {
        let (t0, mut s) = (Instant::now(), Sync::default());
        assert!(
            run_for(&mut s, at(0.0, 0.0, t0), t0, 1.0).is_empty(),
            "a deck without a grid"
        );
        assert!(run_for(&mut s, None, t0, 1.0).is_empty(), "nothing plays");
        assert_eq!(s.step(at(120.0, 0.0, t0), t0).0, Out::Start);
        let t1 = t0 + Duration::from_millis(100);
        assert_eq!(s.step(None, t1).0, Out::Stop);
        assert_eq!(s.step(None, t1 + POLL).0, Out::Nothing, "one STOP");
        assert_eq!(s.step(at(f64::NAN, 1.0, t1), t1).0, Out::Nothing);
    }

    #[test]
    fn a_nudged_lead_deck_is_followed_without_a_burst() {
        let (t0, mut s) = (Instant::now(), Sync::default());
        run_for(&mut s, at(120.0, 0.0, t0), t0, 1.0);
        let pulses = s.pulses;
        // The DJ nudges the lead deck a fifth of a beat ahead, then (later) jumps it 4 beats on.
        let t1 = t0 + Duration::from_secs(1);
        let nudged = run_for(&mut s, at(120.0, 2.2, t1), t1, 1.0);
        let t2 = t1 + Duration::from_secs(1);
        let jumped = run_for(&mut s, at(120.0, 4.2 + 4.0, t2), t2, 1.0);
        for sent in [&nudged, &jumped] {
            for w in sent.windows(2) {
                let gap = (w[1].1 - w[0].1).as_secs_f64() * 1000.0;
                assert!((15.6..=31.3).contains(&gap), "pulse gap {gap} ms");
            }
        }
        // Caught up: one second after the jump, the last pulse sits on the lead deck's beat (the jump of whole beats moved
        // `base`, it added no pulses).
        let beat_now = 6.2 + 4.0;
        let clock_beat = s.base + (s.pulses as f64 - 1.0) / PPQN;
        assert!(
            (clock_beat - beat_now).abs() < 1.5 / PPQN,
            "clock {clock_beat} vs lead {beat_now}"
        );
        assert!(
            s.pulses - pulses <= 2 * 48 + 5,
            "{} pulses",
            s.pulses - pulses
        );
    }
}
