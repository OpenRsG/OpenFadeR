//! The light timer: each tick reads the playheads (and the whole app state when it is due), hands the lights that differ from the
//! controller's to the [`super::port::Sink`], and polls the quit signals.

use super::{Midi, led};
use crate::app::MidiState;
use slint::{ComponentHandle, TimerMode};
use std::{
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// How often the lights re-read the whole Slint state (the jog ring's playheads are read on every light tick).
const STATE_EVERY: Duration = Duration::from_millis(50);

static SIGNALLED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn on_signal(_: i32) {
    SIGNALLED.store(true, Ordering::Relaxed);
}

/// SIGINT / SIGTERM end the event loop (the light timer polls the flag), so `run` returns and the
/// [`super::Guard`] drop turns the LEDs off. ponytail: libc's `signal` declared directly instead of a crate for two calls.
pub(super) fn quit_on_signals() {
    for sig in [2, 15] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe { signal(sig, on_signal) };
    }
}

impl Midi {
    /// One light tick (`JogIndicatorInterval`): the jog ring's playheads, the whole app state when it is due, then the
    /// lights that differ from the controller's go to the [`super::port::Sink`], which spaces them by `MidiOutInterval`.
    pub(super) fn update_leds(&self) {
        self.start_dump();
        let (Some(app), mut out) = (self.app.upgrade(), self.out.borrow_mut()) else {
            return;
        };
        let Some(sink) = out.as_ref() else { return };
        let pos = std::array::from_fn(|d| {
            self.decks[d]
                .with_player(|p| p.position_ms())
                .unwrap_or(0.0)
        });
        let mut snap = self.snap.borrow_mut();
        if self
            .snap_at
            .get()
            .is_none_or(|t| t.elapsed() >= STATE_EVERY)
        {
            *snap = led::snap(&app, self.vinyl.get(), self.cutter.get(), pos);
            self.snap_at.set(Some(Instant::now()));
        } else {
            for (d, p) in snap.decks.iter_mut().zip(pos) {
                d.pos_ms = p;
            }
        }
        for (k, d) in snap.decks.iter_mut().zip(self.decks.iter()) {
            k.pad_fx = d.pad_fx_held();
        }
        (snap.layer, snap.now_ms) = (self.layer.get(), self.started.elapsed().as_millis() as u64);
        self.leds.borrow_mut().update(&snap, |m| sink.send(m));
        if let Some(e) = sink.lost() {
            *out = None; // no silent retry loop: the status line says the lights stopped following
            app.global::<MidiState>()
                .set_status(tr!("LED output lost: {}", e).into());
        }
    }

    /// Exit: everything that is lit goes dark; dropping the sink waits until those messages are out.
    pub(super) fn leds_off(&self) {
        if let Some(sink) = self.out.borrow_mut().take() {
            self.leds.borrow_mut().off(|m| sink.send(m));
        }
    }

    /// (Re)starts the light timer: the jog ring's refresh period with a controller to light, else the plain state period (which
    /// still polls the quit signals).
    pub(super) fn restart_lights(self: &Rc<Self>, ring_ms: Option<u64>) {
        let tick = ring_ms.map_or(STATE_EVERY, |ms| Duration::from_millis(ms.max(1)));
        let weak = Rc::downgrade(self);
        self.lights.start(TimerMode::Repeated, tick, move || {
            if SIGNALLED.load(Ordering::Relaxed) {
                let _ = slint::quit_event_loop();
            }
            if let Some(m) = weak.upgrade() {
                m.update_leds();
            }
        });
    }
}
