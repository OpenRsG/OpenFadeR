//! The per-tick follower: tempo, the armed quantized start and phase correction.

use super::phase::loop_preserves_phase;
use super::{Fix, align, correct, error_ms, start_ms, wait_phase};
use crate::app::deck::Deck;

impl Deck {
    /// PLAY on a paused deck: when it follows a playing master, arms a start on the master's next beat.
    pub(in crate::app::deck) fn play_quantized(&self) -> bool {
        if !self.sync.beat.get() {
            return false; // BPM SYNC: no quantized start
        }
        let Some(peer) = self.master_deck().filter(|_| self.following()) else {
            return false;
        };
        let (own, theirs) = (self.loaded.borrow(), peer.loaded.borrow());
        let (their_pos, their_playing) = peer.clock(&theirs);
        let phase = self
            .tempo_target(&own.analysis.beats, &theirs.analysis.beats, peer.rate.get())
            .filter(|(_, _, k)| loop_preserves_phase(&theirs.analysis.beats, theirs.loop_, *k))
            .and_then(|(_, _, k)| wait_phase(&theirs.analysis.beats, their_pos, k));
        drop((own, theirs));
        match phase.filter(|_| their_playing) {
            Some(p) => {
                self.sync.waiting.set(Some(p));
                self.say(tr!("synced: starting on the master's next beat…"));
                true
            }
            None => false,
        }
    }

    /// The master cannot be followed now (gone, still loading, no grid, tempo out of range): no nudge stays and an armed
    /// start just starts, the DJ pressed PLAY.
    fn stop_following(&self) {
        if self.sync.waiting.take().is_some() && self.ready() {
            self.with_player(|p| p.play());
        }
        self.set_nudge(0.0);
    }

    /// Once per tick: master hand-over, tempo following, the armed start, phase correction.
    pub(in crate::app::deck) fn follow(&self) {
        self.quantize_tick();
        let playing = self.playing();
        if self.sync.was_playing.replace(playing) != playing {
            self.play_changed(playing);
            if !playing {
                self.track_ended();
            }
        }
        let Some(peer) = self
            .master_deck()
            .filter(|p| self.following() && self.ready() && p.ready())
        else {
            return self.stop_following();
        };
        let (own, theirs) = (self.loaded.borrow(), peer.loaded.borrow());
        let (own_pos, _) = self.clock(&own);
        let (their_pos, their_playing) = peer.clock(&theirs);
        let (ob, tb) = (&own.analysis.beats[..], &theirs.analysis.beats[..]);
        let Some((g, rate, k)) = self.tempo_target(ob, tb, peer.rate.get()) else {
            drop((own, theirs));
            return self.stop_following();
        };
        let master_phase = loop_preserves_phase(tb, theirs.loop_, k);
        let free = master_phase
            && !self.sync.scratching.get()
            && self.sync.bend.get() == 0.0
            && own.loop_.is_none()
            && !self.view.borrow().reverse
            && !peer.sync.scratching.get()
            && !peer.view.borrow().reverse;
        // Only the figures this tick's branch uses: each is a binary search per grid.
        let waiting = self.sync.waiting.get();
        let (start, phase) = if waiting.is_some() && master_phase {
            // The start is heard one output latency after it is made: the master has moved on by then, and the own track with it.
            let ahead = self.with_player(|p| p.output_latency_ms()).unwrap_or(0.0) * rate;
            (
                start_ms(ob, own_pos, tb, their_pos, k, ahead),
                wait_phase(tb, their_pos, k),
            )
        } else {
            (None, None)
        };
        // A seek or start not yet heard holds the reported position at its target: measured then, the phase
        // would be off by the output latency, and a correction made on it would land off by as much.
        let aligned = (waiting.is_none()
            && playing
            && their_playing
            && free
            && self.steady()
            && peer.steady()
            && self.settled()
            && peer.settled())
        .then(|| align(ob, own_pos, tb, their_pos, k))
        .flatten();
        drop((own, theirs));
        if (rate - self.rate.get()).abs() > 1e-9 {
            self.set_rate(rate);
        }
        if !self.sync.beat.get() {
            self.sync.waiting.set(None); // BPM SYNC: the tempo follows, the beats are the DJ's
            return self.set_nudge(0.0);
        }
        if let Some(prev) = waiting {
            match phase.zip(start).filter(|_| their_playing) {
                Some((cur, _)) if cur >= prev => self.sync.waiting.set(Some(cur)),
                Some((_, ms)) => {
                    self.sync.waiting.set(None);
                    self.with_player(|p| {
                        p.seek(ms);
                        p.play();
                    });
                }
                None => {
                    self.sync.waiting.set(None); // the master stopped: just start
                    self.with_player(|p| p.play());
                }
            }
            return;
        }
        let Some((err, ms)) = aligned else {
            return self.set_nudge(0.0);
        };
        match correct(error_ms(err, g * rate), self.sync.nudge.get() != 0.0) {
            Fix::Seek => {
                // In phase: the jump is heard one buffer and one output latency later, as far past `ms` as the deck plays meanwhile.
                self.with_player(|p| p.seek_in_phase(ms.max(0.0).round() as u32, own_pos));
                self.set_nudge(0.0);
            }
            Fix::Nudge(d) => self.set_nudge(d),
        }
    }
}
