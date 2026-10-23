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
/// first other shown deck that can be followed, a playing one before an idle one. `spare_audible` (SYNC, not KEY SYNC):
/// a playing `me` never follows an idle deck, except the master the MASTER button pinned (`manual`).
pub fn leader_of(
    master: Option<usize>,
    manual: bool,
    me: usize,
    shown: usize,
    decks: [(bool, bool, bool); DECKS],
    spare_audible: bool,
) -> Option<usize> {
    let audible = spare_audible && decks[me].2;
    if let Some(m) = master.filter(|&m| m != me && m < shown && (!audible || manual || decks[m].2))
    {
        return Some(m);
    }
    (0..shown)
        .filter(|&i| i != me && decks[i].0 && (!audible || decks[i].2))
        .min_by_key(|&i| (!decks[i].1, !decks[i].2))
}

impl Deck {
    /// The deck SYNC on this one follows ([`leader_of`]); `followable` says whether this deck can follow a peer's tempo.
    fn leader(&self, followable: impl Fn(&Deck) -> bool, spare_audible: bool) -> Option<Rc<Deck>> {
        let l = &self.sync.link;
        let info = std::array::from_fn(|i| {
            l.deck(i).map_or((false, false, false), |d| {
                (d.ready(), followable(&d), d.playing())
            })
        });
        leader_of(
            l.master.get(),
            l.manual.get(),
            self.index,
            active(),
            info,
            spare_audible,
        )
        .and_then(|i| l.deck(i))
    }

    /// The deck KEY SYNC matches: the master, else another shown deck holding a track (a playing one first).
    pub(in crate::app::deck) fn key_target(&self) -> Option<Rc<Deck>> {
        self.leader(|_| true, false)
    }

    /// The tempo this deck could follow `peer` at.
    fn can_follow(&self, peer: &Deck) -> bool {
        let (own, theirs) = (self.loaded.borrow(), peer.loaded.borrow());
        self.tempo_target(&own, peer, &theirs).is_some()
    }

    /// SYNC follows a master whose tempo this deck can match: only then its tempo fader is ignored (a deck without a grid, or
    /// a master out of range or still loading, leaves the fader to the DJ).
    pub(in crate::app::deck) fn tracking(&self) -> bool {
        self.following()
            && self
                .master_deck()
                .is_some_and(|p| p.ready() && self.can_follow(&p))
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
        let Some(peer) = self.leader(|d| self.can_follow(d), true) else {
            if self.playing() && self.key_target().is_some() {
                // only audible deck, the others idle: it leads, its tempo stays
                if !self.is_master() {
                    self.set_master(Some(self.index));
                }
                return self.say(tr!("sync: nothing else plays, this deck leads"));
            }
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
            leader_of(Some(3), false, 0, 4, [none, loaded, playing, playing], true),
            Some(3)
        );
        // SYNC on the master itself, or with none: a playing deck before an idle one, followable before not
        assert_eq!(
            leader_of(Some(0), false, 0, 4, [playing, loaded, none, playing], true),
            Some(3)
        );
        assert_eq!(
            leader_of(
                None,
                false,
                0,
                4,
                [none, (true, false, true), loaded, none],
                true
            ),
            Some(2)
        );
        assert_eq!(
            leader_of(None, false, 1, 4, [loaded, none, none, none], true),
            Some(0)
        );
        // nothing else loaded: nobody to follow
        assert_eq!(
            leader_of(None, false, 0, 4, [loaded, none, none, none], true),
            None
        );
        // the 2-deck layout hides C and D: they are neither master nor candidates
        assert_eq!(
            leader_of(Some(2), false, 0, 2, [loaded, none, playing, playing], true),
            None
        );
        assert_eq!(
            leader_of(None, false, 0, 2, [loaded, loaded, playing, playing], true),
            Some(1)
        );
    }

    #[test]
    fn a_playing_deck_never_follows_an_idle_one_unless_it_was_pinned_master() {
        let none = (false, false, false);
        let loaded = (true, true, false);
        let playing = (true, true, true);
        let (a, b) = ([playing, loaded, none, none], [loaded, playing, none, none]);
        // A plays alone, B idle at another tempo: whoever the (unpinned) master is, SYNC finds nothing to follow
        assert_eq!(leader_of(None, false, 0, 4, a, true), None);
        assert_eq!(leader_of(Some(0), false, 0, 4, a, true), None);
        assert_eq!(leader_of(Some(1), false, 0, 4, a, true), None);
        // KEY SYNC still matches the idle deck's key
        assert_eq!(leader_of(None, false, 0, 4, a, false), Some(1));
        // MASTER pressed on idle B: that is the explicit override, followed idle
        assert_eq!(leader_of(Some(1), true, 0, 4, a, true), Some(1));
        // an idle deck still follows a playing one, and a playing master stays followed
        assert_eq!(leader_of(None, false, 0, 4, b, true), Some(1));
        assert_eq!(leader_of(Some(1), false, 0, 4, b, true), Some(1));
    }

    struct NoTracks;
    impl openfader::library::Library for NoTracks {
        fn kind(&self) -> &str {
            "none"
        }
        fn playlists(&self) -> Result<Vec<openfader::library::Playlist>, String> {
            Ok(Vec::new())
        }
        fn tracks(&self, _: Option<&str>) -> Result<Vec<openfader::library::Track>, String> {
            Ok(Vec::new())
        }
    }

    /// A loaded deck's grid at `bpm_x100` (a beat every 500 ms to keep the maths simple).
    fn hold(d: &Deck, bpm_x100: u16) {
        d.view.borrow_mut().loaded = true;
        d.loaded.borrow_mut().analysis.beats = (0..64)
            .map(|i| openfader::analysis::Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100,
                time_ms: i as u32 * 500,
            })
            .collect();
    }

    #[test]
    fn the_tempo_fader_is_ignored_only_while_the_master_can_be_followed() {
        crate::app::headless::init();
        let app = crate::app::App::new().unwrap();
        let model = Rc::new(slint::VecModel::from(vec![
            crate::app::DeckView::default();
            DECKS
        ]));
        let scroll = Rc::new(slint::VecModel::from(vec![
            crate::app::DeckScroll::default();
            DECKS
        ]));
        let link = Rc::new(crate::app::deck::sync::Link::default());
        let decks: [Rc<Deck>; DECKS] = std::array::from_fn(|i| {
            Deck::new(
                &app,
                i,
                model.clone(),
                scroll.clone(),
                link.clone(),
                Rc::new(NoTracks),
            )
        });
        crate::app::deck::sync::connect(&decks);
        let (master, b) = (&decks[0], &decks[1]);
        hold(master, 12000);
        master.set_master(Some(0));
        b.sync.synced.set(true);
        // a track without a grid cannot follow anything: the fader is the DJ's
        hold(b, 0);
        assert!(b.following() && !b.tracking());
        b.tempo(1.0);
        assert!(
            b.rate() > 1.0,
            "the fader of a gridless synced deck is dead"
        );
        // a master at 20 BPM against 120 is outside 0.5 to 2× (also doubled): the same
        hold(b, 12000);
        hold(master, 2000);
        assert!(!b.tracking());
        b.tempo(0.0);
        assert!(
            b.rate() < 1.0,
            "the fader is dead while the master is out of range"
        );
        // a master it can follow takes the fader over
        hold(master, 12000);
        assert!(b.tracking());
        let rate = b.rate();
        b.tempo(1.0);
        assert_eq!(b.rate(), rate);
    }
}
