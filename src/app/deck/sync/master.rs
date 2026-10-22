//! Who is MASTER and who a deck follows: the hand-over rules and the SYNC / MASTER buttons.

use crate::app::{
    DeckState,
    deck::{DECK_NAMES, DECKS, Deck, active},
};
use slint::ComponentHandle;
use std::rc::Rc;

/// Master after deck `me` started (`playing`) or stopped. `on`: which decks play now (`me` included).
/// A start takes the master when there is none, or the master is idle, unpinned and `me` is no
/// follower; a stopped master hands over to the first other playing deck unless pinned.
pub fn after_play_change(
    master: Option<usize>,
    manual: bool,
    me: usize,
    playing: bool,
    synced: bool,
    on: [bool; DECKS],
) -> Option<usize> {
    match (playing, master) {
        (true, None) => Some(me),
        (true, Some(m)) if m != me && !manual && !on[m] && !synced => Some(me),
        (false, Some(m)) if m == me && !manual => (0..DECKS).find(|&i| i != me && on[i]).or(master),
        _ => master,
    }
}

/// What SYNC on deck `me` follows. `decks`: per deck (holds a track, tempo can be followed, plays). The master
/// unless it is `me` (or nobody, or hidden by a 2-deck layout: only the first `shown` decks count); else the
/// first other shown deck that can be followed, a playing one before an idle one.
pub fn leader_of(
    master: Option<usize>,
    me: usize,
    shown: usize,
    decks: [(bool, bool, bool); DECKS],
) -> Option<usize> {
    if let Some(m) = master.filter(|&m| m != me && m < shown) {
        return Some(m);
    }
    (0..shown)
        .filter(|&i| i != me && decks[i].0)
        .min_by_key(|&i| (!decks[i].1, !decks[i].2))
}

impl Deck {
    /// The deck SYNC on this one follows ([`leader_of`]); `followable` says whether this deck can follow a peer's tempo.
    fn leader(&self, followable: impl Fn(&Deck) -> bool) -> Option<Rc<Deck>> {
        let l = &self.sync.link;
        let info = std::array::from_fn(|i| {
            l.deck(i).map_or((false, false, false), |d| {
                (d.ready(), followable(&d), d.playing())
            })
        });
        leader_of(l.master.get(), self.index, active(), info).and_then(|i| l.deck(i))
    }

    /// The deck KEY SYNC matches: the master, else another shown deck holding a track (a playing one first).
    pub(in crate::app::deck) fn key_target(&self) -> Option<Rc<Deck>> {
        self.leader(|_| true)
    }

    /// The tempo this deck could follow `peer` at.
    fn can_follow(&self, peer: &Deck) -> bool {
        let (own, theirs) = (self.loaded.borrow(), peer.loaded.borrow());
        self.tempo_target(&own.analysis.beats, &theirs.analysis.beats, peer.rate.get())
            .is_some()
    }

    /// SYNC button: latch on (tempo and beats follow the master) or off.
    pub fn sync_toggle(&self) {
        if !self.ready() {
            return;
        }
        if self.sync.synced.replace(false) {
            self.sync.waiting.set(None);
            self.set_nudge(0.0);
            self.show(|v| v.synced = false);
            return self.say(tr!("sync off: the tempo fader is yours again"));
        }
        let Some(peer) = self.leader(|d| self.can_follow(d)) else {
            return self.say(tr!("sync: load a track on another deck first"));
        };
        if !peer.ready() || !self.can_follow(&peer) {
            return self.say(tr!(
                "sync: both decks need a beat grid and a tempo within 0.5–2×"
            ));
        }
        if self.sync.link.master.get() != Some(peer.index) {
            self.set_master(Some(peer.index)); // SYNC on the master (or with none): another deck leads
        }
        self.sync.synced.set(true);
        self.show(|v| v.synced = true);
        self.say(tr!(
            "synced to deck {} (tempo and beats follow it)",
            DECK_NAMES[peer.index]
        ));
        self.follow();
    }

    /// MASTER button: makes this deck the pinned master; on the master it toggles pinned / automatic.
    pub fn master_toggle(&self) {
        let l = &self.sync.link;
        if self.is_master() {
            l.manual.set(!l.manual.get());
            self.say(if l.manual.get() {
                tr!("master: pinned here")
            } else {
                tr!("master: automatic (first playing deck)")
            });
        } else {
            self.set_master(Some(self.index));
            l.manual.set(true);
            self.say(tr!("master: this deck, pinned"));
        }
    }

    /// Makes `m` the master (unpinned) in all decks' views and in `DeckState.master`, which the
    /// browser, FX, sampler and AUTOMIX read as the reference deck.
    pub(super) fn set_master(&self, m: Option<usize>) {
        let l = &self.sync.link;
        if l.master.get() != m {
            openfader::plugin::event::fire(
                "master_changed",
                serde_json::json!({"deck": m.map_or(0, |m| m + 1)}),
            );
        }
        l.master.set(m);
        l.manual.set(false);
        self.apply_master(m);
        for d in (0..DECKS)
            .filter(|&i| i != self.index)
            .filter_map(|i| l.deck(i))
        {
            d.apply_master(m);
        }
        if let Some(app) = self.app.upgrade() {
            app.global::<DeckState>()
                .set_master(m.map_or(-1, |m| m as i32));
        }
    }

    fn apply_master(&self, m: Option<usize>) {
        let me = m == Some(self.index);
        if me {
            self.sync.synced.set(false); // the master follows nobody
            self.sync.waiting.set(None);
            self.set_nudge(0.0);
        }
        let synced = self.sync.synced.get();
        self.show(|v| {
            v.master = me;
            v.synced = synced;
        });
    }

    /// Master hand-over after this deck started or stopped playing.
    pub(super) fn play_changed(&self, playing: bool) {
        let l = &self.sync.link;
        let on = std::array::from_fn(|i| {
            if i == self.index {
                playing
            } else {
                l.deck(i).is_some_and(|d| d.playing())
            }
        });
        let m = after_play_change(
            l.master.get(),
            l.manual.get(),
            self.index,
            playing,
            self.is_synced(),
            on,
        );
        if m != l.master.get() {
            self.set_master(m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decks playing now, by letter ("BD" = B and D).
    fn on(playing: &str) -> [bool; DECKS] {
        std::array::from_fn(|i| playing.contains(DECK_NAMES[i]))
    }

    #[test]
    fn auto_master_is_the_first_playing_deck_and_hands_over() {
        // nothing is master: whoever starts first
        assert_eq!(
            after_play_change(None, false, 1, true, false, on("B")),
            Some(1)
        );
        // A leads and plays; B starting stays a follower of A
        assert_eq!(
            after_play_change(Some(0), false, 1, true, false, on("AB")),
            Some(0)
        );
        // A leads but is idle: a free B taking over; a synced B does not
        assert_eq!(
            after_play_change(Some(0), false, 1, true, false, on("B")),
            Some(1)
        );
        assert_eq!(
            after_play_change(Some(0), false, 1, true, true, on("B")),
            Some(0)
        );
        // pinned stays
        assert_eq!(
            after_play_change(Some(0), true, 1, true, false, on("B")),
            Some(0)
        );
        // the master stops: the playing deck takes over, unless pinned or nobody plays
        assert_eq!(
            after_play_change(Some(0), false, 0, false, false, on("B")),
            Some(1)
        );
        assert_eq!(
            after_play_change(Some(0), true, 0, false, false, on("B")),
            Some(0)
        );
        assert_eq!(
            after_play_change(Some(0), false, 0, false, false, on("")),
            Some(0)
        );
        // a follower stopping changes nothing
        assert_eq!(
            after_play_change(Some(0), false, 1, false, true, on("A")),
            Some(0)
        );
    }

    #[test]
    fn decks_c_and_d_take_and_hand_over_the_master() {
        // D starts first and leads; C starting later follows
        assert_eq!(
            after_play_change(None, false, 3, true, false, on("D")),
            Some(3)
        );
        assert_eq!(
            after_play_change(Some(3), false, 2, true, false, on("CD")),
            Some(3)
        );
        // the master D stops: the first other playing deck (B before C) leads
        assert_eq!(
            after_play_change(Some(3), false, 3, false, false, on("BC")),
            Some(1)
        );
        assert_eq!(
            after_play_change(Some(3), false, 3, false, false, on("C")),
            Some(2)
        );
        // an idle master C hands over to a free A
        assert_eq!(
            after_play_change(Some(2), false, 0, true, false, on("A")),
            Some(0)
        );
        // a playing A leaves C's lead alone while the master C plays
        assert_eq!(
            after_play_change(Some(2), false, 0, true, false, on("AC")),
            Some(2)
        );
    }

    #[test]
    fn sync_follows_the_master_else_the_best_other_deck() {
        // (holds a track, can be followed, plays) per deck A..D
        let none = (false, false, false);
        let loaded = (true, true, false);
        let playing = (true, true, true);
        // the master wins, whichever deck it is
        assert_eq!(
            leader_of(Some(3), 0, 4, [none, loaded, playing, playing]),
            Some(3)
        );
        // SYNC on the master itself, or with none: a playing deck before an idle one, followable before not
        assert_eq!(
            leader_of(Some(0), 0, 4, [playing, loaded, none, playing]),
            Some(3)
        );
        assert_eq!(
            leader_of(None, 0, 4, [none, (true, false, true), loaded, none]),
            Some(2)
        );
        assert_eq!(leader_of(None, 1, 4, [loaded, none, none, none]), Some(0));
        // nothing else loaded: nobody to follow
        assert_eq!(leader_of(None, 0, 4, [loaded, none, none, none]), None);
        // the 2-deck layout hides C and D: they are neither master nor candidates
        assert_eq!(
            leader_of(Some(2), 0, 2, [loaded, none, playing, playing]),
            None
        );
        assert_eq!(
            leader_of(None, 0, 2, [loaded, loaded, playing, playing]),
            Some(1)
        );
    }
}
